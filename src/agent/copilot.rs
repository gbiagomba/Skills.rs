//! GitHub Copilot CLI adapter.
//!
//! Facts from the Copilot CLI command reference
//! (<https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference>,
//! section "Skill locations") and the config-directory reference, retrieved
//! 2026-10-07. GitHub states explicitly that this follows the Agent Skills open
//! standard.
//!
//! This is the **standalone `copilot` CLI** (npm `@github/copilot`). It is *not*
//! the deprecated `gh copilot` extension, which was retired on 2025-10-25 and
//! never supported skills at all.
//!
//! Documented skill locations, in Copilot's own priority order, first found
//! winning for a duplicate name:
//!
//! | Location | Scope |
//! |---|---|
//! | `.github/skills/` | project |
//! | `.agents/skills/` | project |
//! | `.claude/skills/` | project |
//! | parent `.github/skills/` | inherited (monorepo) |
//! | `~/.copilot/skills/` | personal |
//! | `~/.agents/skills/` | personal |
//! | plugin directories | plugin |
//! | `COPILOT_SKILLS_DIRS` | custom |
//! | bundled | built-in, lowest |
//! | org/enterprise relay | remote, no local file |
//!
//! Two of those make Copilot unusually entangled with the other agents, and both
//! are disclosed rather than hidden:
//!
//! * It reads **both** `.agents/skills` levels, joining Codex and Gemini CLI on
//!   the shared convention directory.
//! * It reads project **`.claude/skills/`**, so a Claude Code project
//!   installation is visible to Copilot too.

use std::path::{Path, PathBuf};

use super::{
    Agent, Capabilities, Caveat, Detection, Host, NameConflict, Provenance, Scope, SkillRoot,
    SymlinkSupport,
};

/// GitHub Copilot CLI.
#[derive(Debug, Default, Clone, Copy)]
pub struct GitHubCopilot;

/// Resolve the Copilot config root, which `COPILOT_HOME` can relocate.
fn copilot_home(host: &Host) -> PathBuf {
    match host.env("COPILOT_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => host.home().join(".copilot"),
    }
}

impl Agent for GitHubCopilot {
    fn id(&self) -> &'static str {
        "copilot"
    }

    fn aliases(&self) -> &'static [&'static str] {
        // `gh-copilot` is deliberately absent: that names the retired extension,
        // and accepting it would imply this adapter manages something it does not.
        &["github-copilot", "copilot-cli"]
    }

    fn display_name(&self) -> &'static str {
        "GitHub Copilot CLI"
    }

    fn detect(&self, host: &Host) -> Detection {
        let mut evidence = Vec::new();
        let executable = host.which("copilot");
        match &executable {
            Some(path) => evidence.push(format!("`copilot` executable at {}", path.display())),
            None => evidence.push("no `copilot` executable on PATH".to_string()),
        }

        let home = copilot_home(host);
        let config_dir = if home.is_dir() {
            if host.env("COPILOT_HOME").is_some() {
                evidence.push(format!("COPILOT_HOME directory {}", home.display()));
            } else {
                evidence.push(format!("config directory {}", home.display()));
            }
            Some(home)
        } else {
            None
        };

        if executable.is_none() && config_dir.is_some() {
            evidence.push(
                "directory present but executable absent, so Copilot CLI may have been removed"
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
        let home = copilot_home(host);

        match scope {
            Scope::User => {
                // Personal, agent-specific. Preferred write target precisely
                // because it is the one Copilot owns by name.
                roots.push(SkillRoot {
                    path: home.join("skills"),
                    scope: Scope::User,
                    precedence: 20,
                    writable: true,
                    agent_specific: true,
                    default_provenance: Provenance::Unmanaged,
                    label: "personal (~/.copilot/skills)".to_string(),
                });

                // Personal shared convention, read since CLI 1.0.11. Lower
                // priority than ~/.copilot/skills in Copilot's own order.
                roots.push(SkillRoot {
                    path: host.home().join(".agents").join("skills"),
                    scope: Scope::User,
                    precedence: 15,
                    writable: true,
                    agent_specific: false,
                    default_provenance: Provenance::Unmanaged,
                    label: "personal (shared .agents convention)".to_string(),
                });

                // Plugin-provided skills. Never ours.
                roots.push(SkillRoot {
                    path: home.join("installed-plugins"),
                    scope: Scope::User,
                    precedence: 10,
                    writable: false,
                    agent_specific: true,
                    default_provenance: Provenance::PluginManaged,
                    label: "plugin-provided".to_string(),
                });
            }
            Scope::Project => {
                if let Some(project) = host.project() {
                    // Highest-priority project location, and the one Copilot
                    // documents first, so it is the write target.
                    roots.push(SkillRoot {
                        path: project.join(".github").join("skills"),
                        scope: Scope::Project,
                        precedence: 40,
                        writable: true,
                        agent_specific: true,
                        label: "project (.github/skills)".to_string(),
                        default_provenance: Provenance::Unmanaged,
                    });
                    roots.push(SkillRoot {
                        path: project.join(".agents").join("skills"),
                        scope: Scope::Project,
                        precedence: 30,
                        writable: true,
                        agent_specific: false,
                        default_provenance: Provenance::Unmanaged,
                        label: "project (shared .agents convention)".to_string(),
                    });
                    // Copilot reads Claude Code's project directory. Discovered
                    // and disclosed, never written: a skill placed here would
                    // appear to be Claude's.
                    roots.push(SkillRoot {
                        path: project.join(".claude").join("skills"),
                        scope: Scope::Project,
                        precedence: 20,
                        writable: false,
                        agent_specific: false,
                        default_provenance: Provenance::Unmanaged,
                        label: "project (reads Claude Code's .claude/skills)".to_string(),
                    });
                }
            }
        }

        roots
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // Changelog 1.0.62 says symlinked skill directories load, but no
            // GitHub documentation covers it and GitHub's own issue #3264 asks
            // for Windows behaviour to be documented. So it is not promised.
            symlink: SymlinkSupport::Undocumented,
            project_scope: true,
            // `~/.copilot/skills` is Copilot's own.
            isolated_user_root: true,
            max_discovery_depth: None,
            name_conflict: NameConflict::Shadows,
            caveats: vec![
                Caveat {
                    code: "copilot.reads_claude_project_skills",
                    message: "Copilot CLI also reads a project's .claude/skills directory, so a \
                              Claude Code project install is visible to Copilot as well"
                        .to_string(),
                    confirmed: true,
                },
                Caveat {
                    code: "copilot.reads_shared_agents_root",
                    message: "Copilot CLI reads both .agents/skills and ~/.agents/skills, so it \
                              shares those directories with Codex and Gemini CLI"
                        .to_string(),
                    confirmed: true,
                },
                Caveat {
                    code: "copilot.symlinks_undocumented",
                    message: "symlinked skill directories load in Copilot CLI 1.0.62 and later \
                              per its changelog, but GitHub does not document the behaviour and \
                              has an open issue about it on Windows, so `skill link` is not \
                              promised to work here"
                        .to_string(),
                    confirmed: true,
                },
                Caveat {
                    code: "copilot.copilot_home_drops_shared_root",
                    message: "with COPILOT_HOME or --config-dir set, Copilot CLI stops reading \
                              ~/.agents/skills, so a skill installed there becomes invisible to \
                              those sessions"
                        .to_string(),
                    confirmed: true,
                },
                Caveat {
                    code: "copilot.gh_skill_provenance",
                    message: "`gh skill` writes provenance metadata into a skill's SKILL.md \
                              frontmatter and can pin it. skill preserves unknown frontmatter \
                              keys, so that metadata survives, but re-homing such a skill will \
                              break `gh skill update`"
                        .to_string(),
                    confirmed: true,
                },
                Caveat {
                    code: "copilot.org_skills_have_no_local_path",
                    message: "organization and enterprise skills are fetched on demand through \
                              GitHub's relay and have no local file, so they cannot be managed \
                              locally at all"
                        .to_string(),
                    confirmed: true,
                },
            ],
        }
    }

    fn classify(&self, host: &Host, path: &Path) -> Provenance {
        let home = copilot_home(host);

        // Plugin-provided skills are managed by the plugin lifecycle. Copilot
        // documents that they cannot be removed as ordinary skills.
        if path.starts_with(home.join("installed-plugins"))
            || path
                .components()
                .any(|c| c.as_os_str() == "installed-plugins")
        {
            return Provenance::PluginManaged;
        }

        if path.starts_with(home.join("skills"))
            || path.starts_with(host.home().join(".agents").join("skills"))
        {
            return Provenance::Unmanaged;
        }

        if let Some(project) = host.project() {
            for candidate in [".github", ".agents", ".claude"] {
                if path.starts_with(project.join(candidate).join("skills")) {
                    return Provenance::Unmanaged;
                }
            }
        }

        Provenance::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_write_target_is_the_agent_specific_root() {
        let tmp = tempfile::tempdir().unwrap();
        let h = Host::for_test(tmp.path());
        let root = GitHubCopilot.write_root(&h, Scope::User).unwrap();

        assert_eq!(root.path, tmp.path().join(".copilot/skills"));
        assert!(
            !root.is_shared_convention(),
            "isolation is available for Copilot, so it must be preferred"
        );
        assert!(GitHubCopilot.capabilities().isolated_user_root);
    }

    #[test]
    fn project_write_target_is_dot_github_skills() {
        // Copilot documents .github/skills first and highest, so that is where a
        // project install belongs.
        let tmp = tempfile::tempdir().unwrap();
        let h = Host::for_test(tmp.path()).with_project(Some(tmp.path().join("proj")));
        let root = GitHubCopilot.write_root(&h, Scope::Project).unwrap();
        assert_eq!(root.path, tmp.path().join("proj/.github/skills"));
    }

    #[test]
    fn reads_claude_project_skills_but_never_writes_there() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        let h = Host::for_test(tmp.path()).with_project(Some(project.clone()));

        let claude_root = GitHubCopilot
            .roots(&h, Scope::Project)
            .into_iter()
            .find(|r| r.path == project.join(".claude/skills"))
            .expect("Copilot documents reading .claude/skills");

        assert!(
            !claude_root.writable,
            "writing into another agent's directory would misattribute the skill"
        );
    }

    #[test]
    fn copilot_home_relocates_the_personal_root() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tmp.path().join("custom-copilot");
        let h = Host::for_test(tmp.path()).with_env("COPILOT_HOME", elsewhere.to_string_lossy());

        let roots = GitHubCopilot.roots(&h, Scope::User);
        assert!(roots.iter().any(|r| r.path == elsewhere.join("skills")));
        // The shared .agents root is $HOME based and is not relocated by it.
        assert!(roots
            .iter()
            .any(|r| r.path == tmp.path().join(".agents/skills")));
    }

    #[test]
    fn symlink_support_is_reported_as_undocumented() {
        // The changelog says it works; GitHub does not document it. We must not
        // promise what the vendor has not.
        assert_eq!(
            GitHubCopilot.capabilities().symlink,
            SymlinkSupport::Undocumented
        );
    }

    #[test]
    fn does_not_accept_the_retired_gh_copilot_alias() {
        // `gh copilot` is a different, deprecated product with no skills support,
        // so claiming the alias would imply we manage something we do not.
        assert!(!GitHubCopilot.aliases().contains(&"gh-copilot"));
        assert!(GitHubCopilot.aliases().contains(&"github-copilot"));
    }

    #[test]
    fn classifies_plugin_and_relay_skills_as_untouchable() {
        let tmp = tempfile::tempdir().unwrap();
        let h = Host::for_test(tmp.path());

        assert_eq!(
            GitHubCopilot.classify(
                &h,
                &tmp.path().join(".copilot/installed-plugins/p/skills/x")
            ),
            Provenance::PluginManaged
        );
        assert!(!GitHubCopilot
            .classify(
                &h,
                &tmp.path().join(".copilot/installed-plugins/p/skills/x")
            )
            .is_adoptable());

        assert_eq!(
            GitHubCopilot.classify(&h, &tmp.path().join(".copilot/skills/mine")),
            Provenance::Unmanaged
        );
        assert_eq!(
            GitHubCopilot.classify(&h, Path::new("/somewhere/else")),
            Provenance::Unknown
        );
    }

    #[test]
    fn surfaces_the_entanglement_caveats() {
        let caveats = GitHubCopilot.capabilities().caveats;
        for code in [
            "copilot.reads_claude_project_skills",
            "copilot.reads_shared_agents_root",
            "copilot.org_skills_have_no_local_path",
        ] {
            let found = caveats
                .iter()
                .find(|c| c.code == code)
                .unwrap_or_else(|| panic!("{code} must be reported"));
            assert!(found.confirmed, "{code} is documented, not inferred");
        }
    }
}
