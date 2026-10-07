//! Finding skill packages inside an arbitrary directory tree.
//!
//! # The boundary problem
//!
//! A source can be any of several shapes, and guessing wrong is destructive in
//! both directions: too narrow and a package loses its `scripts/`, too wide and we
//! copy somebody's home directory because a `SKILL.md` happened to be in it.
//!
//! The rules applied here, in order:
//!
//! 1. A directory holding `SKILL.md` **is** the package. Its whole subtree is
//!    package content.
//! 2. A directory holding no `SKILL.md` but containing directories that do is a
//!    **collection**. Each child is a separate package.
//! 3. A path to a `SKILL.md` **file** resolves to its containing directory,
//!    which is then treated as case 1. This is the "import that package
//!    boundary" rule.
//! 4. A standalone `.md` file that is not named `SKILL.md`, or a `SKILL.md` whose
//!    directory holds nothing else, becomes a single-file package and the caller
//!    is warned that relative references cannot be resolved.
//!
//! Search depth is bounded. Once a `SKILL.md` is found at some level, nothing
//! deeper is treated as a separate package, because a package's own subtree
//! belongs to it. That is what stops a repository checkout from being flattened
//! into dozens of overlapping packages.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, IoContext, Result};
use crate::pkg::frontmatter::{self, Finding};
use crate::pkg::identity::InstallName;
use crate::safepath::Limits;

/// How deep to look for packages below the supplied root.
///
/// Three levels covers the conventional layouts (`<root>/SKILL.md`,
/// `<root>/<name>/SKILL.md`, `<root>/skills/<name>/SKILL.md`,
/// `<root>/skills/<category>/<name>/SKILL.md`) without turning an arbitrary
/// checkout into a recursive hunt.
pub const DEFAULT_SEARCH_DEPTH: usize = 4;

/// Directory names that are never package content or package containers.
const SKIP_DIRS: [&str; 10] = [
    ".git",
    ".svn",
    ".hg",
    "node_modules",
    "target",
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    ".mypy_cache",
];

/// A package found by scanning.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// The package root directory. For a single-file package this is a staging
    /// directory the caller created, not the original location.
    pub root: PathBuf,
    /// Path of the package root relative to the scanned root, `""` when they match.
    ///
    /// Persisted as the package selector, so it is what distinguishes two skills
    /// coming from one repository.
    pub selector: String,
    /// Install name, taken from frontmatter `name` when valid and otherwise from
    /// the directory name.
    pub name: InstallName,
    /// The `name` the frontmatter actually declared, when it declared one.
    pub declared_name: Option<String>,
    pub description: Option<String>,
    /// Validation and compatibility findings for this package.
    pub findings: Vec<Finding>,
    /// True when the package was synthesised from a lone Markdown file.
    pub standalone: bool,
}

impl Candidate {
    /// True when nothing blocks installing this package on every agent.
    pub fn is_installable(&self) -> bool {
        frontmatter::blocking_errors(&self.findings).is_empty()
    }
}

/// What the scanned path turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// The path is itself one package.
    SinglePackage,
    /// The path contains several packages.
    Collection,
    /// The path is a lone Markdown file.
    StandaloneFile,
}

/// Result of scanning a path.
#[derive(Debug, Clone)]
pub struct ScanResult {
    pub shape: Shape,
    pub candidates: Vec<Candidate>,
    /// Advisory messages about the scan itself, not about any one package.
    pub notes: Vec<String>,
}

impl ScanResult {
    /// Select candidates by name, failing with the available names when one is
    /// not found. Keeps the ordering of `wanted` so output is predictable.
    pub fn select(&self, wanted: &[String]) -> Result<Vec<&Candidate>> {
        let mut out = Vec::with_capacity(wanted.len());
        for want in wanted {
            let found = self
                .candidates
                .iter()
                .find(|c| c.name.as_str() == want || c.declared_name.as_deref() == Some(want));
            match found {
                Some(c) => out.push(c),
                None => {
                    let mut available: Vec<&str> =
                        self.candidates.iter().map(|c| c.name.as_str()).collect();
                    available.sort_unstable();
                    return Err(Error::Usage(format!(
                        "no skill named {want:?} in this source\nhint: available skills are {}",
                        if available.is_empty() {
                            "(none found)".to_string()
                        } else {
                            available.join(", ")
                        }
                    )));
                }
            }
        }
        Ok(out)
    }
}

/// True when `dir` directly contains a `SKILL.md`.
fn holds_skill_md(dir: &Path) -> bool {
    dir.join("SKILL.md").is_file()
}

/// Scan `path` and classify what was found.
///
/// `path` may be a directory or a file. Nothing is copied or modified here; this
/// is a read-only survey used to build a plan.
pub fn scan(path: &Path, limits: &Limits, max_depth: usize) -> Result<ScanResult> {
    let meta = fs::symlink_metadata(path).ctx("inspecting source", path)?;

    if meta.file_type().is_file() {
        return scan_file(path, limits);
    }
    if !meta.file_type().is_dir() {
        // A symlink to a directory is fine; resolve and continue.
        let resolved = fs::canonicalize(path).ctx("resolving source", path)?;
        if resolved.is_dir() {
            return scan_dir(&resolved, limits, max_depth);
        }
        return Err(Error::UnsupportedSource {
            locator: path.display().to_string(),
            reason: "source is neither a file nor a directory".into(),
        });
    }

    scan_dir(path, limits, max_depth)
}

/// Handle a path that points at a file.
fn scan_file(path: &Path, limits: &Limits) -> Result<ScanResult> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    // Rule 3: a SKILL.md inside a real package imports that package's boundary,
    // rather than becoming a lone file that has lost its scripts and references.
    if file_name == "SKILL.md" {
        let dir = path.parent().ok_or_else(|| Error::UnsupportedSource {
            locator: path.display().to_string(),
            reason: "SKILL.md has no containing directory".into(),
        })?;

        let siblings = fs::read_dir(dir)
            .ctx("listing the package directory", dir)?
            .filter_map(std::result::Result::ok)
            .filter(|e| e.file_name() != "SKILL.md")
            .count();

        let mut result = scan_dir(dir, limits, 1)?;
        result.notes.push(format!(
            "source is a SKILL.md inside {}, so that whole package directory was imported ({siblings} other entr{} alongside it)",
            dir.display(),
            if siblings == 1 { "y" } else { "ies" }
        ));
        return Ok(result);
    }

    // Rule 4: any other single Markdown file becomes a one-file package.
    if path.extension().and_then(|e| e.to_str()) != Some("md") {
        return Err(Error::UnsupportedSource {
            locator: path.display().to_string(),
            reason: "a standalone skill source must be a Markdown file".into(),
        });
    }

    let content = fs::read_to_string(path).ctx("reading standalone skill file", path)?;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    let candidate =
        candidate_from_content(&content, path.to_path_buf(), String::new(), &stem, true)?;

    Ok(ScanResult {
        shape: Shape::StandaloneFile,
        notes: vec![format!(
            "{} is a standalone file, so it became a single-file package. Any relative \
             reference inside it (scripts/, references/, assets/) cannot be resolved and will \
             not work once installed.",
            path.display()
        )],
        candidates: vec![candidate],
    })
}

/// Handle a path that points at a directory.
fn scan_dir(root: &Path, limits: &Limits, max_depth: usize) -> Result<ScanResult> {
    // Rule 1: the directory is itself a package.
    if holds_skill_md(root) {
        let name_hint = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "skill".to_string());
        let content = fs::read_to_string(root.join("SKILL.md"))
            .ctx("reading SKILL.md", root.join("SKILL.md"))?;
        let candidate = candidate_from_content(
            &content,
            root.to_path_buf(),
            String::new(),
            &name_hint,
            false,
        )?;
        return Ok(ScanResult {
            shape: Shape::SinglePackage,
            candidates: vec![candidate],
            notes: Vec::new(),
        });
    }

    // Rule 2: look for packages below, stopping at the first level that has them.
    let mut found: BTreeMap<String, Candidate> = BTreeMap::new();
    collect(root, root, max_depth, &mut found)?;
    let _ = limits;

    if found.is_empty() {
        return Err(Error::NotAPackage(root.to_path_buf()));
    }

    let mut notes = Vec::new();
    if found.len() > 1 {
        notes.push(format!(
            "{} is a collection of {} skills; select with --skill or --all-skills",
            root.display(),
            found.len()
        ));
    }

    Ok(ScanResult {
        shape: Shape::Collection,
        candidates: found.into_values().collect(),
        notes,
    })
}

/// Recursively collect packages, never descending into one that was found.
fn collect(
    scan_root: &Path,
    dir: &Path,
    remaining_depth: usize,
    out: &mut BTreeMap<String, Candidate>,
) -> Result<()> {
    if remaining_depth == 0 {
        return Ok(());
    }

    let mut children: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(dir).ctx("listing directory", dir)? {
        let entry = entry.ctx("reading directory entry", dir)?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        // Do not follow links while surveying: a link could point anywhere, and
        // following it is how a scan escapes the tree it was asked about.
        let meta = fs::symlink_metadata(&path).ctx("reading metadata", &path)?;
        if !meta.file_type().is_dir() {
            continue;
        }
        if SKIP_DIRS.contains(&name.as_str()) || name.starts_with('.') {
            continue;
        }
        children.push(path);
    }
    children.sort();

    for child in children {
        let name_hint = child
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        if holds_skill_md(&child) {
            let selector = child
                .strip_prefix(scan_root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            let skill_md = child.join("SKILL.md");
            let content = fs::read_to_string(&skill_md).ctx("reading SKILL.md", &skill_md)?;
            let candidate =
                candidate_from_content(&content, child.clone(), selector, &name_hint, false)?;

            // Two packages wanting one install name is ambiguous, and picking a
            // winner silently would install the wrong skill. Report instead.
            if let Some(existing) = out.insert(candidate.name.as_str().to_string(), candidate) {
                return Err(Error::NameCollision {
                    name: existing.name.as_str().to_string(),
                    existing: format!("also at {}", existing.selector),
                });
            }
            // A package owns its whole subtree, so do not descend into it.
            continue;
        }

        collect(scan_root, &child, remaining_depth - 1, out)?;
    }

    Ok(())
}

/// Build a candidate from `SKILL.md` content, validating as we go.
fn candidate_from_content(
    content: &str,
    root: PathBuf,
    selector: String,
    dir_name: &str,
    standalone: bool,
) -> Result<Candidate> {
    let parsed = frontmatter::parse(content);
    let mut findings = parsed.findings;
    findings.extend(frontmatter::validate(&parsed.frontmatter, dir_name));

    // Prefer the declared name, falling back to the directory, which is what
    // Claude Code and Codex do. Sanitise rather than reject, so a close-enough
    // name still installs, but surface the rename.
    let declared = parsed.frontmatter.name.clone();
    let name = match declared.as_deref() {
        Some(n) => match InstallName::parse(n) {
            Ok(valid) => valid,
            Err(_) => {
                let fallback = InstallName::sanitize(n)?;
                findings.push(Finding {
                    code: "name.sanitized",
                    severity: frontmatter::Severity::Warning,
                    message: format!(
                        "frontmatter name {n:?} is not usable as a directory name; installing as \
                         {:?}",
                        fallback.as_str()
                    ),
                    agent: None,
                    confirmed: true,
                });
                fallback
            }
        },
        None => InstallName::sanitize(dir_name)?,
    };

    Ok(Candidate {
        root,
        selector,
        name,
        declared_name: declared,
        description: parsed.frontmatter.description.clone(),
        findings,
        standalone,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(dir: &Path, name: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Does {name} things.\n---\n\nbody\n"),
        )
        .unwrap();
    }

    fn limits() -> Limits {
        Limits::default()
    }

    #[test]
    fn a_directory_with_skill_md_is_one_package() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("my-skill");
        write_skill(&root, "my-skill");
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("scripts/run.sh"), "#!/bin/sh\n").unwrap();

        let result = scan(&root, &limits(), DEFAULT_SEARCH_DEPTH).unwrap();
        assert_eq!(result.shape, Shape::SinglePackage);
        assert_eq!(result.candidates.len(), 1);
        let c = &result.candidates[0];
        assert_eq!(c.name.as_str(), "my-skill");
        assert_eq!(c.selector, "");
        assert!(!c.standalone);
        assert!(c.is_installable(), "{:?}", c.findings);
    }

    #[test]
    fn a_directory_of_skills_is_a_collection() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(&tmp.path().join("alpha"), "alpha");
        write_skill(&tmp.path().join("beta"), "beta");

        let result = scan(tmp.path(), &limits(), DEFAULT_SEARCH_DEPTH).unwrap();
        assert_eq!(result.shape, Shape::Collection);
        let names: Vec<&str> = result.candidates.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta"]);
        assert_eq!(result.candidates[0].selector, "alpha");
        assert!(result.notes.iter().any(|n| n.contains("collection")));
    }

    #[test]
    fn finds_skills_nested_under_a_container_directory() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(&tmp.path().join("skills/web/design"), "design");
        let result = scan(tmp.path(), &limits(), DEFAULT_SEARCH_DEPTH).unwrap();
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].selector, "skills/web/design");
    }

    #[test]
    fn a_package_subtree_is_not_split_into_more_packages() {
        // A skill that itself ships an example skill under references/ must stay
        // one package, not become two.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("outer");
        write_skill(&root, "outer");
        write_skill(&root.join("references/example"), "inner");

        let result = scan(&root, &limits(), DEFAULT_SEARCH_DEPTH).unwrap();
        assert_eq!(result.shape, Shape::SinglePackage);
        assert_eq!(
            result.candidates.len(),
            1,
            "must not descend into a package"
        );
        assert_eq!(result.candidates[0].name.as_str(), "outer");
    }

    #[test]
    fn pointing_at_skill_md_imports_the_package_boundary() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("my-skill");
        write_skill(&root, "my-skill");
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("scripts/run.sh"), "#!/bin/sh\n").unwrap();

        let result = scan(&root.join("SKILL.md"), &limits(), DEFAULT_SEARCH_DEPTH).unwrap();
        assert_eq!(result.shape, Shape::SinglePackage);
        assert_eq!(result.candidates[0].root, root);
        assert!(
            result
                .notes
                .iter()
                .any(|n| n.contains("whole package directory")),
            "the boundary decision must be disclosed: {:?}",
            result.notes
        );
    }

    #[test]
    fn a_standalone_markdown_file_warns_about_relative_references() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("lonely.md");
        fs::write(
            &file,
            "---\nname: lonely\ndescription: A lone skill.\n---\nSee scripts/run.sh\n",
        )
        .unwrap();
        // A neighbouring file must not be collected.
        fs::write(tmp.path().join("unrelated.txt"), "not mine").unwrap();

        let result = scan(&file, &limits(), DEFAULT_SEARCH_DEPTH).unwrap();
        assert_eq!(result.shape, Shape::StandaloneFile);
        assert_eq!(result.candidates.len(), 1);
        assert!(result.candidates[0].standalone);
        assert!(
            result
                .notes
                .iter()
                .any(|n| n.contains("cannot be resolved")),
            "must warn about unresolved relative references: {:?}",
            result.notes
        );
    }

    #[test]
    fn refuses_a_non_markdown_file() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("thing.tar");
        fs::write(&file, "nope").unwrap();
        let err = scan(&file, &limits(), DEFAULT_SEARCH_DEPTH).unwrap_err();
        assert!(matches!(err, Error::UnsupportedSource { .. }), "{err:?}");
    }

    #[test]
    fn refuses_a_directory_with_no_skills() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("README.md"), "# not a skill").unwrap();
        let err = scan(tmp.path(), &limits(), DEFAULT_SEARCH_DEPTH).unwrap_err();
        assert!(matches!(err, Error::NotAPackage(_)), "{err:?}");
    }

    #[test]
    fn skips_vcs_and_build_directories() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(&tmp.path().join("real"), "real");
        write_skill(&tmp.path().join(".git/hooks/fake"), "fake");
        write_skill(&tmp.path().join("node_modules/pkg/fake2"), "fake2");

        let result = scan(tmp.path(), &limits(), DEFAULT_SEARCH_DEPTH).unwrap();
        let names: Vec<&str> = result.candidates.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["real"]);
    }

    #[test]
    fn two_skills_claiming_one_name_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(&tmp.path().join("dir-a"), "same-name");
        write_skill(&tmp.path().join("dir-b"), "same-name");
        let err = scan(tmp.path(), &limits(), DEFAULT_SEARCH_DEPTH).unwrap_err();
        assert!(matches!(err, Error::NameCollision { .. }), "{err:?}");
    }

    #[test]
    fn falls_back_to_the_directory_name_when_frontmatter_has_none() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("from-dir");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("SKILL.md"), "---\ndescription: d\n---\n").unwrap();

        let result = scan(&root, &limits(), DEFAULT_SEARCH_DEPTH).unwrap();
        let c = &result.candidates[0];
        assert_eq!(c.name.as_str(), "from-dir");
        assert_eq!(c.declared_name, None);
        // Installable overall, but Gemini is flagged as a confirmed problem.
        assert!(c.is_installable());
        assert!(c
            .findings
            .iter()
            .any(|f| f.agent.as_deref() == Some("gemini") && f.confirmed));
    }

    #[test]
    fn select_reports_available_names_when_one_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(&tmp.path().join("alpha"), "alpha");
        write_skill(&tmp.path().join("beta"), "beta");
        let result = scan(tmp.path(), &limits(), DEFAULT_SEARCH_DEPTH).unwrap();

        assert_eq!(result.select(&["beta".into()]).unwrap().len(), 1);

        let err = result.select(&["gamma".into()]).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("alpha, beta"), "must list options: {text}");
    }

    #[test]
    fn does_not_follow_links_while_surveying() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        write_skill(&outside.join("sneaky"), "sneaky");
        let root = tmp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        write_skill(&root.join("real"), "real");

        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&outside, root.join("link")).unwrap();

        let result = scan(&root, &limits(), DEFAULT_SEARCH_DEPTH).unwrap();
        let names: Vec<&str> = result.candidates.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["real"],
            "a link must not pull in outside skills"
        );
    }
}
