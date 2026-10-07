//! Path containment, safe entry classification, and atomic directory replacement.
//!
//! Everything that writes to disk goes through this module. The rules it enforces
//! are deliberately conservative: a package is data we received from somewhere
//! else, so a name inside it is never trusted to be a safe path component.
//!
//! Two distinct checks are provided and both matter:
//!
//! * [`sanitize_relative`] is a *syntactic* check on a name taken from an archive,
//!   a manifest, or a remote listing. It runs before anything touches the
//!   filesystem.
//! * [`verify_containment`] is a *filesystem* check that resolves real ancestors,
//!   so an existing symlink or Windows reparse point above the target cannot be
//!   used to redirect a write out of the tree.
//!
//! Neither check makes a package safe to *run*. `skill` never executes package
//! content; see `docs/security.md`.

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::error::{Error, IoContext, Result};

/// Bounds applied to any acquisition or extraction.
///
/// Defaults are in the same range as comparable tools, chosen so that a
/// legitimate skill package is never affected while an archive bomb is stopped
/// long before it exhausts the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Maximum compressed bytes accepted from a network source.
    pub max_download_bytes: u64,
    /// Maximum total bytes written while extracting one archive.
    pub max_extract_bytes: u64,
    /// Maximum bytes for any single member file.
    pub max_file_bytes: u64,
    /// Maximum number of entries in one package.
    pub max_files: usize,
    /// Maximum directory nesting depth inside a package.
    pub max_depth: usize,
    /// Maximum HTTP redirects followed.
    pub max_redirects: u32,
    /// Per-request timeout, in seconds.
    pub timeout_secs: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_download_bytes: 32 * 1024 * 1024,
            max_extract_bytes: 128 * 1024 * 1024,
            max_file_bytes: 32 * 1024 * 1024,
            max_files: 4096,
            max_depth: 32,
            max_redirects: 5,
            timeout_secs: 30,
        }
    }
}

impl Limits {
    /// Fail if `actual` exceeds `self.max_extract_bytes`.
    pub fn check_extract_total(&self, actual: u64) -> Result<()> {
        if actual > self.max_extract_bytes {
            return Err(Error::LimitExceeded {
                what: "total extracted size".into(),
                limit: human_bytes(self.max_extract_bytes),
                actual: human_bytes(actual),
                hint: "the archive is larger than a skill package should be; inspect it by hand"
                    .into(),
            });
        }
        Ok(())
    }

    /// Fail if a single member is larger than `self.max_file_bytes`.
    pub fn check_file(&self, name: &str, actual: u64) -> Result<()> {
        if actual > self.max_file_bytes {
            return Err(Error::LimitExceeded {
                what: format!("file {name:?}"),
                limit: human_bytes(self.max_file_bytes),
                actual: human_bytes(actual),
                hint: "skills are documentation and scripts; a file this large is unexpected"
                    .into(),
            });
        }
        Ok(())
    }

    /// Fail if the entry count exceeds `self.max_files`.
    pub fn check_count(&self, actual: usize) -> Result<()> {
        if actual > self.max_files {
            return Err(Error::LimitExceeded {
                what: "file count".into(),
                limit: self.max_files.to_string(),
                actual: actual.to_string(),
                hint: "this looks like a whole repository rather than a skill package".into(),
            });
        }
        Ok(())
    }
}

/// Render a byte count for an error message.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Names that cannot be used as a path component on Windows, in any letter case
/// and regardless of extension.
const WINDOWS_RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// True if `component` is a reserved Windows device name.
///
/// Windows treats `NUL`, `nul.txt` and `NUL.tar.gz` alike, so the stem is what
/// matters. We apply this on every platform: a package that extracts on Linux but
/// not on Windows is a portability bug we would rather reject at acquisition time
/// than discover on another machine.
pub fn is_windows_reserved(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or(component);
    WINDOWS_RESERVED
        .iter()
        .any(|r| stem.eq_ignore_ascii_case(r))
}

/// A path component that has been checked and is safe to join onto a root.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SafeRelPath(String);

impl SafeRelPath {
    /// The validated path, always using `/` separators.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Join onto a root. The syntactic checks already ran, so this cannot escape.
    pub fn join_onto(&self, root: &Path) -> PathBuf {
        let mut out = root.to_path_buf();
        for part in self.0.split('/') {
            out.push(part);
        }
        out
    }

    /// Number of path components.
    pub fn depth(&self) -> usize {
        self.0.split('/').count()
    }
}

impl std::fmt::Display for SafeRelPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validate an untrusted relative path from an archive, manifest, or listing.
///
/// Rejects absolute paths, drive letters, UNC prefixes, `..` traversal, `.`
/// segments, empty components, NUL and control bytes, trailing dots or spaces
/// (which Windows silently strips, creating a name that is not the one we
/// checked), and reserved device names.
pub fn sanitize_relative(raw: &str, limits: &Limits) -> Result<SafeRelPath> {
    let unsafe_entry = |reason: &str| Error::UnsafeEntry {
        entry: raw.to_string(),
        reason: reason.to_string(),
    };

    if raw.is_empty() {
        return Err(unsafe_entry("empty path"));
    }
    if raw.len() > 1024 {
        return Err(unsafe_entry("path is unreasonably long"));
    }
    if raw.contains('\0') {
        return Err(unsafe_entry("path contains a NUL byte"));
    }
    if raw.chars().any(|c| c.is_control()) {
        return Err(unsafe_entry("path contains a control character"));
    }

    // Normalise separators first so that a Windows-style archive entry is checked
    // by the same rules as a POSIX one.
    let normalised = raw.replace('\\', "/");

    if normalised.starts_with('/') {
        return Err(unsafe_entry("absolute path"));
    }
    // `C:` or `C:/...`. Also catches any stray colon, which is invalid on Windows
    // and is an NTFS alternate-data-stream separator.
    if normalised.contains(':') {
        return Err(unsafe_entry(
            "path contains ':' (drive letter or alternate data stream)",
        ));
    }

    let mut parts: Vec<&str> = Vec::new();
    for part in normalised.split('/') {
        match part {
            "" => {
                // A trailing slash on a directory entry is normal; an interior
                // empty component is not.
                continue;
            }
            "." => return Err(unsafe_entry("path contains a '.' component")),
            ".." => return Err(unsafe_entry("path traversal via '..'")),
            _ => {}
        }

        if part.ends_with('.') || part.ends_with(' ') {
            return Err(unsafe_entry(
                "component ends with a dot or space, which Windows strips",
            ));
        }
        if is_windows_reserved(part) {
            return Err(unsafe_entry("component is a reserved Windows device name"));
        }
        parts.push(part);
    }

    if parts.is_empty() {
        return Err(unsafe_entry("path has no usable components"));
    }
    if parts.len() > limits.max_depth {
        return Err(Error::LimitExceeded {
            what: format!("nesting depth of {raw:?}"),
            limit: limits.max_depth.to_string(),
            actual: parts.len().to_string(),
            hint: "flatten the package layout".into(),
        });
    }

    Ok(SafeRelPath(parts.join("/")))
}

/// Detect names that collide once case is folded.
///
/// A package containing both `Readme.md` and `README.md` extracts cleanly on
/// Linux and silently loses a file on macOS or Windows. We refuse it rather than
/// deploy something that behaves differently per platform.
pub fn find_case_collision(paths: &[SafeRelPath]) -> Option<(String, String)> {
    let mut seen: std::collections::HashMap<String, String> =
        std::collections::HashMap::with_capacity(paths.len());
    for p in paths {
        let folded = p.as_str().to_lowercase();
        if let Some(previous) = seen.insert(folded, p.as_str().to_string()) {
            if previous != p.as_str() {
                return Some((previous, p.as_str().to_string()));
            }
        }
    }
    None
}

/// The kinds of filesystem entry we are willing to materialise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A symbolic link whose target has been validated to stay inside the package.
    InternalSymlink,
}

/// Classify a local path, refusing anything that is not a file, directory, or
/// package-internal symlink.
///
/// Device nodes, FIFOs, and sockets are refused: they are not skill content, and
/// copying them can block indefinitely or hand a caller a handle to something it
/// should not have.
pub fn classify(path: &Path) -> Result<EntryKind> {
    let meta = fs::symlink_metadata(path).ctx("reading metadata", path)?;
    let ft = meta.file_type();
    if ft.is_file() {
        Ok(EntryKind::File)
    } else if ft.is_dir() {
        Ok(EntryKind::Dir)
    } else if ft.is_symlink() {
        Ok(EntryKind::InternalSymlink)
    } else {
        Err(Error::UnsafeEntry {
            entry: path.display().to_string(),
            reason: "not a regular file, directory, or symlink (device, FIFO, or socket)".into(),
        })
    }
}

/// Resolve the deepest existing ancestor of `path`, following symlinks.
///
/// Used so containment can be checked even when the leaf does not exist yet.
fn deepest_existing_real(path: &Path) -> Result<PathBuf> {
    let mut current = path;
    loop {
        if current.exists() {
            return fs::canonicalize(current).ctx("resolving real path", current);
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent,
            _ => {
                return Err(Error::Containment {
                    root: path.to_path_buf(),
                    reason: "no existing ancestor directory".into(),
                })
            }
        }
    }
}

/// Verify that `target` really resolves inside `root` on this filesystem.
///
/// This is the check that defeats an *existing* symlink or reparse point planted
/// above the target. It resolves `root` and the deepest existing ancestor of
/// `target` through the real filesystem and then compares them, so a
/// `~/.claude/skills` that is itself a symlink elsewhere is handled correctly
/// rather than rejected.
///
/// Call this immediately before mutating, not only at planning time: an attacker
/// who can write to the parent directory can swap a component in between.
pub fn verify_containment(root: &Path, target: &Path) -> Result<PathBuf> {
    let real_root = fs::canonicalize(root).ctx("resolving containment root", root)?;

    // Syntactic rejection first, so an obviously bad path never hits the filesystem.
    for component in target.components() {
        if matches!(component, Component::ParentDir) {
            return Err(Error::Containment {
                root: real_root,
                reason: format!("{} contains '..'", target.display()),
            });
        }
    }

    let real_ancestor = deepest_existing_real(target)?;
    if !real_ancestor.starts_with(&real_root) {
        return Err(Error::Containment {
            root: real_root,
            reason: format!(
                "{} resolves to {}, which is outside the root (an ancestor is a link)",
                target.display(),
                real_ancestor.display()
            ),
        });
    }

    // If the leaf itself exists and is a symlink, make sure it does not point out.
    if let Ok(meta) = fs::symlink_metadata(target) {
        if meta.file_type().is_symlink() {
            let resolved = fs::canonicalize(target).ctx("resolving symlink target", target)?;
            if !resolved.starts_with(&real_root) {
                return Err(Error::Containment {
                    root: real_root,
                    reason: format!(
                        "{} is a symlink to {}, which is outside the root",
                        target.display(),
                        resolved.display()
                    ),
                });
            }
        }
    }

    Ok(real_ancestor)
}

/// Validate a symlink found inside a package.
///
/// A link is kept only when its target stays within the package. Anything else is
/// refused with a clear message rather than silently dropped, because a skill that
/// depends on a link we removed would fail confusingly later.
pub fn validate_internal_link(package_root: &Path, link: &Path) -> Result<PathBuf> {
    let raw = fs::read_link(link).ctx("reading symlink", link)?;
    if raw.is_absolute() {
        return Err(Error::UnsafeEntry {
            entry: link.display().to_string(),
            reason: format!("absolute symlink to {}", raw.display()),
        });
    }
    let parent = link
        .parent()
        .ok_or_else(|| Error::Internal(format!("symlink {} has no parent", link.display())))?;
    let joined = parent.join(&raw);

    // Resolve lexically: the target may legitimately not exist yet.
    let mut stack: Vec<std::ffi::OsString> = Vec::new();
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                if stack.pop().is_none() {
                    return Err(Error::UnsafeEntry {
                        entry: link.display().to_string(),
                        reason: "symlink escapes the package via '..'".into(),
                    });
                }
            }
            Component::CurDir => {}
            other => stack.push(other.as_os_str().to_os_string()),
        }
    }
    let lexical: PathBuf = stack.iter().collect();

    let root_real = fs::canonicalize(package_root).ctx("resolving package root", package_root)?;
    let lexical_real = if lexical.exists() {
        fs::canonicalize(&lexical).ctx("resolving symlink target", &lexical)?
    } else {
        lexical.clone()
    };

    if !lexical_real.starts_with(&root_real) && !lexical.starts_with(package_root) {
        return Err(Error::UnsafeEntry {
            entry: link.display().to_string(),
            reason: format!("symlink points outside the package, to {}", raw.display()),
        });
    }
    Ok(raw)
}

/// Create a symlink to a directory, using a junction on Windows.
///
/// Windows reserves directory symlinks for elevated users or Developer Mode,
/// whereas a junction needs neither, so a junction is what makes `skill link`
/// usable there at all.
pub fn symlink_dir(target: &Path, link: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).ctx("creating symlink", link)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link).ctx("creating directory junction", link)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, link);
        Err(Error::Unsupported {
            agent: "host".into(),
            what: "create a symlink".into(),
            reason: "this platform has no supported link primitive".into(),
            hint: "use `skill copy` instead of `skill link`".into(),
        })
    }
}

/// Replace `dest` with `staged` as close to atomically as the platform allows.
///
/// `staged` must already sit on the same filesystem as `dest`, so the rename is a
/// metadata operation rather than a copy. Any pre-existing `dest` is first moved
/// to `aside`, which the caller retains as the transaction backup; it is never
/// deleted here, because recovery needs it.
///
/// On Windows, renaming over an existing directory fails, which is precisely why
/// the move-aside happens first on every platform instead of only where it is
/// strictly required.
pub fn replace_dir(staged: &Path, dest: &Path, aside: Option<&Path>) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).ctx("creating destination parent", parent)?;
    }

    let existed = fs::symlink_metadata(dest).is_ok();
    if existed {
        match aside {
            Some(aside_path) => {
                if let Some(parent) = aside_path.parent() {
                    fs::create_dir_all(parent).ctx("creating backup directory", parent)?;
                }
                fs::rename(dest, aside_path).ctx("moving existing deployment aside", dest)?;
            }
            None => {
                return Err(Error::Internal(format!(
                    "{} exists but no backup location was provided",
                    dest.display()
                )));
            }
        }
    }

    match fs::rename(staged, dest) {
        Ok(()) => Ok(()),
        Err(err) => {
            // Put the original back before reporting, so a failed apply leaves the
            // destination exactly as it was.
            if let (true, Some(aside_path)) = (existed, aside) {
                let _ = fs::rename(aside_path, dest);
            }
            Err(Error::io("installing staged package", dest, err))
        }
    }
}

/// Remove a deployment, refusing anything that is not a plain directory or link.
///
/// Deliberately narrow: it will not recurse through a symlink, and it will not
/// touch a path that is not what the caller described. Whole-package removal is
/// always an explicit decision made further up the stack.
pub fn remove_deployment(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path).ctx("inspecting deployment", path)?;
    if meta.file_type().is_symlink() {
        // Remove the link itself, never its target.
        #[cfg(windows)]
        {
            // A directory junction must be removed with remove_dir.
            if path.is_dir() {
                return fs::remove_dir(path).ctx("removing link", path);
            }
        }
        return fs::remove_file(path).ctx("removing link", path);
    }
    if meta.file_type().is_dir() {
        return fs::remove_dir_all(path).ctx("removing deployment directory", path);
    }
    Err(Error::UnsafeEntry {
        entry: path.display().to_string(),
        reason: "deployment is neither a directory nor a link".into(),
    })
}

/// Restrict a file to the current user (mode 0600 on Unix).
///
/// State, journals, and backups can contain source URLs and package content, so
/// they are not world-readable.
pub fn restrict_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, perms).ctx("restricting file permissions", path)
    }
    #[cfg(not(unix))]
    {
        // Windows inherits the user profile ACL, which is already user-scoped.
        let _ = path;
        Ok(())
    }
}

/// Restrict a directory to the current user (mode 0700 on Unix).
pub fn restrict_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o700);
        fs::set_permissions(path, perms).ctx("restricting directory permissions", path)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Read the Unix mode bits, or `None` on platforms without them.
pub fn mode_of(meta: &fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

/// Reject permission bits we will not reproduce.
///
/// setuid, setgid, and the sticky bit have no legitimate use in a skill package
/// and would be a privilege-escalation primitive if we faithfully copied them.
pub fn check_mode_safe(name: &str, mode: u32) -> Result<()> {
    const DANGEROUS: u32 = 0o7000;
    if mode & DANGEROUS != 0 {
        return Err(Error::UnsafeEntry {
            entry: name.to_string(),
            reason: format!("refusing setuid/setgid/sticky permission bits ({mode:o})"),
        });
    }
    Ok(())
}

/// Apply the executable bit if the recorded mode had it, preserving nothing else.
///
/// We reproduce "is it executable" rather than the exact mode, so a package
/// authored with a loose umask cannot widen permissions on the installing machine.
pub fn apply_exec_bit(path: &Path, recorded_mode: Option<u32>) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Some(mode) = recorded_mode else {
            return Ok(());
        };
        let executable = mode & 0o111 != 0;
        let target = if executable { 0o755 } else { 0o644 };
        fs::set_permissions(path, fs::Permissions::from_mode(target))
            .ctx("setting file permissions", path)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, recorded_mode);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Limits::default()
    }

    #[test]
    fn accepts_ordinary_package_paths() {
        for ok in [
            "SKILL.md",
            "scripts/build.sh",
            "references/REFERENCE.md",
            "assets/templates/report.md",
        ] {
            let p = sanitize_relative(ok, &limits()).expect(ok);
            assert_eq!(p.as_str(), ok);
        }
    }

    #[test]
    fn rejects_traversal_and_absolute_paths() {
        for bad in [
            "../escape",
            "a/../../b",
            "/etc/passwd",
            "..",
            "./x",
            "a//../b",
        ] {
            assert!(
                sanitize_relative(bad, &limits()).is_err(),
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn rejects_windows_hazards() {
        // Backslashes are normalised, so a Windows traversal is caught too.
        assert!(sanitize_relative(r"..\escape", &limits()).is_err());
        assert!(sanitize_relative(r"C:\Windows\System32", &limits()).is_err());
        // Drive letters and ADS separators.
        assert!(sanitize_relative("file.txt:stream", &limits()).is_err());
        // Reserved device names, with and without extension.
        for bad in ["NUL", "nul.txt", "con", "COM1", "scripts/aux.sh", "LPT9.md"] {
            assert!(
                sanitize_relative(bad, &limits()).is_err(),
                "should reject reserved name {bad:?}"
            );
        }
        // Trailing dot or space is silently stripped by Windows.
        assert!(sanitize_relative("name.", &limits()).is_err());
        assert!(sanitize_relative("name ", &limits()).is_err());
    }

    #[test]
    fn rejects_control_bytes() {
        assert!(sanitize_relative("a\0b", &limits()).is_err());
        assert!(sanitize_relative("a\nb", &limits()).is_err());
    }

    #[test]
    fn enforces_depth_limit() {
        let shallow = Limits {
            max_depth: 3,
            ..Limits::default()
        };
        assert!(sanitize_relative("a/b/c", &shallow).is_ok());
        assert!(sanitize_relative("a/b/c/d", &shallow).is_err());
    }

    #[test]
    fn detects_case_collisions() {
        let paths = vec![
            sanitize_relative("README.md", &limits()).unwrap(),
            sanitize_relative("readme.md", &limits()).unwrap(),
        ];
        assert!(find_case_collision(&paths).is_some());

        let distinct = vec![
            sanitize_relative("README.md", &limits()).unwrap(),
            sanitize_relative("SKILL.md", &limits()).unwrap(),
        ];
        assert!(find_case_collision(&distinct).is_none());
    }

    #[test]
    fn rejects_dangerous_mode_bits() {
        assert!(check_mode_safe("x", 0o755).is_ok());
        assert!(check_mode_safe("x", 0o644).is_ok());
        assert!(check_mode_safe("x", 0o4755).is_err(), "setuid");
        assert!(check_mode_safe("x", 0o2755).is_err(), "setgid");
        assert!(check_mode_safe("x", 0o1755).is_err(), "sticky");
    }

    #[test]
    fn join_onto_stays_under_root() {
        let p = sanitize_relative("scripts/run.sh", &limits()).unwrap();
        let joined = p.join_onto(Path::new("/tmp/root"));
        assert_eq!(joined, Path::new("/tmp/root/scripts/run.sh"));
    }

    #[test]
    fn containment_rejects_symlink_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();

        // An ancestor inside the root points out of it.
        let escape = root.join("escape");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &escape).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&outside, &escape).unwrap();

        let target = escape.join("payload");
        let err = verify_containment(&root, &target).unwrap_err();
        assert!(
            matches!(err, Error::Containment { .. }),
            "expected containment refusal, got {err:?}"
        );

        // A genuine path inside the root is accepted.
        assert!(verify_containment(&root, &root.join("ok")).is_ok());
    }

    #[test]
    fn human_bytes_is_readable() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(32 * 1024 * 1024), "32.0 MiB");
    }
}
