//! Source backends.
//!
//! A backend turns a [`locator::Locator`] into a directory on local disk that
//! `skill` can scan, hash, and deploy from. Backends are deliberately narrow:
//! they fetch bytes and nothing else. **No backend ever executes anything it
//! fetched**, including install scripts, hooks, or build steps. Retrieved content
//! is data.
//!
//! Each backend reports its [`Capabilities`] so callers can tell a transport that
//! supports revision checks from one that does not, rather than inferring it.

pub mod fs;
pub mod locator;

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::pkg::identity::SourceType;
use crate::safepath::Limits;

/// What a backend can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Capabilities {
    /// Can report whether upstream has a newer revision without a full fetch.
    pub revision_check: bool,
    /// Has a concept of an immutable revision that can be pinned.
    pub pinnable: bool,
    /// Uses credentials from the environment or the OS.
    pub authenticated: bool,
    /// Requires the network.
    pub network: bool,
}

/// An acquired package, staged on local disk.
#[derive(Debug)]
pub struct Acquired {
    /// Directory holding the fetched content.
    ///
    /// For a filesystem source this is the original location, which is read-only
    /// to us. For a remote source it is a staging directory owned by `_staging`.
    pub root: PathBuf,
    /// The immutable revision, when the transport has one.
    pub resolved_revision: Option<String>,
    /// Advisory messages, such as disclosed network activity.
    pub notes: Vec<String>,
    /// Keeps a temporary staging directory alive for as long as `root` is used,
    /// and removes it afterwards. `None` for a source we did not copy.
    pub _staging: Option<tempfile::TempDir>,
}

impl Acquired {
    /// An acquisition that reads the source in place.
    pub fn in_place(root: PathBuf) -> Self {
        Self {
            root,
            resolved_revision: None,
            notes: Vec::new(),
            _staging: None,
        }
    }
}

/// What upstream currently looks like, for `update --check`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionCheck {
    /// The revision upstream reports now.
    pub revision: Option<String>,
    /// True when it differs from the revision we recorded.
    pub changed: bool,
    /// How the answer was obtained, so a reader can judge how much to trust it.
    pub evidence: String,
}

/// A transport that can produce a package directory.
pub trait Backend: std::fmt::Debug {
    /// Which source type this backend serves.
    fn source_type(&self) -> SourceType;

    fn capabilities(&self) -> Capabilities;

    /// Fetch the source into a directory we can read.
    fn acquire(&self, locator: &locator::Locator, limits: &Limits) -> Result<Acquired>;

    /// Ask upstream whether it has moved, without fetching content.
    ///
    /// The default refuses rather than returning "unchanged", because reporting
    /// "up to date" without having checked is exactly the false assurance the
    /// product promise forbids.
    fn check_revision(
        &self,
        _locator: &locator::Locator,
        _recorded: Option<&str>,
    ) -> Result<RevisionCheck> {
        Err(crate::Error::Unsupported {
            agent: self.source_type().slug().to_string(),
            what: "check for a newer upstream revision".to_string(),
            reason: "this transport exposes no revision to compare".to_string(),
            hint: "re-acquire the source to compare its content instead".to_string(),
        })
    }
}

/// Pick the backend for a locator.
///
/// A transport whose feature is not compiled in fails with the feature name
/// rather than a generic error, so the fix is obvious.
pub fn backend_for(locator: &locator::Locator) -> Result<Box<dyn Backend>> {
    match locator.source_type {
        SourceType::Filesystem => Ok(Box::new(fs::FilesystemBackend)),
        SourceType::Git => Err(not_yet("git", "git")),
        SourceType::Http => Err(not_yet("http", "http")),
        SourceType::Smb => {
            #[cfg(feature = "smb")]
            {
                Err(not_yet("smb", "smb"))
            }
            #[cfg(not(feature = "smb"))]
            {
                Err(crate::Error::BackendUnavailable {
                    transport: "SMB".to_string(),
                    feature: "smb".to_string(),
                })
            }
        }
        SourceType::Bundle | SourceType::Adopted => Err(crate::Error::UnsupportedSource {
            locator: locator.sanitized.clone(),
            reason: "this source type is restored by `skill import`, not acquired directly".into(),
        }),
    }
}

/// A transport that is designed and specified but not implemented in this build.
///
/// Reported as an explicit failure rather than a silent no-op, so a caller is
/// never told an operation succeeded when no transport ran.
fn not_yet(transport: &str, _feature: &str) -> crate::Error {
    crate::Error::BackendUnavailable {
        transport: transport.to_string(),
        feature: format!(
            "{transport} (not implemented in this build; see docs/checklist.md for status)"
        ),
    }
}

/// Copy a tree into a staging directory, applying every containment rule.
///
/// Used by the remote backends and by `copy`, so one hardened copy path serves
/// them all rather than each re-implementing the checks.
pub fn copy_tree(from: &Path, to: &Path, limits: &Limits) -> Result<()> {
    use crate::error::IoContext;
    use crate::safepath::{self, EntryKind};

    let tree = crate::pkg::tree::build(from, limits)?;
    std::fs::create_dir_all(to).ctx("creating the staging directory", to)?;
    let real_to = std::fs::canonicalize(to).ctx("resolving the staging directory", to)?;

    for entry in &tree.entries {
        let relative = safepath::sanitize_relative(&entry.path, limits)?;
        let destination = relative.join_onto(&real_to);

        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).ctx("creating a package directory", parent)?;
        }
        // Re-check containment immediately before each write, not only once at
        // the start: a parent could have been swapped in between.
        safepath::verify_containment(&real_to, &destination)?;

        let origin = relative.join_onto(from);
        match entry.kind {
            EntryKind::Dir => {
                std::fs::create_dir_all(&destination)
                    .ctx("creating a package directory", &destination)?;
            }
            EntryKind::File => {
                std::fs::copy(&origin, &destination).ctx("copying a package file", &origin)?;
                safepath::apply_exec_bit(&destination, entry.mode)?;
            }
            EntryKind::InternalSymlink => {
                // Validated as package-internal when the tree was built.
                let target = entry.link_target.as_deref().unwrap_or_default();
                #[cfg(unix)]
                std::os::unix::fs::symlink(target, &destination)
                    .ctx("recreating a package symlink", &destination)?;
                #[cfg(windows)]
                {
                    // Windows needs to know whether the target is a directory.
                    let resolved = destination
                        .parent()
                        .map(|p| p.join(target))
                        .unwrap_or_default();
                    if resolved.is_dir() {
                        std::os::windows::fs::symlink_dir(target, &destination)
                            .ctx("recreating a package symlink", &destination)?;
                    } else {
                        std::os::windows::fs::symlink_file(target, &destination)
                            .ctx("recreating a package symlink", &destination)?;
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filesystem_locator_gets_the_filesystem_backend() {
        let loc = locator::parse("./x").unwrap();
        let backend = backend_for(&loc).unwrap();
        assert_eq!(backend.source_type(), SourceType::Filesystem);
        assert!(!backend.capabilities().network);
    }

    #[test]
    fn an_unimplemented_transport_fails_loudly_rather_than_silently() {
        // The product promise forbids reporting success for work that did not
        // happen, so this must be an error, never a no-op.
        let loc = locator::parse("https://github.com/o/r.git").unwrap();
        let err = backend_for(&loc).unwrap_err();
        assert!(
            matches!(err, crate::Error::BackendUnavailable { .. }),
            "{err:?}"
        );
        assert_eq!(err.exit_code(), crate::ExitCode::Source);
    }

    #[test]
    fn check_revision_refuses_rather_than_claiming_up_to_date() {
        // Returning "unchanged" without checking would be a false assurance.
        #[derive(Debug)]
        struct Dumb;
        impl Backend for Dumb {
            fn source_type(&self) -> SourceType {
                SourceType::Filesystem
            }
            fn capabilities(&self) -> Capabilities {
                Capabilities {
                    revision_check: false,
                    pinnable: false,
                    authenticated: false,
                    network: false,
                }
            }
            fn acquire(&self, _: &locator::Locator, _: &Limits) -> Result<Acquired> {
                unreachable!()
            }
        }

        let loc = locator::parse("./x").unwrap();
        let err = Dumb.check_revision(&loc, None).unwrap_err();
        assert!(matches!(err, crate::Error::Unsupported { .. }), "{err:?}");
    }

    #[test]
    fn copy_tree_reproduces_content_and_the_executable_bit() {
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("scripts")).unwrap();
        std::fs::write(
            src.path().join("SKILL.md"),
            "---\nname: t\ndescription: d\n---\n",
        )
        .unwrap();
        std::fs::write(src.path().join("scripts/run.sh"), "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                src.path().join("scripts/run.sh"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }

        let dst = tempfile::tempdir().unwrap();
        copy_tree(src.path(), dst.path(), &Limits::default()).unwrap();

        let before = crate::pkg::tree::build(src.path(), &Limits::default()).unwrap();
        let after = crate::pkg::tree::build(dst.path(), &Limits::default()).unwrap();
        assert_eq!(
            before.digest, after.digest,
            "a copy must be digest-identical to its source"
        );
        #[cfg(unix)]
        assert!(after.get("scripts/run.sh").unwrap().is_executable());
    }

    #[test]
    fn copy_tree_refuses_a_package_that_escapes_itself() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(
            src.path().join("SKILL.md"),
            "---\nname: t\ndescription: d\n---\n",
        )
        .unwrap();
        std::fs::write(src.path().join("outside.txt"), "x").unwrap();
        let pkg = src.path().join("pkg");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("SKILL.md"), "---\nname: t\ndescription: d\n---\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("../outside.txt", pkg.join("leak.txt")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(src.path().join("outside.txt"), pkg.join("leak.txt"))
            .unwrap();

        let dst = tempfile::tempdir().unwrap();
        assert!(copy_tree(&pkg, dst.path(), &Limits::default()).is_err());
    }
}
