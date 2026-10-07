//! The `skill` binary.
//!
//! Deliberately thin: it parses argv, resolves configuration, dispatches, and
//! maps a typed error onto the documented exit code. Everything else lives in the
//! library so the acceptance tests can drive the engine directly as well as
//! through the command line.

use std::collections::BTreeMap;
use std::process::ExitCode as ProcessExit;

use clap::Parser;

use skill::cli::commands::{self, Context};
use skill::cli::render::Output;
use skill::cli::Cli;
use skill::config::{Config, Overrides};
use skill::{agent::Host, ExitCode};

fn main() -> ProcessExit {
    // clap handles --help and --version itself, exiting with its own status.
    let cli = Cli::parse();
    let command_name = cli.command.name();

    // Build the renderer before anything can fail, so even a configuration
    // error is reported in the format the caller asked for.
    let output = Output {
        json: cli.global.json,
        verbose: cli.global.verbose,
        dry_run: cli.global.dry_run,
    };

    match run(&cli, &output) {
        Ok(()) => {
            output.flush();
            ProcessExit::from(ExitCode::Success.code() as u8)
        }
        Err(err) => {
            output.emit_error(command_name, &err);
            output.flush();
            ProcessExit::from(err.exit_code().code() as u8)
        }
    }
}

fn run(cli: &Cli, output: &Output) -> skill::Result<()> {
    let overrides = Overrides {
        store: cli.global.store.clone(),
        config: cli.global.config.clone(),
        scope: None,
        project_dir: project_dir_of(cli),
        allow_http: None,
        offline: if cli.global.offline { Some(true) } else { None },
        yes: cli.global.yes,
        dry_run: cli.global.dry_run,
        json: cli.global.json,
        verbose: cli.global.verbose,
    };

    // Only the variables we document are read, and they are collected once so
    // the rest of the run works from an explicit snapshot.
    let mut env = BTreeMap::new();
    for key in skill::config::env_vars::ALL {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.to_string(), value);
        }
    }

    let config = Config::resolve(
        &overrides,
        &env,
        std::io::IsTerminal::is_terminal(&std::io::stdin()),
    )?;

    let host = Host::detect_with_home(config.agent_home.clone())?.with_project(
        config
            .project_dir
            .clone()
            .map(|dir| std::fs::canonicalize(&dir).unwrap_or(dir)),
    );

    let ctx = Context {
        config,
        host,
        output: *output,
    };

    commands::run(&ctx, &cli.command, &cli.global)
}

/// Pull `--project-dir` out of whichever subcommand carries it.
fn project_dir_of(cli: &Cli) -> Option<std::path::PathBuf> {
    use skill::cli::Command;
    match &cli.command {
        Command::Copy { scope, .. }
        | Command::Link { scope, .. }
        | Command::Migrate { scope, .. }
        | Command::Import { scope, .. } => scope.project_dir.clone(),
        _ => None,
    }
}
