//! Acceptance tests for the safety guarantees.
//!
//! These encode the product promise: install once, manage everywhere, never
//! silently lose edits. Each test states the hazard it is guarding against.

mod common;

use common::{digest, envelope, run, Sandbox};
use skill::ExitCode;

#[test]
fn a_full_package_is_copied_with_its_modes_and_layout() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let source = sandbox.write_skill("complete");

    let (code, _, stderr) = run(sandbox.cmd().arg("copy").arg(&source).arg("claude"));
    assert_eq!(code, 0, "{stderr}");

    let deployed = sandbox.agent_skills("claude").join("complete");
    for relative in ["SKILL.md", "scripts/run.sh", "references/REFERENCE.md"] {
        assert!(
            deployed.join(relative).is_file(),
            "{relative} must be deployed, not just SKILL.md"
        );
    }

    // Byte-identical to the source, which is what the digest comparison proves.
    assert_eq!(
        digest(&source),
        digest(&deployed),
        "a copy must reproduce the source exactly"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(deployed.join("scripts/run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert!(mode & 0o111 != 0, "the executable bit must survive");
    }

    // The source is left exactly as it was.
    assert!(source.join("SKILL.md").is_file());
}

#[test]
fn the_source_is_never_modified() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let source = sandbox.write_skill("untouched");
    let before = digest(&source);

    run(sandbox.cmd().arg("copy").arg(&source).arg("claude"));

    assert_eq!(
        before,
        digest(&source),
        "copy must leave the supplied source intact"
    );
}

#[test]
fn link_creates_a_symlink_to_the_store_not_to_the_source() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("gemini");
    let source = sandbox.write_skill("linked");

    let (code, _, stderr) = run(sandbox.cmd().arg("link").arg(&source).arg("gemini"));
    assert_eq!(code, 0, "{stderr}");

    let deployed = sandbox.agent_skills("gemini").join("linked");
    let meta = std::fs::symlink_metadata(&deployed).expect("the link should exist");
    assert!(
        meta.file_type().is_symlink(),
        "link mode must create a link"
    );

    // The link points into the canonical store, never at the disposable source.
    let target = std::fs::read_link(&deployed).unwrap();
    assert_eq!(
        target,
        sandbox.store().join("linked"),
        "a link must point at the canonical store, so the store stays the single \
         source of truth"
    );
    assert!(
        deployed.join("SKILL.md").is_file(),
        "reading through the link must work"
    );
}

#[test]
fn two_agents_sharing_one_directory_are_written_once() {
    // Codex and Gemini both read ~/.agents/skills. Writing it twice would be
    // wasteful, and deleting it later on behalf of one agent would break the
    // other.
    let sandbox = Sandbox::new();
    sandbox.install_agent("codex");
    sandbox.install_agent("gemini");
    let source = sandbox.write_skill("shared");

    // Force Gemini to the shared alias by pre-creating only that root.
    let (code, stdout, stderr) = run(sandbox
        .cmd_isolated_path()
        .arg("copy")
        .arg(&source)
        .args(["codex", "gemini", "--json"]));
    assert_eq!(code, 0, "{stderr}");
    let value = envelope(&stdout);

    let destinations = value["data"]["installed"][0]["destinations"]
        .as_array()
        .expect("destinations");
    // Gemini prefers its own root, so these are genuinely two directories.
    assert_eq!(destinations.len(), 2);
    assert!(sandbox.agent_skills("codex").join("shared").is_dir());
    assert!(sandbox.agent_skills("gemini").join("shared").is_dir());
}

#[test]
fn installing_for_codex_alone_discloses_gemini_visibility() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("codex");
    let source = sandbox.write_skill("disclosed");

    let (code, stdout, stderr) = run(sandbox
        .cmd_isolated_path()
        .arg("copy")
        .arg(&source)
        .args(["codex", "--json"]));
    assert_eq!(code, 0, "{stderr}");

    let value = envelope(&stdout);
    let notes = value["notes"].as_array().unwrap();
    let joined = notes
        .iter()
        .map(|n| n.as_str().unwrap())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        joined.contains("visible to gemini"),
        "shared visibility must be disclosed: {joined}"
    );
    assert!(
        joined.contains("isolation is not available"),
        "we must not imply isolation Codex cannot provide: {joined}"
    );
    assert!(stderr.contains("visible to gemini"), "{stderr}");
}

#[test]
fn an_existing_unmanaged_destination_is_never_overwritten() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let source = sandbox.write_skill("collide");

    // The user hand-wrote a skill of the same name.
    let theirs = sandbox.agent_skills("claude").join("collide");
    std::fs::create_dir_all(&theirs).unwrap();
    std::fs::write(
        theirs.join("SKILL.md"),
        "---\nname: collide\ndescription: Written by the user.\n---\nTHEIRS\n",
    )
    .unwrap();

    let (code, _, stderr) = run(sandbox.cmd().arg("copy").arg(&source).arg("claude"));
    assert_eq!(code, ExitCode::Refused.code(), "{stderr}");
    assert!(stderr.contains("not managed by skill"), "{stderr}");
    assert!(
        std::fs::read_to_string(theirs.join("SKILL.md"))
            .unwrap()
            .contains("THEIRS"),
        "the user's own file must survive untouched"
    );
}

#[test]
fn reinstalling_over_a_locally_edited_deployment_refuses_rather_than_discarding() {
    // This is the exact failure mode the product promise exists to prevent.
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let source = sandbox.write_skill("precious");

    run(sandbox.cmd().arg("copy").arg(&source).arg("claude"));
    let deployed = sandbox.agent_skills("claude").join("precious");

    // The operator edits the deployed copy.
    let mut content = std::fs::read_to_string(deployed.join("SKILL.md")).unwrap();
    content.push_str("\nMY LOCAL EDIT\n");
    std::fs::write(deployed.join("SKILL.md"), &content).unwrap();

    let (code, _, stderr) = run(sandbox.cmd().arg("copy").arg(&source).arg("claude"));
    assert_eq!(code, ExitCode::Conflict.code(), "{stderr}");
    assert!(
        stderr.contains("has changed since skill last wrote it"),
        "{stderr}"
    );
    assert!(
        stderr.contains("adopt-from claude"),
        "the refusal must say how to keep the edits: {stderr}"
    );
    assert!(
        std::fs::read_to_string(deployed.join("SKILL.md"))
            .unwrap()
            .contains("MY LOCAL EDIT"),
        "the edit must survive"
    );
}

#[test]
fn yes_does_not_authorize_a_conflict_overwrite() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let source = sandbox.write_skill("stubborn");
    run(sandbox.cmd().arg("copy").arg(&source).arg("claude"));

    let deployed = sandbox.agent_skills("claude").join("stubborn");
    std::fs::write(deployed.join("SKILL.md"), "EDITED\n").unwrap();

    let (code, _, stderr) = run(sandbox
        .cmd()
        .arg("copy")
        .arg(&source)
        .args(["claude", "--yes"]));
    assert_eq!(
        code,
        ExitCode::Conflict.code(),
        "--yes accepts a determined plan, never a conflict: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(deployed.join("SKILL.md")).unwrap(),
        "EDITED\n"
    );
}

#[test]
fn the_full_sync_truth_table_behaves_as_documented() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let source = sandbox.write_skill("table");
    run(sandbox.cmd().arg("copy").arg(&source).arg("claude"));

    let deployed = sandbox.agent_skills("claude").join("table");
    let canonical = sandbox.store().join("table");

    // Row: neither changed. No-op.
    let (code, stdout, _) = run(sandbox.cmd().args(["status", "--json"]));
    assert_eq!(code, 0);
    assert_eq!(envelope(&stdout)["data"]["items"][0]["drift"], "unchanged");

    // Row: canonical changed, deployment unchanged. Safe update.
    std::fs::write(
        canonical.join("SKILL.md"),
        "---\nname: table\ndescription: v2.\n---\n",
    )
    .unwrap();
    let (_, stdout, _) = run(sandbox.cmd().args(["status", "--json"]));
    assert_eq!(
        envelope(&stdout)["data"]["items"][0]["drift"],
        "source_ahead"
    );

    let (code, _, stderr) = run(sandbox.cmd().args(["sync"]));
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        digest(&canonical),
        digest(&deployed),
        "a safe update must bring the deployment into line"
    );

    // Row: deployment changed, canonical unchanged. Edits preserved.
    std::fs::write(deployed.join("SKILL.md"), "DEPLOYED EDIT\n").unwrap();
    let (_, stdout, _) = run(sandbox.cmd().args(["status", "--json"]));
    assert_eq!(
        envelope(&stdout)["data"]["items"][0]["drift"],
        "target_drifted"
    );
    let (code, _, _) = run(sandbox.cmd().args(["sync"]));
    assert_eq!(code, 0, "preserving an edit is sync working, not failing");
    assert_eq!(
        std::fs::read_to_string(deployed.join("SKILL.md")).unwrap(),
        "DEPLOYED EDIT\n",
        "the edit must be preserved"
    );

    // Row: both changed differently. Conflict, nothing written.
    std::fs::write(canonical.join("SKILL.md"), "CANONICAL EDIT\n").unwrap();
    let (_, stdout, _) = run(sandbox.cmd().args(["status", "--json"]));
    assert_eq!(envelope(&stdout)["data"]["items"][0]["drift"], "conflict");

    let (code, _, stderr) = run(sandbox.cmd().args(["sync"]));
    assert_eq!(
        code,
        ExitCode::Conflict.code(),
        "a conflict must be reported, not hidden: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(deployed.join("SKILL.md")).unwrap(),
        "DEPLOYED EDIT\n"
    );
    assert_eq!(
        std::fs::read_to_string(canonical.join("SKILL.md")).unwrap(),
        "CANONICAL EDIT\n"
    );

    // Row: both changed to identical content. Baseline refresh, no rewrite.
    std::fs::write(deployed.join("SKILL.md"), "CANONICAL EDIT\n").unwrap();
    let (_, stdout, _) = run(sandbox.cmd().args(["status", "--json"]));
    assert_eq!(
        envelope(&stdout)["data"]["items"][0]["drift"],
        "converged_identically"
    );

    // Row: deployment missing. Reported, never inferred as a deletion.
    std::fs::remove_dir_all(&deployed).unwrap();
    let (_, stdout, _) = run(sandbox.cmd().args(["status", "--json"]));
    assert_eq!(
        envelope(&stdout)["data"]["items"][0]["drift"],
        "target_missing"
    );
    // The package itself is still managed: a missing directory is not a removal.
    let (_, stdout, _) = run(sandbox.cmd().args(["list", "--json"]));
    assert_eq!(
        envelope(&stdout)["data"].as_array().unwrap().len(),
        1,
        "a missing deployment must not imply the package was uninstalled"
    );
}

#[test]
fn a_file_deletion_inside_a_package_counts_as_a_change() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let source = sandbox.write_skill("deletions");
    run(sandbox.cmd().arg("copy").arg(&source).arg("claude"));

    let deployed = sandbox.agent_skills("claude").join("deletions");
    std::fs::remove_file(deployed.join("references/REFERENCE.md")).unwrap();

    let (_, stdout, _) = run(sandbox.cmd().args(["status", "--json"]));
    let value = envelope(&stdout);
    assert_eq!(value["data"]["items"][0]["drift"], "target_drifted");

    let (code, stdout, _) = run(sandbox.cmd().args(["diff", "deletions", "--json"]));
    assert_eq!(code, 0);
    let value = envelope(&stdout);
    let deleted = value["data"]["deployments"][0]["deleted"]
        .as_array()
        .expect("deleted list");
    assert!(
        deleted.iter().any(|p| p == "references/REFERENCE.md"),
        "a deletion must be visible to reconciliation: {deleted:?}"
    );
}

#[test]
fn dry_run_leaves_managed_state_byte_for_byte() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let source = sandbox.write_skill("planned");
    run(sandbox.cmd().arg("copy").arg(&source).arg("claude"));

    let before_store = digest(&sandbox.store());
    let before_deployed = digest(&sandbox.agent_skills("claude"));
    let before_db = std::fs::read(sandbox.path().join("state/state.sqlite3")).unwrap();

    let second = sandbox.write_skill("planned-two");
    let (code, stdout, stderr) =
        run(sandbox
            .cmd()
            .arg("copy")
            .arg(&second)
            .args(["claude", "--dry-run", "--json"]));
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        envelope(&stdout)["dry_run"],
        true,
        "the envelope must mark a plan as a plan"
    );

    assert_eq!(before_store, digest(&sandbox.store()), "store changed");
    assert_eq!(
        before_deployed,
        digest(&sandbox.agent_skills("claude")),
        "deployments changed"
    );
    assert_eq!(
        before_db,
        std::fs::read(sandbox.path().join("state/state.sqlite3")).unwrap(),
        "the state database changed during a dry run"
    );
    assert!(
        !sandbox.store().join("planned-two").exists(),
        "a dry run must not create anything"
    );
}

#[test]
fn repeating_a_successful_install_is_idempotent() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let source = sandbox.write_skill("repeat");

    let (code, _, stderr) = run(sandbox.cmd().arg("copy").arg(&source).arg("claude"));
    assert_eq!(code, 0, "{stderr}");
    let after_first = digest(&sandbox.agent_skills("claude").join("repeat"));

    let (code, _, stderr) = run(sandbox.cmd().arg("copy").arg(&source).arg("claude"));
    assert_eq!(code, 0, "a repeat must succeed: {stderr}");
    assert_eq!(
        after_first,
        digest(&sandbox.agent_skills("claude").join("repeat"))
    );

    let (_, stdout, _) = run(sandbox.cmd().args(["list", "--json"]));
    assert_eq!(
        envelope(&stdout)["data"].as_array().unwrap().len(),
        1,
        "a repeat must not create a second package"
    );
}

#[test]
fn the_same_name_from_a_different_origin_is_refused() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");

    let first = sandbox.write_skill_in(&sandbox.path().join("sources/one/dup"), "dup");
    let second = sandbox.write_skill_in(&sandbox.path().join("sources/two/dup"), "dup");

    let (code, _, stderr) = run(sandbox.cmd().arg("copy").arg(&first).arg("claude"));
    assert_eq!(code, 0, "{stderr}");

    let (code, _, stderr) = run(sandbox.cmd().arg("copy").arg(&second).arg("claude"));
    assert_eq!(
        code,
        ExitCode::InvalidPackage.code(),
        "a second origin must not silently take the name: {stderr}"
    );
    assert!(stderr.contains("--as"), "must offer an alias: {stderr}");

    // The alias resolves it.
    let (code, _, stderr) = run(sandbox
        .cmd()
        .arg("copy")
        .arg(&second)
        .args(["claude", "--as", "dup-two"]));
    assert_eq!(code, 0, "{stderr}");
    assert!(sandbox.agent_skills("claude").join("dup-two").is_dir());
}

#[test]
fn a_standalone_markdown_file_warns_about_unresolved_references() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let file = sandbox.path().join("sources/lonely.md");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        "---\nname: lonely\ndescription: A lone skill file.\n---\nSee scripts/run.sh\n",
    )
    .unwrap();
    // A neighbour that must not be collected.
    std::fs::write(sandbox.path().join("sources/unrelated.txt"), "not mine").unwrap();

    let (code, stdout, stderr) = run(sandbox
        .cmd()
        .arg("copy")
        .arg(&file)
        .args(["claude", "--json"]));
    assert_eq!(code, 0, "{stderr}");

    let notes = envelope(&stdout)["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        notes.contains("cannot be resolved"),
        "must warn about relative references: {notes}"
    );
    assert!(
        !sandbox
            .agent_skills("claude")
            .join("lonely/unrelated.txt")
            .exists(),
        "a neighbouring file must not be collected"
    );
}

#[test]
fn pointing_at_skill_md_imports_the_whole_package() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let source = sandbox.write_skill("boundary");

    let (code, _, stderr) = run(sandbox
        .cmd()
        .arg("copy")
        .arg(source.join("SKILL.md"))
        .arg("claude"));
    assert_eq!(code, 0, "{stderr}");

    let deployed = sandbox.agent_skills("claude").join("boundary");
    assert!(
        deployed.join("scripts/run.sh").is_file(),
        "the package boundary must be imported, not just the one file"
    );
    assert_eq!(digest(&source), digest(&deployed));
}

#[test]
fn an_invalid_package_is_refused_before_anything_is_written() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let bad = sandbox.path().join("sources/bad");
    std::fs::create_dir_all(&bad).unwrap();
    // No description, which every agent needs.
    std::fs::write(bad.join("SKILL.md"), "---\nname: bad\n---\nbody\n").unwrap();

    let (code, _, stderr) = run(sandbox.cmd().arg("copy").arg(&bad).arg("claude"));
    assert_eq!(code, ExitCode::InvalidPackage.code(), "{stderr}");
    assert!(stderr.contains("description"), "{stderr}");
    assert!(
        !sandbox.store().join("bad").exists(),
        "nothing may be written for an invalid package"
    );
}

#[test]
fn a_package_containing_an_escaping_symlink_is_refused() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let secret = sandbox.path().join("secret.txt");
    std::fs::write(&secret, "private").unwrap();

    let malicious = sandbox.path().join("sources/malicious");
    std::fs::create_dir_all(&malicious).unwrap();
    std::fs::write(
        malicious.join("SKILL.md"),
        "---\nname: malicious\ndescription: Tries to escape its package.\n---\n",
    )
    .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("../../secret.txt", malicious.join("leak.txt")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&secret, malicious.join("leak.txt")).unwrap();

    let (code, _, stderr) = run(sandbox.cmd().arg("copy").arg(&malicious).arg("claude"));
    assert_eq!(code, ExitCode::InvalidPackage.code(), "{stderr}");
    assert!(
        !sandbox.agent_skills("claude").join("malicious").exists(),
        "nothing may be deployed from a package that escapes itself"
    );
}

#[test]
fn rollback_restores_a_deployment_and_refuses_to_discard_later_edits() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");

    // Adopt an existing managed deployment, then change the canonical content so
    // a sync produces a backup we can roll back.
    let source = sandbox.write_skill("revertible");
    run(sandbox.cmd().arg("copy").arg(&source).arg("claude"));

    let deployed = sandbox.agent_skills("claude").join("revertible");
    let canonical = sandbox.store().join("revertible");
    let original = std::fs::read_to_string(deployed.join("SKILL.md")).unwrap();

    std::fs::write(
        canonical.join("SKILL.md"),
        "---\nname: revertible\ndescription: Version two.\n---\n",
    )
    .unwrap();
    let (code, _, stderr) = run(sandbox.cmd().args(["sync"]));
    assert_eq!(code, 0, "{stderr}");
    assert_ne!(
        std::fs::read_to_string(deployed.join("SKILL.md")).unwrap(),
        original
    );

    // Find the transaction to undo.
    let (_, stdout, _) = run(sandbox.cmd().args(["doctor", "--json"]));
    let _ = envelope(&stdout);
    let journals: Vec<_> = std::fs::read_dir(sandbox.path().join("state/transactions"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    assert!(!journals.is_empty(), "a sync must leave a journal");

    let txn_id = journals
        .iter()
        .max_by_key(|p| p.metadata().unwrap().modified().unwrap())
        .unwrap()
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .to_string();

    // An edit made after the transaction must block the rollback.
    std::fs::write(deployed.join("SKILL.md"), "EDITED AFTER THE SYNC\n").unwrap();
    let (code, _, stderr) = run(sandbox.cmd().args(["rollback", &txn_id]));
    assert_eq!(code, ExitCode::Conflict.code(), "{stderr}");
    assert!(
        stderr.contains("edited after the transaction"),
        "the refusal must explain itself: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(deployed.join("SKILL.md")).unwrap(),
        "EDITED AFTER THE SYNC\n",
        "a refused rollback must not touch the later edit"
    );
}

#[test]
fn an_unfinished_transaction_blocks_mutation_and_is_reported_by_doctor() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let source = sandbox.write_skill("interrupted");
    run(sandbox.cmd().arg("copy").arg(&source).arg("claude"));

    // Simulate a crash by leaving an open transaction row behind.
    let db = sandbox.path().join("state/state.sqlite3");
    let store = skill::state::Store::open(&db).unwrap();
    store.begin_txn("crashed-run", "sync", None).unwrap();
    drop(store);

    let (code, stdout, stderr) = run(sandbox.cmd().args(["doctor", "--json"]));
    assert_eq!(
        code, 0,
        "doctor must stay usable on a damaged store: {stderr}"
    );
    let value = envelope(&stdout);
    let problems = value["data"]["problems"].as_array().unwrap();
    assert!(
        problems
            .iter()
            .any(|p| p.as_str().unwrap().contains("crashed-run")),
        "doctor must surface the unfinished run: {problems:?}"
    );

    let second = sandbox.write_skill("after-crash");
    let (code, _, stderr) = run(sandbox.cmd().arg("copy").arg(&second).arg("claude"));
    assert_eq!(
        code,
        ExitCode::Partial.code(),
        "a mutating command must refuse to start mid-change: {stderr}"
    );
    assert!(stderr.contains("skill rollback"), "{stderr}");
}

#[test]
fn a_project_scope_install_needs_a_project_directory() {
    let sandbox = Sandbox::new();
    sandbox.install_agent("claude");
    let source = sandbox.write_skill("scoped");

    let (code, _, stderr) = run(sandbox
        .cmd()
        .arg("copy")
        .arg(&source)
        .args(["claude", "--scope", "project"]));
    assert_eq!(code, ExitCode::Usage.code(), "{stderr}");
    assert!(stderr.contains("--project-dir"), "{stderr}");

    let (code, _, stderr) = run(sandbox
        .cmd()
        .arg("copy")
        .arg(&source)
        .args(["claude", "--scope", "project", "--project-dir"])
        .arg(sandbox.project()));
    assert_eq!(code, 0, "{stderr}");
    assert!(sandbox.project().join(".claude/skills/scoped").is_dir());
}

#[test]
fn a_gemini_project_install_discloses_the_folder_trust_caveat() {
    // Gemini skips workspace skills in an untrusted folder, and trust defaults to
    // untrusted, so a correct write can still never load.
    let sandbox = Sandbox::new();
    sandbox.install_agent("gemini");
    let source = sandbox.write_skill("workspace");

    let (code, stdout, stderr) = run(sandbox
        .cmd()
        .arg("copy")
        .arg(&source)
        .args(["gemini", "--scope", "project", "--project-dir"])
        .arg(sandbox.project())
        .arg("--json"));
    assert_eq!(code, 0, "{stderr}");

    let notes = envelope(&stdout)["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        notes.contains("untrusted folder"),
        "the trust caveat must reach the operator: {notes}"
    );
}
