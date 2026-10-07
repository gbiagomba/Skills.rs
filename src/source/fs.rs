//! Filesystem backend: local paths, `file://` URIs, mounted shares, UNC paths.
//!
//! The source is read in place and never modified, which is what `copy` and
//! `link` both promise about the path they were handed.
//!
//! A mounted SMB or NFS share is handled here because, once mounted, it *is* a
//! local path. That is support for mounted shares, and it is deliberately not
//! called native SMB support: the kernel is doing the protocol work, the
//! credentials came from whoever mounted it, and none of the SMB-specific
//! behaviour applies. Native `smb://` acquisition is a separate backend.

use std::path::PathBuf;

use super::{locator, Acquired, Backend, Capabilities};
use crate::error::{Error, Result};
use crate::pkg::identity::SourceType;
use crate::safepath::Limits;

/// Reads packages from the local filesystem.
#[derive(Debug, Default, Clone, Copy)]
pub struct FilesystemBackend;

impl Backend for FilesystemBackend {
    fn source_type(&self) -> SourceType {
        SourceType::Filesystem
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // A directory has no revision. `update` compares content instead,
            // which is why this is false rather than quietly pretending.
            revision_check: false,
            pinnable: false,
            authenticated: false,
            network: false,
        }
    }

    fn acquire(&self, locator: &locator::Locator, _limits: &Limits) -> Result<Acquired> {
        let locator::Target::Path(path) = &locator.target else {
            return Err(Error::Internal(
                "the filesystem backend was given a non-path target".into(),
            ));
        };

        let metadata = std::fs::symlink_metadata(path).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                Error::SourceUnavailable {
                    locator: path.display().to_string(),
                    reason: "no such file or directory".into(),
                }
            } else {
                Error::io("inspecting the source", path, err)
            }
        })?;

        // Resolve a symlinked source once, up front, so everything downstream
        // works with a real path.
        let root: PathBuf = if metadata.file_type().is_symlink() {
            std::fs::canonicalize(path)
                .map_err(|err| Error::io("resolving the source", path, err))?
        } else {
            path.clone()
        };

        let mut notes = Vec::new();
        if is_likely_network_mount(&root) {
            notes.push(format!(
                "{} looks like a mounted network share. This is mounted-path support, not native \
                 SMB: the mount's own credentials and caching apply, and the share must stay \
                 mounted for a re-read to work.",
                root.display()
            ));
        }

        Ok(Acquired {
            root,
            resolved_revision: None,
            notes,
            _staging: None,
        })
    }
}

/// Heuristic test for a mounted network share.
///
/// Deliberately a heuristic and labelled as one: there is no portable way to ask
/// "is this a network filesystem", so the note it produces is advisory and never
/// changes behaviour.
fn is_likely_network_mount(path: &std::path::Path) -> bool {
    let text = path.to_string_lossy();
    if cfg!(target_os = "macos") {
        text.starts_with("/Volumes/")
    } else if cfg!(windows) {
        text.starts_with(r"\\")
    } else {
        text.starts_with("/mnt/") || text.starts_with("/media/") || text.starts_with("/net/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquires_a_local_directory_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("SKILL.md"),
            "---\nname: t\ndescription: d\n---\n",
        )
        .unwrap();

        let loc = locator::parse(&tmp.path().to_string_lossy()).unwrap();
        let acquired = FilesystemBackend.acquire(&loc, &Limits::default()).unwrap();

        assert_eq!(acquired.root, tmp.path());
        assert!(
            acquired._staging.is_none(),
            "a local source is read in place, not copied"
        );
        assert_eq!(acquired.resolved_revision, None);
    }

    #[test]
    fn leaves_the_source_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let skill = tmp.path().join("SKILL.md");
        std::fs::write(&skill, "---\nname: t\ndescription: d\n---\n").unwrap();
        let before = crate::pkg::tree::build(tmp.path(), &Limits::default()).unwrap();

        let loc = locator::parse(&tmp.path().to_string_lossy()).unwrap();
        FilesystemBackend.acquire(&loc, &Limits::default()).unwrap();

        let after = crate::pkg::tree::build(tmp.path(), &Limits::default()).unwrap();
        assert_eq!(before.digest, after.digest);
    }

    #[test]
    fn a_missing_source_is_reported_as_unavailable() {
        let loc = locator::parse("/definitely/not/here/skill").unwrap();
        let err = FilesystemBackend
            .acquire(&loc, &Limits::default())
            .unwrap_err();
        assert!(matches!(err, Error::SourceUnavailable { .. }), "{err:?}");
        assert_eq!(err.exit_code(), crate::ExitCode::Source);
    }

    #[test]
    fn resolves_a_symlinked_source() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("SKILL.md"), "---\nname: t\ndescription: d\n---\n").unwrap();
        let link = tmp.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&real, &link).unwrap();

        let loc = locator::parse(&link.to_string_lossy()).unwrap();
        let acquired = FilesystemBackend.acquire(&loc, &Limits::default()).unwrap();
        assert_eq!(
            acquired.root.canonicalize().unwrap(),
            real.canonicalize().unwrap()
        );
    }

    #[test]
    fn declares_that_it_cannot_check_a_revision() {
        // `update` must know to compare content rather than ask for a revision.
        assert!(!FilesystemBackend.capabilities().revision_check);
        assert!(!FilesystemBackend.capabilities().pinnable);

        let loc = locator::parse("./x").unwrap();
        assert!(FilesystemBackend.check_revision(&loc, None).is_err());
    }

    #[test]
    fn mounted_share_support_is_labelled_as_such() {
        // The note must not claim native SMB support.
        let path = if cfg!(target_os = "macos") {
            "/Volumes/share/skill"
        } else if cfg!(windows) {
            r"\\server\share\skill"
        } else {
            "/mnt/share/skill"
        };
        assert!(is_likely_network_mount(std::path::Path::new(path)));
        assert!(!is_likely_network_mount(std::path::Path::new(
            "/home/me/skill"
        )));
    }
}
