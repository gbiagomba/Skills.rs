//! Claude Code adapter.
//!
//! Facts from <https://code.claude.com/docs/en/skills>, retrieved 2026-10-07.
//!
//! * Personal skills: `~/.claude/skills/<name>/SKILL.md`.
//! * Project skills: `.claude/skills/<name>/SKILL.md`, searched in the starting
//!   directory and every parent up to the repository root.
//! * Precedence: **enterprise over personal over project**. Note the direction:
//!   personal beats project, which is the opposite of the Agent Skills
//!   client-implementation guidance.
//! * Symlinked skill directories are documented as supported, and a skill is
//!   loaded once even when several locations point at the same target.
//! * No environment variable or `settings.json` key relocates the skills
//!   directory. `--add-dir` only adds a location for one session, and
//!   `permissions.additionalDirectories` explicitly does not load skills.

use std::path::{Path, PathBuf};

use super::{
    Agent, Capabilities, Caveat, Detection, Host, NameConflict, Provenance, Scope, SkillRoot,
    SymlinkSupport,
};

/// Claude Code.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeCode;

/// Directory inside `~/.claude/skills` that holds claude.ai-synced skills.
const SYNCED_DIR: &str = "synced";
/// Where Claude Code moves synced skills when sync is turned off.
const TRASH_DIR: &str = ".trash";

/// Managed-settings locations that hold organisation-deployed skills.
///
/// The documentation gives `/etc/claude-code/` as the Linux example; the macOS
/// and Windows equivalents follow each platform's managed-configuration
/// convention. These are read-only to us on every platform.
fn managed_roots() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        vec![
            PathBuf::from("/Library/Application Support/ClaudeCode/.claude/skills"),
            PathBuf::from("/etc/claude-code/.claude/skills"),
        ]
    } else if cfg!(windows) {
        vec![PathBuf::from(r"C:\ProgramData\ClaudeCode\.claude\skills")]
    } else {
        vec![PathBuf::from("/etc/claude-code/.claude/skills")]
    }
}

impl Agent for ClaudeCode {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["claude-code", "claudecode"]
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn detect(&self, host: &Host) -> Detection {
        let mut evidence = Vec::new();
        let executable = host.which("claude");
        match &executable {
            Some(path) => evidence.push(format!("`claude` executable at {}", path.display())),
            None => evidence.push("no `claude` executable on PATH".to_string()),
        }

        let config_dir = {
            let dir = host.home().join(".claude");
            if dir.is_dir() {
                evidence.push(format!("config directory {}", dir.display()));
                Some(dir)
            } else {
                None
            }
        };

        // A skills directory on its own is not proof of installation; say so
        // plainly rather than letting a leftover directory imply the agent exists.
        if executable.is_none() && config_dir.is_some() {
            evidence.push(
                "directory present but executable absent, so Claude Code may have been removed"
                    .to_string(),
            );
        }

        Detection {
            installed: executable.is_some(),
            executable,
            config_dir,
            evidence,
        }
    }

    fn roots(&self, host: &Host, scope: Scope) -> Vec<SkillRoot> {
        let mut roots = Vec::new();

        match scope {
            Scope::User => {
                // Precedence 30: enterprise, highest and never writable.
                for managed in managed_roots() {
                    roots.push(SkillRoot {
                        path: managed.clone(),
                        scope: Scope::User,
                        precedence: 30,
                        writable: false,
                        agent_specific: true,
                        default_provenance: Provenance::OrgManaged,
                        label: "enterprise (managed settings)".to_string(),
                    });
                }

                let personal = host.home().join(".claude").join("skills");

                // Precedence 20: personal, the only writable user root and the
                // one Claude Code prefers over a project skill.
                roots.push(SkillRoot {
                    path: personal.clone(),
                    scope: Scope::User,
                    precedence: 20,
                    writable: true,
                    agent_specific: true,
                    default_provenance: Provenance::Unmanaged,
                    label: "personal".to_string(),
                });

                // Account-synced and bookkeeping directories live inside the
                // personal root, so they are listed explicitly to keep them from
                // being treated as ordinary personal skills.
                roots.push(SkillRoot {
                    path: personal.join(SYNCED_DIR),
                    scope: Scope::User,
                    precedence: 20,
                    writable: false,
                    agent_specific: true,
                    default_provenance: Provenance::AccountSynced,
                    label: "claude.ai account sync".to_string(),
                });
                roots.push(SkillRoot {
                    path: personal.join(TRASH_DIR),
                    scope: Scope::User,
                    precedence: 0,
                    writable: false,
                    agent_specific: true,
                    default_provenance: Provenance::Ignored,
                    label: "sync trash".to_string(),
                });
            }
            Scope::Project => {
                if let Some(project) = host.project() {
                    roots.push(SkillRoot {
                        path: project.join(".claude").join("skills"),
                        scope: Scope::Project,
                        precedence: 10,
                        writable: true,
                        agent_specific: true,
                        default_provenance: Provenance::Unmanaged,
                        label: "project".to_string(),
                    });
                }
            }
        }

        roots
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            symlink: SymlinkSupport::Documented,
            project_scope: true,
            // `~/.claude/skills` is Claude Code's alone.
            isolated_user_root: true,
            max_discovery_depth: None,
            name_conflict: NameConflict::Shadows,
            caveats: vec![
                Caveat {
                    code: "claude.personal_beats_project",
                    message: "Claude Code resolves a same-named skill as enterprise over \
                              personal over project, so a personal install shadows a project one"
                        .to_string(),
                    confirmed: true,
                },
                Caveat {
                    code: "claude.no_path_override",
                    message: "Claude Code has no setting or environment variable that relocates \
                              its skills directory; --add-dir adds a location for one session only"
                        .to_string(),
                    confirmed: true,
                },
                Caveat {
                    code: "claude.spec_upload_strictness",
                    message: "Claude Code ignores unknown frontmatter fields, but claude.ai \
                              upload and package_skill.py reject them, so a skill that loads \
                              locally may still fail to package"
                        .to_string(),
                    confirmed: true,
                },
            ],
        }
    }

    fn classify(&self, host: &Host, path: &Path) -> Provenance {
        let personal = host.home().join(".claude").join("skills");

        if path.starts_with(personal.join(TRASH_DIR)) {
            return Provenance::Ignored;
        }
        if path.starts_with(personal.join(SYNCED_DIR)) {
            return Provenance::AccountSynced;
        }
        for managed in managed_roots() {
            if path.starts_with(&managed) {
                return Provenance::OrgManaged;
            }
        }

        // A skill folder carrying a plugin manifest is plugin-managed even when it
        // sits in the personal directory, and a `plugins/` ancestor is the usual
        // case. Both are documented, so both are checked.
        if path.join(".claude-plugin").join("plugin.json").is_file() {
            return Provenance::PluginManaged;
        }
        if path
            .components()
            .any(|c| c.as_os_str() == "plugins" || c.as_os_str() == "marketplaces")
        {
            return Provenance::PluginManaged;
        }

        if path.starts_with(&personal) {
            return Provenance::Unmanaged;
        }
        if let Some(project) = host.project() {
            if path.starts_with(project.join(".claude").join("skills")) {
                return Provenance::Unmanaged;
            }
        }

        Provenance::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn host(tmp: &Path) -> Host {
        Host::for_test(tmp).with_project(Some(tmp.join("proj")))
    }

    #[test]
    fn personal_root_is_the_write_target_and_is_isolated() {
        let tmp = tempfile::tempdir().unwrap();
        let h = host(tmp.path());
        let root = ClaudeCode.write_root(&h, Scope::User).unwrap();
        assert_eq!(root.path, tmp.path().join(".claude/skills"));
        assert!(root.writable);
        assert!(
            !root.is_shared_convention(),
            "Claude Code's user root is its own, not a shared convention directory"
        );
        assert!(ClaudeCode.capabilities().isolated_user_root);
    }

    #[test]
    fn enterprise_outranks_personal_which_outranks_project() {
        let tmp = tempfile::tempdir().unwrap();
        let h = host(tmp.path());
        let user = ClaudeCode.roots(&h, Scope::User);
        let project = ClaudeCode.roots(&h, Scope::Project);

        let enterprise = user
            .iter()
            .find(|r| r.default_provenance == Provenance::OrgManaged)
            .unwrap();
        let personal = user.iter().find(|r| r.label == "personal").unwrap();
        let proj = project.first().unwrap();

        assert!(enterprise.precedence > personal.precedence);
        assert!(
            personal.precedence > proj.precedence,
            "Claude Code documents personal over project, not the other way round"
        );
    }

    #[test]
    fn managed_and_synced_roots_are_never_writable() {
        let tmp = tempfile::tempdir().unwrap();
        let h = host(tmp.path());
        for root in ClaudeCode.roots(&h, Scope::User) {
            if root.default_provenance != Provenance::Unmanaged {
                assert!(!root.writable, "{} must not be writable", root.label);
            }
        }
    }

    #[test]
    fn classifies_the_documented_special_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let h = host(tmp.path());
        let skills = tmp.path().join(".claude/skills");

        assert_eq!(
            ClaudeCode.classify(&h, &skills.join("mine")),
            Provenance::Unmanaged
        );
        assert_eq!(
            ClaudeCode.classify(&h, &skills.join("synced/from-account")),
            Provenance::AccountSynced
        );
        assert_eq!(
            ClaudeCode.classify(&h, &skills.join(".trash/old")),
            Provenance::Ignored
        );
        // Ask the platform for its managed-settings location rather than
        // hardcoding the Linux one: on Windows this lives under ProgramData, and
        // a hardcoded POSIX path silently stops testing anything there.
        let managed = managed_roots()
            .into_iter()
            .next()
            .expect("every platform declares a managed root");
        assert_eq!(
            ClaudeCode.classify(&h, &managed.join("corp")),
            Provenance::OrgManaged
        );
        assert_eq!(
            ClaudeCode.classify(&h, Path::new("/somewhere/else")),
            Provenance::Unknown,
            "unknown provenance must stay unknown, not default to personal"
        );
    }

    #[test]
    fn a_plugin_manifest_makes_a_skill_folder_plugin_managed() {
        let tmp = tempfile::tempdir().unwrap();
        let h = host(tmp.path());
        let folder = tmp.path().join(".claude/skills/looks-personal");
        fs::create_dir_all(folder.join(".claude-plugin")).unwrap();
        fs::write(folder.join(".claude-plugin/plugin.json"), "{}").unwrap();

        assert_eq!(
            ClaudeCode.classify(&h, &folder),
            Provenance::PluginManaged,
            "a plugin manifest wins over its location"
        );
    }

    #[test]
    fn detection_separates_executable_from_leftover_directory() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join(".claude/skills")).unwrap();
        let h = Host::for_test(tmp.path());

        let d = ClaudeCode.detect(&h);
        assert!(
            !d.installed,
            "a directory alone is not proof of installation"
        );
        assert!(d.config_dir.is_some());
        assert!(
            d.summary().contains("executable absent"),
            "evidence must say what was actually seen: {}",
            d.summary()
        );
    }
}
