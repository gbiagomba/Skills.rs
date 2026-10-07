//! Gemini CLI adapter.
//!
//! Facts from <https://geminicli.com/docs/cli/skills/>, retrieved 2026-10-07 and
//! confirmed against the `google-gemini/gemini-cli` repository.
//!
//! Discovery tiers, lowest to highest precedence:
//!
//! 1. built-in (inside the installed package, not user writable)
//! 2. extension-bundled
//! 3. `~/.gemini/skills/`
//! 4. `~/.agents/skills/`
//! 5. `<project>/.gemini/skills/`
//! 6. `<project>/.agents/skills/`
//!
//! A higher tier wins by name, and Gemini emits a conflict warning. Within a tier
//! the `.agents/skills/` alias outranks the agent-specific directory.
//!
//! Two behaviours matter enough to be surfaced to the operator:
//!
//! * **Discovery is one level deep.** The glob is `SKILL.md` and `*/SKILL.md`, so
//!   a skill nested deeper than `<root>/<name>/SKILL.md` is never found.
//! * **Workspace skills are skipped entirely in an untrusted folder**, and trust
//!   defaults to untrusted. A project-scope deployment can therefore be written
//!   correctly and still not load, so it is reported as written rather than active.
//!
//! `GEMINI_CLI_HOME` relocates both user roots.

use std::path::{Path, PathBuf};

use super::{
    Agent, Capabilities, Caveat, Detection, Host, NameConflict, Provenance, Scope, SkillRoot,
    SymlinkSupport,
};

/// Gemini CLI.
#[derive(Debug, Default, Clone, Copy)]
pub struct GeminiCli;

/// The base directory Gemini resolves its user roots against.
///
/// `GEMINI_CLI_HOME` replaces the home directory outright, which is exactly how
/// the Gemini source behaves.
fn gemini_base(host: &Host) -> PathBuf {
    match host.env("GEMINI_CLI_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => host.home().to_path_buf(),
    }
}

impl Agent for GeminiCli {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["gemini-cli", "google-gemini"]
    }

    fn display_name(&self) -> &'static str {
        "Gemini CLI"
    }

    fn detect(&self, host: &Host) -> Detection {
        let mut evidence = Vec::new();
        let executable = host.which("gemini");
        match &executable {
            Some(path) => evidence.push(format!("`gemini` executable at {}", path.display())),
            None => evidence.push("no `gemini` executable on PATH".to_string()),
        }

        let base = gemini_base(host);
        let dir = base.join(".gemini");
        let config_dir = if dir.is_dir() {
            if host.env("GEMINI_CLI_HOME").is_some() {
                evidence.push(format!("GEMINI_CLI_HOME directory {}", dir.display()));
            } else {
                evidence.push(format!("config directory {}", dir.display()));
            }
            Some(dir)
        } else {
            None
        };

        if executable.is_none() && config_dir.is_some() {
            evidence.push(
                "directory present but executable absent, so Gemini CLI may have been removed"
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
        let base = gemini_base(host);

        match scope {
            Scope::User => {
                // Tier 3: agent-specific. Preferred write target precisely
                // because it is not shared.
                roots.push(SkillRoot {
                    path: base.join(".gemini").join("skills"),
                    scope: Scope::User,
                    precedence: 30,
                    writable: true,
                    shared_with: Vec::new(),
                    default_provenance: Provenance::Unmanaged,
                    label: "user (.gemini/skills)".to_string(),
                });

                // Tier 4: the shared alias, which outranks tier 3 for Gemini.
                // Discoverable and writable, but only chosen when the operator
                // asks, since it leaks visibility to Codex.
                roots.push(SkillRoot {
                    path: base.join(".agents").join("skills"),
                    scope: Scope::User,
                    precedence: 40,
                    writable: true,
                    shared_with: vec!["codex"],
                    default_provenance: Provenance::Unmanaged,
                    label: "user (shared .agents alias, outranks .gemini)".to_string(),
                });
            }
            Scope::Project => {
                if let Some(project) = host.project() {
                    roots.push(SkillRoot {
                        path: project.join(".gemini").join("skills"),
                        scope: Scope::Project,
                        precedence: 50,
                        writable: true,
                        shared_with: Vec::new(),
                        default_provenance: Provenance::Unmanaged,
                        label: "workspace (.gemini/skills)".to_string(),
                    });
                    roots.push(SkillRoot {
                        path: project.join(".agents").join("skills"),
                        scope: Scope::Project,
                        precedence: 60,
                        writable: true,
                        shared_with: vec!["codex"],
                        default_provenance: Provenance::Unmanaged,
                        label: "workspace (shared .agents alias)".to_string(),
                    });
                }
            }
        }

        roots
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // Gemini ships `gemini skills link`, which uses a junction on Windows.
            symlink: SymlinkSupport::Documented,
            project_scope: true,
            // `~/.gemini/skills` is Gemini's own, so isolation is available.
            isolated_user_root: true,
            // The loader glob is `SKILL.md` and `*/SKILL.md`, nothing deeper.
            max_discovery_depth: Some(1),
            name_conflict: NameConflict::Shadows,
            caveats: vec![
                Caveat {
                    code: "gemini.untrusted_workspace",
                    message: "Gemini CLI skips workspace skills entirely in an untrusted folder, \
                              and trust defaults to untrusted. A project-scope install can be \
                              written correctly and still never load until the folder is trusted."
                        .to_string(),
                    confirmed: true,
                },
                Caveat {
                    code: "gemini.shallow_discovery",
                    message: "Gemini CLI only looks for SKILL.md and */SKILL.md, so a package \
                              nested deeper than one level below the skills root is never found"
                        .to_string(),
                    confirmed: true,
                },
                Caveat {
                    code: "gemini.agents_alias_outranks",
                    message: "within a tier, ~/.agents/skills outranks ~/.gemini/skills, so a \
                              copy in the shared directory shadows the agent-specific one"
                        .to_string(),
                    confirmed: true,
                },
                Caveat {
                    code: "gemini.name_rewriting",
                    message: "Gemini CLI rewrites the characters : \\ / < > * ? \" | in a skill \
                              name to '-', so a skill can load under a name the operator did not \
                              choose"
                        .to_string(),
                    confirmed: true,
                },
                Caveat {
                    code: "gemini.own_installer",
                    message: "Gemini ships `gemini skills install|link`, so skills it installed \
                              appear to skill as unmanaged and need explicit adoption"
                        .to_string(),
                    confirmed: true,
                },
            ],
        }
    }

    fn classify(&self, host: &Host, path: &Path) -> Provenance {
        let base = gemini_base(host);

        // Built-ins live inside the installed npm package, never under the home
        // directory, so a path containing the package layout is bundled content.
        if path.components().any(|c| {
            let s = c.as_os_str();
            s == "node_modules" || s == "builtin"
        }) {
            return Provenance::BuiltIn;
        }
        if path.components().any(|c| c.as_os_str() == "extensions") {
            return Provenance::PluginManaged;
        }

        if path.starts_with(base.join(".gemini").join("skills"))
            || path.starts_with(base.join(".agents").join("skills"))
        {
            return Provenance::Unmanaged;
        }
        if let Some(project) = host.project() {
            if path.starts_with(project.join(".gemini").join("skills"))
                || path.starts_with(project.join(".agents").join("skills"))
            {
                return Provenance::Unmanaged;
            }
        }
        Provenance::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_the_agent_specific_root_over_the_shared_alias() {
        let tmp = tempfile::tempdir().unwrap();
        let h = Host::for_test(tmp.path());
        let root = GeminiCli.write_root(&h, Scope::User).unwrap();

        assert_eq!(
            root.path,
            tmp.path().join(".gemini/skills"),
            "isolation is available for Gemini, so it must be chosen by default"
        );
        assert!(!root.is_shared());
    }

    #[test]
    fn the_shared_alias_outranks_the_specific_root_in_gemini_precedence() {
        // Gemini's documented tier order, which is why a stray copy in
        // ~/.agents/skills can shadow the one we installed.
        let tmp = tempfile::tempdir().unwrap();
        let h = Host::for_test(tmp.path());
        let roots = GeminiCli.roots(&h, Scope::User);

        let specific = roots
            .iter()
            .find(|r| r.path.ends_with(".gemini/skills"))
            .unwrap();
        let shared = roots
            .iter()
            .find(|r| r.path.ends_with(".agents/skills"))
            .unwrap();
        assert!(shared.precedence > specific.precedence);
    }

    #[test]
    fn declares_its_shallow_discovery_depth() {
        assert_eq!(GeminiCli.capabilities().max_discovery_depth, Some(1));
    }

    #[test]
    fn surfaces_the_untrusted_workspace_caveat() {
        let caveat = GeminiCli
            .capabilities()
            .caveats
            .into_iter()
            .find(|c| c.code == "gemini.untrusted_workspace")
            .expect("the trust caveat must be present");
        assert!(caveat.confirmed);
        assert!(caveat.message.contains("never load"));
    }

    #[test]
    fn gemini_cli_home_relocates_both_user_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        let h = Host::for_test(tmp.path()).with_env("GEMINI_CLI_HOME", elsewhere.to_string_lossy());

        let roots = GeminiCli.roots(&h, Scope::User);
        assert!(roots
            .iter()
            .any(|r| r.path == elsewhere.join(".gemini/skills")));
        assert!(roots
            .iter()
            .any(|r| r.path == elsewhere.join(".agents/skills")));
        assert!(
            !roots
                .iter()
                .any(|r| r.path.starts_with(tmp.path().join(".gemini"))),
            "the default home must not still be used once GEMINI_CLI_HOME is set"
        );
    }

    #[test]
    fn classifies_builtin_and_extension_skills_as_untouchable() {
        let tmp = tempfile::tempdir().unwrap();
        let h = Host::for_test(tmp.path());

        assert_eq!(
            GeminiCli.classify(
                &h,
                Path::new("/usr/lib/node_modules/@google/gemini-cli/builtin/pr-creator")
            ),
            Provenance::BuiltIn
        );
        assert_eq!(
            GeminiCli.classify(&h, &tmp.path().join(".gemini/extensions/ext/skills/x")),
            Provenance::PluginManaged
        );
        assert_eq!(
            GeminiCli.classify(&h, &tmp.path().join(".gemini/skills/mine")),
            Provenance::Unmanaged
        );
    }
}
