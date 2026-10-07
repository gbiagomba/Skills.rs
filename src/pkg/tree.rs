//! Package trees and their deterministic digests.
//!
//! A [`PackageTree`] is the complete, ordered inventory of a skill package: every
//! file, its relative path, kind, executable bit, size, and content digest. The
//! tree digest is computed over a canonical rendering of that inventory, so it
//! changes when a file's content changes, when a file is added or deleted, and
//! when a file gains or loses its executable bit.
//!
//! The hashing and the inventory live together on purpose. A digest over anything
//! less than the whole tree would silently miss the cases reconciliation exists to
//! catch: a deleted `references/` file, or an edit to a script rather than to
//! `SKILL.md`.
//!
//! Modification times are recorded for auditing only and are deliberately *not*
//! part of the digest. An mtime is not evidence of content, and treating it as
//! such is how tools end up overwriting edits.

use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{Error, IoContext, Result};
use crate::safepath::{self, EntryKind, Limits, SafeRelPath};

/// One entry in a package tree.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileEntry {
    /// Path relative to the package root, always with `/` separators.
    pub path: String,
    pub kind: EntryKind,
    /// Unix mode bits, absent on platforms without them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    /// Content length in bytes. Zero for directories and links.
    pub size: u64,
    /// Lowercase hex SHA-256 of the file content. For a link, the digest of its
    /// target string. Empty for a directory.
    pub digest: String,
    /// The raw link target, when this entry is a package-internal symlink.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_target: Option<String>,
}

impl FileEntry {
    /// True when the recorded mode has any executable bit set.
    pub fn is_executable(&self) -> bool {
        self.mode.map(|m| m & 0o111 != 0).unwrap_or(false)
    }
}

/// The complete inventory of a package, plus its digest.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PackageTree {
    /// Entries sorted by path, so the ordering is reproducible across platforms.
    pub entries: Vec<FileEntry>,
    /// Lowercase hex SHA-256 over the canonical manifest.
    pub digest: String,
    /// Total content bytes, for reporting.
    pub total_bytes: u64,
}

impl PackageTree {
    /// Look up one entry by its relative path.
    pub fn get(&self, path: &str) -> Option<&FileEntry> {
        self.entries
            .binary_search_by(|e| e.path.as_str().cmp(path))
            .ok()
            .map(|i| &self.entries[i])
    }

    /// True when the package contains a `SKILL.md` at its root.
    pub fn has_skill_md(&self) -> bool {
        self.get("SKILL.md").is_some()
    }

    /// Every path in the tree, in order.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.path.as_str())
    }

    /// Paths present here but not in `other`.
    pub fn paths_missing_from(&self, other: &PackageTree) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|e| other.get(&e.path).is_none())
            .map(|e| e.path.as_str())
            .collect()
    }

    /// Paths whose content or executable bit differs between the two trees.
    pub fn changed_against(&self, other: &PackageTree) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|e| match other.get(&e.path) {
                None => false,
                Some(o) => {
                    e.digest != o.digest
                        || e.is_executable() != o.is_executable()
                        || e.kind != o.kind
                }
            })
            .map(|e| e.path.as_str())
            .collect()
    }
}

/// Hash raw bytes, returning lowercase hex.
pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Hash a file's content by streaming it, so a large asset is not buffered whole.
fn hash_file(path: &Path, limits: &Limits, name: &str) -> Result<(String, u64)> {
    use std::io::Read;

    let file = fs::File::open(path).ctx("opening file to hash", path)?;
    let declared = file.metadata().ctx("reading file size", path)?.len();
    limits.check_file(name, declared)?;

    let mut reader = std::io::BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;

    loop {
        let read = reader.read(&mut buffer).ctx("reading file to hash", path)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        // Re-check as we go: the file could be growing underneath us.
        limits.check_file(name, total)?;
        hasher.update(&buffer[..read]);
    }

    Ok((hex::encode(hasher.finalize()), total))
}

/// The canonical manifest a tree digest is computed over.
///
/// One line per entry, tab separated, sorted by path:
///
/// ```text
/// <kind>\t<exec>\t<path>\t<digest>\n
/// ```
///
/// Only the *executable* bit is included rather than the full mode, matching what
/// is actually reproduced on deployment. Including the whole mode would make the
/// digest depend on the authoring machine's umask, so two byte-identical packages
/// would compare as different.
fn canonical_manifest(entries: &[FileEntry]) -> String {
    let mut out = String::with_capacity(entries.len() * 96);
    for e in entries {
        let kind = match e.kind {
            EntryKind::File => "f",
            EntryKind::Dir => "d",
            EntryKind::InternalSymlink => "l",
        };
        let exec = if e.is_executable() { "x" } else { "-" };
        out.push_str(kind);
        out.push('\t');
        out.push_str(exec);
        out.push('\t');
        out.push_str(&e.path);
        out.push('\t');
        out.push_str(&e.digest);
        out.push('\n');
    }
    out
}

/// Compute a tree digest from already-built entries.
pub fn tree_digest(entries: &[FileEntry]) -> String {
    hash_bytes(canonical_manifest(entries).as_bytes())
}

/// Walk `root` and build its package tree.
///
/// Refuses anything that is not a file, directory, or package-internal symlink,
/// and enforces every bound in `limits`. Empty directories are recorded so that a
/// package which relies on one (an empty `assets/`, say) round-trips.
pub fn build(root: &Path, limits: &Limits) -> Result<PackageTree> {
    let real_root = fs::canonicalize(root).ctx("resolving package root", root)?;
    let mut entries: Vec<FileEntry> = Vec::new();
    let mut total_bytes = 0u64;

    let walker = walkdir::WalkDir::new(&real_root)
        .follow_links(false)
        .max_depth(limits.max_depth)
        .sort_by_file_name();

    for item in walker {
        let item = item.map_err(|err| {
            let path = err.path().unwrap_or(&real_root).to_path_buf();
            match err.into_io_error() {
                Some(io) => Error::io("walking package", path, io),
                None => Error::Internal(format!("walk failed under {}", path.display())),
            }
        })?;

        let path = item.path();
        if path == real_root {
            continue;
        }

        let relative = path
            .strip_prefix(&real_root)
            .map_err(|_| Error::Internal(format!("{} is not under its root", path.display())))?;
        let raw_rel = relative.to_string_lossy().replace('\\', "/");
        let safe: SafeRelPath = safepath::sanitize_relative(&raw_rel, limits)?;

        entries.push(entry_for(
            path,
            &safe,
            &real_root,
            limits,
            &mut total_bytes,
        )?);
        limits.check_count(entries.len())?;
        limits.check_extract_total(total_bytes)?;
    }

    entries.sort_by(|a, b| a.path.cmp(&b.path));

    // A package that cannot be materialised identically on every supported
    // platform is refused here rather than deployed and discovered later.
    let safe_paths: Vec<SafeRelPath> = entries
        .iter()
        .map(|e| safepath::sanitize_relative(&e.path, limits))
        .collect::<Result<Vec<_>>>()?;
    if let Some((a, b)) = safepath::find_case_collision(&safe_paths) {
        return Err(Error::UnsafeEntry {
            entry: format!("{a} and {b}"),
            reason: "two paths differ only by letter case, which collides on macOS and Windows"
                .into(),
        });
    }

    let digest = tree_digest(&entries);
    Ok(PackageTree {
        entries,
        digest,
        total_bytes,
    })
}

/// Build one entry, validating its kind, mode, and (for links) its target.
fn entry_for(
    path: &Path,
    safe: &SafeRelPath,
    package_root: &Path,
    limits: &Limits,
    total_bytes: &mut u64,
) -> Result<FileEntry> {
    let kind = safepath::classify(path)?;
    let meta = fs::symlink_metadata(path).ctx("reading metadata", path)?;
    let mode = safepath::mode_of(&meta);

    if let Some(mode) = mode {
        safepath::check_mode_safe(safe.as_str(), mode)?;
    }

    match kind {
        EntryKind::Dir => Ok(FileEntry {
            path: safe.as_str().to_string(),
            kind,
            mode,
            size: 0,
            digest: String::new(),
            link_target: None,
        }),
        EntryKind::File => {
            let (digest, size) = hash_file(path, limits, safe.as_str())?;
            *total_bytes += size;
            Ok(FileEntry {
                path: safe.as_str().to_string(),
                kind,
                mode,
                size,
                digest,
                link_target: None,
            })
        }
        EntryKind::InternalSymlink => {
            // Only links that stay inside the package are kept. Anything else is
            // refused with a clear reason rather than silently dropped, because a
            // skill depending on a link we removed would fail confusingly later.
            let target = safepath::validate_internal_link(package_root, path)?;
            let target_str = target.to_string_lossy().replace('\\', "/");
            Ok(FileEntry {
                path: safe.as_str().to_string(),
                kind,
                mode,
                size: 0,
                // Hash the target, so repointing a link is a tree change.
                digest: hash_bytes(target_str.as_bytes()),
                link_target: Some(target_str),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Build a small package on disk and return its root.
    fn fixture(dir: &Path, exec: bool) -> &Path {
        fs::create_dir_all(dir.join("scripts")).unwrap();
        fs::create_dir_all(dir.join("references")).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            "---\nname: t\ndescription: d\n---\nbody\n",
        )
        .unwrap();
        fs::write(dir.join("scripts/run.sh"), "#!/bin/sh\necho hi\n").unwrap();
        fs::write(dir.join("references/REFERENCE.md"), "ref\n").unwrap();
        #[cfg(unix)]
        if exec {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                dir.join("scripts/run.sh"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let _ = exec;
        dir
    }

    #[test]
    fn builds_a_complete_inventory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fixture(tmp.path(), false);
        let tree = build(root, &Limits::default()).unwrap();

        let paths: Vec<&str> = tree.paths().collect();
        assert_eq!(
            paths,
            vec![
                "SKILL.md",
                "references",
                "references/REFERENCE.md",
                "scripts",
                "scripts/run.sh",
            ]
        );
        assert!(tree.has_skill_md());
        assert_eq!(tree.digest.len(), 64);
        assert!(tree.total_bytes > 0);
    }

    #[test]
    fn digest_is_deterministic_and_order_independent() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let ta = build(fixture(a.path(), false), &Limits::default()).unwrap();
        let tb = build(fixture(b.path(), false), &Limits::default()).unwrap();
        assert_eq!(ta.digest, tb.digest, "identical content must hash the same");
    }

    #[test]
    fn digest_changes_when_any_file_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fixture(tmp.path(), false).to_path_buf();
        let before = build(&root, &Limits::default()).unwrap();

        // An edit to a file that is not SKILL.md must still move the digest.
        fs::write(root.join("references/REFERENCE.md"), "changed\n").unwrap();
        let after = build(&root, &Limits::default()).unwrap();
        assert_ne!(before.digest, after.digest);
    }

    #[test]
    fn digest_changes_when_a_file_is_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fixture(tmp.path(), false).to_path_buf();
        let before = build(&root, &Limits::default()).unwrap();
        fs::remove_file(root.join("references/REFERENCE.md")).unwrap();
        let after = build(&root, &Limits::default()).unwrap();
        assert_ne!(
            before.digest, after.digest,
            "a deletion is a change and must be visible to reconciliation"
        );
        assert_eq!(
            before.paths_missing_from(&after),
            vec!["references/REFERENCE.md"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn digest_changes_when_the_executable_bit_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fixture(tmp.path(), false).to_path_buf();
        let before = build(&root, &Limits::default()).unwrap();

        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            root.join("scripts/run.sh"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let after = build(&root, &Limits::default()).unwrap();
        assert_ne!(before.digest, after.digest);
        assert_eq!(after.changed_against(&before), vec!["scripts/run.sh"]);
    }

    #[test]
    fn mtime_alone_does_not_change_the_digest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fixture(tmp.path(), false).to_path_buf();
        let before = build(&root, &Limits::default()).unwrap();

        // Rewrite identical bytes. The mtime moves, the content does not.
        let same = fs::read(root.join("SKILL.md")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(root.join("SKILL.md"), same).unwrap();

        let after = build(&root, &Limits::default()).unwrap();
        assert_eq!(
            before.digest, after.digest,
            "a timestamp is not evidence of a content change"
        );
    }

    #[test]
    fn refuses_a_symlink_that_escapes_the_package() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("pkg");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("SKILL.md"), "---\nname: t\ndescription: d\n---\n").unwrap();
        fs::write(tmp.path().join("secret.txt"), "private").unwrap();

        #[cfg(unix)]
        std::os::unix::fs::symlink("../secret.txt", root.join("leak.txt")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(tmp.path().join("secret.txt"), root.join("leak.txt"))
            .unwrap();

        let err = build(&root, &Limits::default()).unwrap_err();
        assert!(
            matches!(err, Error::UnsafeEntry { .. }),
            "expected refusal, got {err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn keeps_a_valid_internal_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("pkg");
        fs::create_dir_all(root.join("references")).unwrap();
        fs::write(root.join("SKILL.md"), "---\nname: t\ndescription: d\n---\n").unwrap();
        fs::write(root.join("references/real.md"), "content").unwrap();
        std::os::unix::fs::symlink("references/real.md", root.join("alias.md")).unwrap();

        let tree = build(&root, &Limits::default()).unwrap();
        let link = tree.get("alias.md").expect("link should be recorded");
        assert_eq!(link.kind, EntryKind::InternalSymlink);
        assert_eq!(link.link_target.as_deref(), Some("references/real.md"));
    }

    #[test]
    fn enforces_the_file_count_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for i in 0..10 {
            fs::write(root.join(format!("f{i}.md")), "x").unwrap();
        }
        let tight = Limits {
            max_files: 3,
            ..Limits::default()
        };
        let err = build(root, &tight).unwrap_err();
        assert!(matches!(err, Error::LimitExceeded { .. }), "{err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_setuid_content() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("SKILL.md"), "---\nname: t\ndescription: d\n---\n").unwrap();
        let evil = root.join("evil");
        fs::write(&evil, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&evil, fs::Permissions::from_mode(0o4755)).unwrap();

        let err = build(root, &Limits::default()).unwrap_err();
        assert!(matches!(err, Error::UnsafeEntry { .. }), "{err:?}");
    }
}
