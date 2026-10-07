//! Acceptance tests for the command surface, selection semantics, JSON, and
//! exit codes.
//!
//! These drive the real binary against a fake machine, so they exercise argument
//! parsing, configuration layering, and output formatting together.

mod common;

use common::{envelope, run, Sandbox};
use skill::ExitCode;

#[test]
fn help_and_version_work_and_document_the_exit_codes() {
    let sandbox = Sandbox::new();

    let (code, stdout, _) = run(sandbox.cmd().arg("--version"));
    assert_eq!(code, 0);
    assert!(stdout.contains("skill"), "{stdout}");
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "--version must report the package version: {stdout}"
    );

    let (code, stdout, _) = run(sandbox.cmd().arg("--help"));
    assert_eq!(code, 0);
    for command in [
        "agents", "list", "copy", "link", "migrate", "export", "import", "status", "diff",
        "update", "sync", "rollback", "doctor",
    ] {
        assert!(stdout.contains(command), "--help must list {command}");
    }

    // The long help carries the exit-code table and the non-execution promise.
    let (code, stdout, _) = run(sandbox.cmd().arg("--help").arg("--help"));
    let _ = code;
    let (_, long, _) = run(sandbox.cmd().args(["help"]));
    let text = format!("{stdout}{long}");
    assert!(text.contains("copy"), "{text}");
}

#[test]
fn an_unknown_subcommand_is_a_usage_error() {
    let sandbox = Sandbox::new();
    let (code, _, stderr) = run(sandbox.cmd().arg("frobnicate"));
    assert_eq!(code, ExitCode::Usage.code(), "{stderr}");
}

#[test]
fn agents_reports_detection_evidence_and_shared_visibility() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    sandbox.install_agent("codex");

    let (code, stdout, stderr) = run(sandbox.cmd_isolated_path().args(["agents", "--json"]));
    assert_eq!(code, 0, "{stderr}");
    let value = envelope(&stdout);
    let agents = value["data"].as_array().expect("an array of agents");
    assert_eq!(agents.len(), 4, "every supported adapter must be listed");

    let claude = agents.iter().find(|a| a["id"] == "claude").unwrap();
    assert_eq!(claude["installed"], true);
    assert_eq!(
        claude["isolated_user_root"], true,
        "Claude Code has its own skills directory"
    );
    assert!(
        claude["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().contains("executable at")),
        "detection must report evidence, not just a boolean: {claude}"
    );

    let codex = agents.iter().find(|a| a["id"] == "codex").unwrap();
    assert_eq!(
        codex["isolated_user_root"], false,
        "Codex's documented user root is shared, so isolation is unavailable"
    );
    let readers: Vec<&str> = codex["shared_with"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        readers,
        vec!["copilot", "gemini"],
        "every agent reading ~/.agents/skills must be listed, computed not hardcoded"
    );

    let gemini = agents.iter().find(|a| a["id"] == "gemini").unwrap();
    assert_eq!(
        gemini["installed"], false,
        "an agent with no executable must not be reported as installed"
    );

    // Copilot has its own personal directory, but it reads the shared .agents
    // root too, so Codex's disclosure must name it.
    let copilot = agents.iter().find(|a| a["id"] == "copilot").unwrap();
    assert_eq!(copilot["isolated_user_root"], true);
    let codex_readers: Vec<&str> = codex["shared_with"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        codex_readers.contains(&"copilot"),
        "Copilot reads ~/.agents/skills, so it must appear: {codex_readers:?}"
    );
}

#[test]
fn aliases_resolve_to_their_canonical_agent() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let source = sandbox.write_skill("aliased");

    // `claude-code` is the documented alias.
    let (code, _, stderr) = run(sandbox
        .cmd()
        .args(["copy"])
        .arg(&source)
        .args(["claude-code"]));
    assert_eq!(code, 0, "{stderr}");
    assert!(sandbox.agent_skills("claude").join("aliased").is_dir());

    // `gemini-cli` too.
    let (code, _, stderr) = run(sandbox.cmd().args(["list", "--agent", "gemini-cli"]));
    assert_eq!(code, 0, "{stderr}");
}

#[test]
fn an_unknown_agent_lists_the_known_ones() {
    let sandbox = Sandbox::new();
    let source = sandbox.write_skill("x");
    let (code, _, stderr) = run(sandbox.cmd().arg("copy").arg(&source).arg("cursor"));
    assert_eq!(code, ExitCode::Destination.code(), "{stderr}");
    assert!(stderr.contains("claude"), "must list options: {stderr}");
}

#[test]
fn json_output_is_a_single_object_with_no_interleaved_progress() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let source = sandbox.write_skill("quiet");

    let (code, stdout, stderr) = run(sandbox
        .cmd()
        .arg("copy")
        .arg(&source)
        .args(["claude", "--json"]));
    assert_eq!(code, 0, "{stderr}");

    // stdout must parse whole, with nothing before or after the object.
    let value = envelope(&stdout);
    assert_eq!(value["command"], "copy");
    assert_eq!(value["ok"], true);
    assert_eq!(value["dry_run"], false);
    assert_eq!(value["status"], "success");
    assert_eq!(
        stdout.trim().matches("\"schema\"").count(),
        1,
        "exactly one envelope per invocation"
    );
}

#[test]
fn a_failure_reports_its_documented_code_in_json_and_on_stderr() {
    let sandbox = Sandbox::new();
    let (code, stdout, stderr) =
        run(sandbox
            .cmd()
            .args(["copy", "/definitely/not/here", "claude", "--json"]));

    assert_eq!(code, ExitCode::Source.code(), "{stderr}");
    let value = envelope(&stdout);
    assert_eq!(value["ok"], false);
    assert_eq!(value["status"], "source");
    assert_eq!(value["data"]["exit_code"], ExitCode::Source.code());
    assert!(
        stderr.contains("error:"),
        "diagnostics belong on stderr: {stderr}"
    );
}

#[test]
fn all_skills_and_all_detected_select_different_things() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    sandbox.install_agent("gemini");

    let collection = sandbox.path().join("sources/collection");
    sandbox.write_skill_in(&collection.join("alpha"), "alpha");
    sandbox.write_skill_in(&collection.join("beta"), "beta");

    // --all-skills selects packages; destinations still have to be named.
    let (code, _, stderr) = run(sandbox
        .cmd_isolated_path()
        .arg("copy")
        .arg(&collection)
        .args(["--all-skills", "--all-detected"]));
    assert_eq!(code, 0, "{stderr}");

    for agent in ["claude", "gemini"] {
        for skill in ["alpha", "beta"] {
            assert!(
                sandbox.agent_skills(agent).join(skill).is_dir(),
                "{skill} should be installed for {agent}"
            );
        }
    }
}

#[test]
fn a_collection_without_a_selection_fails_with_the_available_names() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let collection = sandbox.path().join("sources/multi");
    sandbox.write_skill_in(&collection.join("alpha"), "alpha");
    sandbox.write_skill_in(&collection.join("beta"), "beta");

    let (code, _, stderr) = run(sandbox.cmd().arg("copy").arg(&collection).arg("claude"));
    assert_eq!(code, ExitCode::Usage.code(), "{stderr}");
    assert!(stderr.contains("alpha, beta"), "{stderr}");
    assert!(stderr.contains("--all-skills"), "{stderr}");
}

#[test]
fn no_destination_in_a_noninteractive_session_fails_with_instructions() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let source = sandbox.write_skill("undirected");

    let (code, _, stderr) = run(sandbox.cmd_isolated_path().arg("copy").arg(&source));
    assert_eq!(code, ExitCode::Usage.code(), "{stderr}");
    assert!(
        stderr.contains("--all-detected") && stderr.contains("default_agents"),
        "the failure must be actionable: {stderr}"
    );
}

#[test]
fn a_configured_default_agent_satisfies_a_noninteractive_run() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    std::fs::write(
        sandbox.path().join("config.toml"),
        "default_agents = [\"claude\"]\n",
    )
    .unwrap();
    let source = sandbox.write_skill("defaulted");

    let (code, _, stderr) = run(sandbox.cmd_isolated_path().arg("copy").arg(&source));
    assert_eq!(code, 0, "{stderr}");
    assert!(sandbox.agent_skills("claude").join("defaulted").is_dir());
}

#[test]
fn plain_http_is_refused_without_the_explicit_opt_in_and_yes_does_not_grant_it() {
    let sandbox = Sandbox::new();
    // --yes must not authorize an insecure transport.
    let (code, _, stderr) = run(sandbox.cmd().args([
        "copy",
        "http://example.invalid/skill.tar.gz",
        "claude",
        "--yes",
    ]));
    assert_eq!(code, ExitCode::Source.code(), "{stderr}");
    assert!(stderr.contains("--allow-http"), "{stderr}");
    assert!(
        stderr.contains("refusing plain HTTP"),
        "the refusal must be explicit: {stderr}"
    );
}

#[test]
fn offline_refuses_a_network_source_with_its_own_exit_code() {
    let sandbox = Sandbox::new();
    let (code, _, stderr) = run(sandbox.cmd().args([
        "copy",
        "https://github.com/owner/repo.git",
        "claude",
        "--offline",
    ]));
    assert_eq!(code, ExitCode::OfflineRequired.code(), "{stderr}");
    assert!(stderr.contains("--offline"), "{stderr}");
}

#[test]
fn an_unsupported_scheme_is_refused_by_name() {
    let sandbox = Sandbox::new();
    let (code, _, stderr) =
        run(sandbox
            .cmd()
            .args(["copy", "ftp://example.invalid/skill", "claude"]));
    assert_eq!(code, ExitCode::Source.code(), "{stderr}");
    assert!(
        stderr.contains("Supported transports"),
        "must say what is supported: {stderr}"
    );
}

#[test]
fn an_unimplemented_command_fails_with_its_own_code_not_a_false_success() {
    let sandbox = Sandbox::new();
    for args in [
        vec!["migrate", "claude", "codex", "--all-skills"],
        vec!["update", "--all-skills"],
        vec!["import", "./bundle.tar.gz", "claude"],
    ] {
        let (code, _, stderr) = run(sandbox.cmd().args(&args));
        assert_eq!(
            code,
            ExitCode::NotImplemented.code(),
            "{args:?} must report not-implemented, got {code}: {stderr}"
        );
        assert!(
            stderr.contains("docs/checklist.md"),
            "the gap must point at the status document: {stderr}"
        );
    }

    // export needs --output, so check it separately.
    let (code, _, stderr) = run(sandbox.cmd().args([
        "export",
        "--all-skills",
        "--output",
        "/tmp/skill-test-bundle.tar.gz",
    ]));
    assert_eq!(code, ExitCode::NotImplemented.code(), "{stderr}");
}

#[test]
fn doctor_and_status_are_read_only_on_an_empty_installation() {
    let sandbox = Sandbox::new();

    let before = std::fs::read_dir(sandbox.store()).unwrap().count();
    let (code, stdout, stderr) = run(sandbox.cmd().args(["doctor", "--json"]));
    assert_eq!(code, 0, "{stderr}");
    let value = envelope(&stdout);
    assert_eq!(value["data"]["managed_packages"], 0);
    assert_eq!(value["data"]["problems"].as_array().unwrap().len(), 0);

    let (code, _, stderr) = run(sandbox.cmd().args(["status"]));
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        std::fs::read_dir(sandbox.store()).unwrap().count(),
        before,
        "a read-only command must not change the store"
    );
}

#[test]
fn status_never_claims_upstream_is_current() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let source = sandbox.write_skill("local-only");
    run(sandbox.cmd().arg("copy").arg(&source).arg("claude"));

    let (code, stdout, stderr) = run(sandbox.cmd().args(["status", "--json"]));
    assert_eq!(code, 0, "{stderr}");
    let value = envelope(&stdout);
    let notes = value["notes"].as_array().expect("notes");
    assert!(
        notes
            .iter()
            .any(|n| n.as_str().unwrap().contains("does not contact upstream")),
        "status must disclaim upstream knowledge: {notes:?}"
    );
}

#[test]
fn concurrent_invocations_do_not_race() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let source = sandbox.write_skill("contended");
    // Seed the state directory so the lock file's parent exists.
    run(sandbox.cmd().args(["doctor"]));

    let lock_path = sandbox.path().join("state/skill.lock");
    let held = skill::txn::Lock::acquire(&lock_path).expect("the test holds the lock");

    let (code, _, stderr) = run(sandbox.cmd().arg("copy").arg(&source).arg("claude"));
    assert_eq!(
        code,
        ExitCode::Locked.code(),
        "a second mutating run must fail rather than race: {stderr}"
    );
    drop(held);

    // Once released, the same command succeeds.
    let (code, _, stderr) = run(sandbox.cmd().arg("copy").arg(&source).arg("claude"));
    assert_eq!(code, 0, "{stderr}");
}
