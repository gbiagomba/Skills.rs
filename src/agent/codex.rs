//! Codex adapter.
//!
//! Facts from <https://developers.openai.com/codex/skills/>, which redirects to
//! <https://learn.chatgpt.com/docs/build-skills>, retrieved 2026-10-07.
//!
//! The documented scopes are `REPO`, `USER`, `ADMIN`, and `SYSTEM`:
//!
//! * `USER` is **`$HOME/.agents/skills`**. This is the shared cross-client
//!   convention directory, not a Codex-specific one.
//! * `REPO` is `.agents/skills` at the working directory, at every ancestor, and
//!   at the repository root.
//! * `ADMIN` is `/etc/codex/skills`.
//! * `SYSTEM` is bundled by OpenAI and materialised at
//!   `$CODEX_HOME/skills/.system`, which is a managed cache rather than an install.
//!
//! `~/.codex/skills` still works but the Codex source marks it deprecated, so it
//! is read for discovery and never written: buying isolation there would mean
//! depending on a path the vendor has already called deprecated.
//!
//! Codex does **not** resolve same-name conflicts. The documentation states that
//! both skills can appear in the picker, so a collision here is a duplicate
//! warning rather than a silent override.

use std::path::{Path, PathBuf};

use super::{
    Agent, Capabilities, Caveat, Detection, Host, NameConflict, Provenance, Scope, SkillRoot,
    SymlinkSupport,
};

/// Codex.
#[derive(Debug, Default, Clone, Copy)]
pub struct Codex;

/// Resolve `CODEX_HOME`, which defaults to `~/.codex`.
fn codex_home(host: &Host) -> PathBuf {
    match host.env("CODEX_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => host.home().join(".codex"),
    }
}

/// The admin-managed root, which differs on Windows.
fn admin_root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\ProgramData\codex\skills")
    } else {
        PathBuf::from("/etc/codex/skills")
    }
}

impl Agent for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["openai-codex", "codex-cli"]
    }

    fn display_name(&self) -> &'static str {
        "Codex"
    }

    fn detect(&self, host: &Host) -> Detection {
        let mut evidence = Vec::new();
        let executable = host.which("codex");
        match &executable {
            Some(path) => evidence.push(format!("`codex` executable at {}", path.display())),
            None => evidence.push("no `codex` executable on PATH".to_string()),
        }

        let home = codex_home(host);
        let config_dir = if home.is_dir() {
            if host.env("CODEX_HOME").is_some() {
                evidence.push(format!("CODEX_HOME directory {}", home.display()));
            } else {
                evidence.push(format!("config directory {}", home.display()));
            }
            Some(home)
        } else {
            None
        };

        if executable.is_none() && config_dir.is_some() {
            evidence.push(
                "directory present but executable absent, so Codex may have been removed"
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
        let home = codex_home(host);

        match scope {
            Scope::User => {
                // SYSTEM: a managed cache that Codex wipes and rewrites itself.
                roots.push(SkillRoot {
                    path: home.join("skills").join(".system"),
                    scope: Scope::User,
                    precedence: 5,
                    writable: false,
                    agent_specific: true,
                    default_provenance: Provenance::BuiltIn,
                    label: "bundled by OpenAI (managed cache)".to_string(),
                });

                // ADMIN.
                roots.push(SkillRoot {
                    path: admin_root(),
                    scope: Scope::User,
                    precedence: 25,
                    writable: false,
                    agent_specific: true,
                    default_provenance: Provenance::OrgManaged,
                    label: "admin (machine-wide)".to_string(),
                });

                // USER: the documented root, which is the shared convention
                // directory. Gemini CLI reads it too, so a deployment here is
                // visible to Gemini whether or not it was selected.
                roots.push(SkillRoot {
                    path: host.home().join(".agents").join("skills"),
                    scope: Scope::User,
                    precedence: 20,
                    writable: true,
                    agent_specific: false,
                    default_provenance: Provenance::Unmanaged,
                    label: "user (shared .agents convention)".to_string(),
                });

                // Deprecated but still read. Discovery only.
                roots.push(SkillRoot {
                    path: home.join("skills"),
                    scope: Scope::User,
                    precedence: 15,
                    writable: false,
                    agent_specific: true,
                    default_provenance: Provenance::Unmanaged,
                    label: "user (deprecated $CODEX_HOME/skills)".to_string(),
                });
            }
            Scope::Project => {
                if let Some(project) = host.project() {
                    roots.push(SkillRoot {
                        path: project.join(".agents").join("skills"),
                        scope: Scope::Project,
                        precedence: 10,
                        writable: true,
                        agent_specific: false,
                        default_provenance: Provenance::Unmanaged,
                        label: "repo (shared .agents convention)".to_string(),
                    });
                    // Present in the Codex source but not in its documentation,
                    // so it is read and never written.
                    roots.push(SkillRoot {
                        path: project.join(".codex").join("skills"),
                        scope: Scope::Project,
                        precedence: 8,
                        writable: false,
                        agent_specific: true,
                        default_provenance: Provenance::Unmanaged,
                        label: "repo (.codex/skills, undocumented)".to_string(),
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
            // The only Codex-specific user root is documented as deprecated, so
            // there is no isolated place to install. This is why `skill`
            // discloses shared visibility instead of promising isolation.
            isolated_user_root: false,
            max_discovery_depth: None,
            name_conflict: NameConflict::KeepsBoth,
            caveats: vec![
                Caveat {
                    code: "codex.user_root_is_shared",
                    message: "Codex's documented user root is the shared ~/.agents/skills, so a \
                              user-scope install is also visible to Gemini CLI and to any other \
                              client using that convention. Codex offers no isolated alternative."
                        .to_string(),
                    confirmed: true,
                },
                Caveat {
                    code: "codex.keeps_both_on_conflict",
                    message: "Codex does not merge or shadow same-named skills; both appear in \
                              the picker, so a name collision is a duplicate rather than an \
                              override"
                        .to_string(),
                    confirmed: true,
                },
                Caveat {
                    code: "codex.sidecar_is_codex_only",
                    message: "an agents/openai.yaml sidecar is Codex-specific package content; it \
                              is preserved on migration but other agents ignore it"
                        .to_string(),
                    confirmed: true,
                },
                Caveat {
                    code: "codex.no_skills_subcommand",
                    message: "Codex has no `codex skills` subcommand, so a loaded skill can only \
                              be confirmed interactively with /skills"
                        .to_string(),
                    confirmed: true,
                },
            ],
        }
    }

    fn classify(&self, host: &Host, path: &Path) -> Provenance {
        let home = codex_home(host);

        if path.starts_with(home.join("skills").join(".system")) {
            return Provenance::BuiltIn;
        }
        if path.starts_with(admin_root()) {
            return Provenance::OrgManaged;
        }
        // Codex distributes reusable skills through plugins, which live under the
        // shared plugin directory rather than a skills root.
        if path.components().any(|c| c.as_os_str() == "plugins") {
            return Provenance::PluginManaged;
        }
        if path.starts_with(host.home().join(".agents").join("skills"))
            || path.starts_with(home.join("skills"))
        {
            return Provenance::Unmanaged;
        }
        if let Some(project) = host.project() {
            if path.starts_with(project.join(".agents").join("skills"))
                || path.starts_with(project.join(".codex").join("skills"))
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
    fn user_write_target_is_the_shared_agents_root() {
        let tmp = tempfile::tempdir().unwrap();
        let h = Host::for_test(tmp.path());
        let root = Codex.write_root(&h, Scope::User).unwrap();

        assert_eq!(root.path, tmp.path().join(".agents/skills"));
        assert!(
            root.is_shared_convention(),
            "the Codex user root is the shared convention directory"
        );
        // Which agents actually read it is a registry question; see
        // `registry::readers_of` and its tests.
    }

    #[test]
    fn codex_cannot_offer_an_isolated_user_root() {
        // This is the product constraint the whole shared-root disclosure exists
        // for. If it ever becomes true, the disclosure can be relaxed.
        assert!(!Codex.capabilities().isolated_user_root);
        assert!(Codex
            .capabilities()
            .caveats
            .iter()
            .any(|c| c.code == "codex.user_root_is_shared" && c.confirmed));
    }

    #[test]
    fn the_deprecated_root_is_read_but_never_written() {
        let tmp = tempfile::tempdir().unwrap();
        let h = Host::for_test(tmp.path());
        let deprecated = h.home().join(".codex/skills");

        let root = Codex
            .roots(&h, Scope::User)
            .into_iter()
            .find(|r| r.path == deprecated)
            .expect("the deprecated root must still be discovered");

        assert!(
            !root.writable,
            "writing to a deprecated path would silently break later"
        );
        assert!(root.label.contains("deprecated"));
    }

    #[test]
    fn codex_home_env_var_relocates_the_deprecated_and_system_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tmp.path().join("custom-codex");
        let h = Host::for_test(tmp.path()).with_env("CODEX_HOME", elsewhere.to_string_lossy());

        let roots = Codex.roots(&h, Scope::User);
        assert!(roots.iter().any(|r| r.path == elsewhere.join("skills")));
        assert!(roots
            .iter()
            .any(|r| r.path == elsewhere.join("skills/.system")));

        // The USER root is $HOME based and is not affected by CODEX_HOME.
        assert!(roots
            .iter()
            .any(|r| r.path == tmp.path().join(".agents/skills")));
    }

    #[test]
    fn keeps_both_on_a_name_conflict() {
        assert_eq!(Codex.capabilities().name_conflict, NameConflict::KeepsBoth);
    }

    #[test]
    fn classifies_bundled_and_admin_skills_as_untouchable() {
        let tmp = tempfile::tempdir().unwrap();
        let h = Host::for_test(tmp.path());

        assert_eq!(
            Codex.classify(&h, &tmp.path().join(".codex/skills/.system/plan")),
            Provenance::BuiltIn
        );
        assert_eq!(
            Codex.classify(&h, &admin_root().join("corp-skill")),
            Provenance::OrgManaged
        );
        assert_eq!(
            Codex.classify(&h, &tmp.path().join(".agents/skills/mine")),
            Provenance::Unmanaged
        );
        assert!(!Codex
            .classify(&h, &tmp.path().join(".codex/skills/.system/plan"))
            .is_adoptable());
    }
}
