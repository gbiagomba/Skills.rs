//! The command-line surface.
//!
//! Every command in the product contract is declared here, so `--help` documents
//! the whole interface. A command that this build does not implement yet fails
//! with [`crate::ExitCode::NotImplemented`] and a pointer to
//! `docs/checklist.md`, rather than returning a misleading success.
//!
//! Two selection flags exist and they mean different things, which is why there
//! is no single `--all`:
//!
//! * `--all-skills` selects **packages** inside a source.
//! * `--all-detected` selects **destinations**, meaning every detected agent.

pub mod commands;
pub mod render;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::exit::ExitCode;

/// Build the `--help` epilogue from the exit-code table itself, so the
/// documentation cannot drift away from the implementation.
fn exit_code_help() -> String {
    let mut out = String::from("Exit codes:\n");
    for code in ExitCode::ALL {
        out.push_str(&format!("  {:<3} {}\n", code.code(), code.describe()));
    }
    out.push_str(
        "\nSkill content is never executed. Acquired packages are treated as data.\n\
         See docs/security.md for the full threat model.",
    );
    out
}

/// A cross-agent skill manager.
#[derive(Debug, Parser)]
#[command(
    name = "skill",
    version,
    about = "A cross-agent skill manager with safe copying, linking, migration, updates, and synchronization.",
    long_about = "Install once, manage everywhere, never silently lose edits.\n\n\
                  skill discovers installed coding agents (Claude Code, Codex, Gemini CLI) and \
                  manages reusable Agent Skills across them. It manages local CLI skill \
                  installations only. It does not synchronise hosted accounts, and installing \
                  locally never changes a cloud account.",
    after_long_help = exit_code_help(),
    propagate_version = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    #[command(flatten)]
    pub global: GlobalArgs,
}

/// Options accepted by every subcommand.
#[derive(Debug, Args, Clone, Default)]
pub struct GlobalArgs {
    /// Canonical store location (default: ~/skills).
    #[arg(long, global = true, value_name = "DIR")]
    pub store: Option<PathBuf>,

    /// Configuration file to read.
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Show what would change without changing anything.
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Accept a fully determined plan without prompting.
    ///
    /// Does not authorize a conflict overwrite, plain HTTP, adoption of an
    /// unmanaged destination, or any deletion.
    #[arg(long, short = 'y', global = true)]
    pub yes: bool,

    /// Emit one schema-versioned JSON object on stdout.
    #[arg(long, global = true)]
    pub json: bool,

    /// Include per-file detail in human-readable output.
    #[arg(long, short = 'v', global = true)]
    pub verbose: bool,

    /// Make no network access at all.
    #[arg(long, global = true)]
    pub offline: bool,
}

/// Where a deployment goes.
#[derive(Debug, Args, Clone, Default)]
pub struct ScopeArgs {
    /// Install for this user, or inside a project directory.
    #[arg(long, value_name = "SCOPE", value_parser = ["user", "project"])]
    pub scope: Option<String>,

    /// The project directory, when --scope project is used.
    #[arg(long, value_name = "DIR")]
    pub project_dir: Option<PathBuf>,
}

/// Which packages, and which destinations.
#[derive(Debug, Args, Clone, Default)]
pub struct SelectionArgs {
    /// Select a package by name. Repeatable.
    #[arg(long = "skill", value_name = "NAME")]
    pub skills: Vec<String>,

    /// Select every package found in the source.
    #[arg(long)]
    pub all_skills: bool,

    /// Select every detected agent as a destination.
    #[arg(long)]
    pub all_detected: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List supported agents and the evidence that each is installed.
    Agents,

    /// List managed skills and where they are deployed.
    List {
        /// Show only skills deployed to this agent.
        #[arg(long, value_name = "AGENT")]
        agent: Option<String>,
    },

    /// Acquire a source into the store and deploy independent copies.
    Copy {
        /// A path, file:// URI, git address, http(s) URL, or smb:// URL.
        source: String,

        /// Destination agents. Omit to choose interactively.
        agents: Vec<String>,

        /// Install under a different name, to resolve a collision.
        #[arg(long = "as", value_name = "NAME")]
        install_as: Option<String>,

        /// Git branch, tag, or commit to acquire.
        #[arg(long, value_name = "REF")]
        r#ref: Option<String>,

        /// Permit plain HTTP. Never implied by --yes.
        #[arg(long)]
        allow_http: bool,

        #[command(flatten)]
        selection: SelectionArgs,

        #[command(flatten)]
        scope: ScopeArgs,
    },

    /// Acquire a source into the store and deploy per-skill symlinks.
    Link {
        source: String,
        agents: Vec<String>,

        #[arg(long = "as", value_name = "NAME")]
        install_as: Option<String>,

        #[arg(long, value_name = "REF")]
        r#ref: Option<String>,

        #[arg(long)]
        allow_http: bool,

        #[command(flatten)]
        selection: SelectionArgs,

        #[command(flatten)]
        scope: ScopeArgs,
    },

    /// Move selected installations from one agent to another.
    Migrate {
        /// Agent to move from.
        from: String,
        /// Agent to move to.
        to: String,

        #[command(flatten)]
        selection: SelectionArgs,

        #[command(flatten)]
        scope: ScopeArgs,
    },

    /// Write a portable bundle.
    Export {
        /// Skills to include. Omit with --all-skills.
        skills: Vec<String>,

        /// Bundle path to write.
        #[arg(long, short = 'o', value_name = "FILE")]
        output: PathBuf,

        #[arg(long)]
        all_skills: bool,
    },

    /// Restore a portable bundle, rebasing paths onto this machine.
    Import {
        /// The bundle to read.
        bundle: PathBuf,

        /// Destination agents.
        agents: Vec<String>,

        /// Deploy as independent copies or as symlinks.
        #[arg(long, value_name = "MODE", value_parser = ["copy", "link"])]
        mode: Option<String>,

        #[command(flatten)]
        selection: SelectionArgs,

        #[command(flatten)]
        scope: ScopeArgs,
    },

    /// Report the state of every managed skill. Read-only.
    Status {
        /// Report only this skill.
        skill: Option<String>,
    },

    /// Show how a skill differs between the store and its deployments.
    Diff {
        /// The skill to compare.
        skill: String,
    },

    /// Fetch upstream changes, protecting local edits.
    Update {
        /// Skills to update.
        skills: Vec<String>,

        /// Report whether upstream moved, without changing anything.
        #[arg(long)]
        check: bool,

        #[arg(long)]
        all_skills: bool,
    },

    /// Reconcile the store with its deployments. Does not fetch upstream.
    Sync {
        /// Skills to reconcile. Omit for all.
        skills: Vec<String>,

        /// Promote this agent's deployed content into the store.
        #[arg(long, value_name = "AGENT")]
        adopt_from: Option<String>,

        #[arg(long)]
        all_skills: bool,
    },

    /// Undo a recorded transaction from its backups.
    Rollback {
        /// The transaction id, as reported by `skill doctor`.
        transaction_id: String,
    },

    /// Check the installation, the store, and recoverable state. Read-only.
    Doctor,
}

impl Command {
    /// The name used in JSON output and error messages.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::List { .. } => "list",
            Self::Copy { .. } => "copy",
            Self::Link { .. } => "link",
            Self::Migrate { .. } => "migrate",
            Self::Export { .. } => "export",
            Self::Import { .. } => "import",
            Self::Status { .. } => "status",
            Self::Diff { .. } => "diff",
            Self::Update { .. } => "update",
            Self::Sync { .. } => "sync",
            Self::Rollback { .. } => "rollback",
            Self::Doctor => "doctor",
        }
    }

    /// True when the command may write to disk.
    pub fn mutates(&self) -> bool {
        !matches!(
            self,
            Self::Agents
                | Self::List { .. }
                | Self::Status { .. }
                | Self::Diff { .. }
                | Self::Doctor
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_every_documented_example() {
        // Straight from the product contract. If one of these stops parsing, the
        // documented interface has broken.
        let examples: &[&[&str]] = &[
            &["skill", "agents"],
            &["skill", "list"],
            &["skill", "list", "--agent", "claude"],
            &["skill", "copy", "./my-skill", "claude"],
            &["skill", "copy", "./my-skill", "claude", "codex", "gemini"],
            &[
                "skill",
                "copy",
                "file:///absolute/path/my-skill/SKILL.md",
                "codex",
            ],
            &[
                "skill",
                "copy",
                "./my-skills",
                "--all-detected",
                "--all-skills",
            ],
            &["skill", "link", "./my-skill", "--all-detected"],
            &[
                "skill",
                "copy",
                "https://github.com/owner/repo.git",
                "codex",
                "--skill",
                "my-skill",
            ],
            &[
                "skill",
                "copy",
                "git@github.com:owner/repo.git",
                "claude",
                "--skill",
                "my-skill",
                "--ref",
                "main",
            ],
            &[
                "skill",
                "copy",
                "https://example.com/my-skill.tar.gz",
                "--all-detected",
            ],
            &["skill", "copy", "smb://server/share/my-skill", "claude"],
            &[
                "skill",
                "copy",
                "./my-skill",
                "codex",
                "--scope",
                "project",
                "--project-dir",
                "./example-project",
            ],
            &["skill", "migrate", "claude", "codex", "--skill", "my-skill"],
            &[
                "skill",
                "migrate",
                "claude",
                "codex",
                "--all-skills",
                "--dry-run",
            ],
            &[
                "skill",
                "export",
                "my-skill",
                "--output",
                "./my-skill.tar.gz",
            ],
            &[
                "skill",
                "export",
                "--all-skills",
                "--output",
                "./skills-backup.tar.gz",
            ],
            &[
                "skill",
                "import",
                "./skills-backup.tar.gz",
                "--all-detected",
                "--mode",
                "link",
            ],
            &["skill", "status"],
            &["skill", "diff", "my-skill"],
            &["skill", "update", "my-skill", "--check"],
            &["skill", "update", "my-skill"],
            &["skill", "update", "--all-skills"],
            &["skill", "sync", "--dry-run"],
            &["skill", "sync", "my-skill"],
            &["skill", "sync", "my-skill", "--adopt-from", "claude"],
            &["skill", "rollback", "TRANSACTION_ID"],
            &["skill", "doctor"],
        ];

        for argv in examples {
            Cli::try_parse_from(*argv).unwrap_or_else(|err| panic!("{argv:?} should parse: {err}"));
        }
    }

    #[test]
    fn skill_is_repeatable() {
        let cli = Cli::try_parse_from([
            "skill", "copy", "./src", "claude", "--skill", "a", "--skill", "b",
        ])
        .unwrap();
        match cli.command {
            Command::Copy { selection, .. } => assert_eq!(selection.skills, vec!["a", "b"]),
            other => panic!("expected copy, got {other:?}"),
        }
    }

    #[test]
    fn all_skills_and_all_detected_are_separate_flags() {
        // Overloading one --all with both meanings is explicitly out.
        let cli = Cli::try_parse_from(["skill", "copy", "./src", "--all-skills"]).unwrap();
        match cli.command {
            Command::Copy { selection, .. } => {
                assert!(selection.all_skills);
                assert!(
                    !selection.all_detected,
                    "package selection must not imply destinations"
                );
            }
            other => panic!("expected copy, got {other:?}"),
        }
        assert!(Cli::try_parse_from(["skill", "copy", "./src", "--all"]).is_err());
    }

    #[test]
    fn global_flags_work_before_or_after_the_subcommand() {
        for argv in [["skill", "--json", "list"], ["skill", "list", "--json"]] {
            let cli = Cli::try_parse_from(argv).unwrap();
            assert!(cli.global.json, "{argv:?}");
        }
    }

    #[test]
    fn allow_http_is_a_separate_opt_in_from_yes() {
        let cli =
            Cli::try_parse_from(["skill", "copy", "http://x/y.tar.gz", "claude", "--yes"]).unwrap();
        match cli.command {
            Command::Copy { allow_http, .. } => {
                assert!(!allow_http, "--yes must never imply --allow-http")
            }
            other => panic!("expected copy, got {other:?}"),
        }
    }

    #[test]
    fn read_only_commands_are_marked_as_such() {
        for argv in [
            vec!["skill", "status"],
            vec!["skill", "diff", "x"],
            vec!["skill", "doctor"],
            vec!["skill", "list"],
            vec!["skill", "agents"],
        ] {
            let cli = Cli::try_parse_from(argv.clone()).unwrap();
            assert!(!cli.command.mutates(), "{argv:?} must be read-only");
        }

        for argv in [
            vec!["skill", "copy", "./x", "claude"],
            vec!["skill", "sync"],
            vec!["skill", "rollback", "t"],
        ] {
            let cli = Cli::try_parse_from(argv.clone()).unwrap();
            assert!(
                cli.command.mutates(),
                "{argv:?} should be a mutating command"
            );
        }
    }

    #[test]
    fn rejects_an_unknown_scope() {
        assert!(Cli::try_parse_from(["skill", "copy", "./x", "c", "--scope", "global"]).is_err());
    }

    #[test]
    fn rejects_an_unknown_import_mode() {
        assert!(
            Cli::try_parse_from(["skill", "import", "./b.tar.gz", "--mode", "symlink"]).is_err()
        );
    }

    #[test]
    fn the_help_epilogue_documents_every_exit_code() {
        let help = exit_code_help();
        for code in ExitCode::ALL {
            assert!(
                help.contains(&format!("{:<3} {}", code.code(), code.describe())),
                "exit code {} is missing from --help",
                code.code()
            );
        }
        // The non-execution promise belongs in --help, not only in the docs.
        assert!(help.contains("never executed"));
    }

    #[test]
    fn command_names_are_unique() {
        let names = [
            "agents", "list", "copy", "link", "migrate", "export", "import", "status", "diff",
            "update", "sync", "rollback", "doctor",
        ];
        let mut sorted = names.to_vec();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len());
    }
}
