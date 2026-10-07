# Implementation checklist

Status of every requirement in `docs/prompt.md`, as of 2026-10-07.

`Done` means implemented **and** covered by a test that would fail if it broke.
`Partial` names exactly what is missing. `Not done` means not in this build, and
the corresponding command exits **12** (`not_implemented`) rather than returning a
misleading success.

Verification run for this status: `cargo fmt --all -- --check`,
`cargo clippy --all-targets --all-features -- -D warnings`, the same with
`--no-default-features`, `cargo test --locked --all-features`,
`cargo test --locked --no-default-features`, `cargo +1.88 check --locked --all-targets`,
`actionlint`. All green. 240 tests pass (201 unit, 18 CLI, 21 safety).

## 1. Product and scope

| Requirement | Status | Evidence |
| --- | --- | --- |
| Three adapters: Claude Code, Codex, Gemini CLI | Done | `src/agent/{claude,codex,gemini}.rs` |
| `claude-code` and `gemini-cli` aliases | Done | `resolves_ids_and_documented_aliases`, `aliases_resolve_to_their_canonical_agent` |
| Extensible agent support | Done | the `Agent` trait; no plugin machinery, by design |
| Local installations only, no cloud claim | Done | stated in `--long-help` and the README |
| Attribute inspiration, no clean-room claim | Done | `ATTRIBUTION.md` |
| Verify the five primary sources | Done | `docs/compatibility.md`, retrieval date and redirects recorded |

## 2. CLI contract

Every one of the 28 documented example invocations parses, asserted by
`parses_every_documented_example`.

| Command | Status | Note |
| --- | --- | --- |
| `agents` | Done | reports detection evidence and shared visibility |
| `list`, `list --agent` | Done | |
| `copy` | Done | local and `file://` sources |
| `link` | Done | per-skill symlink into the store, never to the source |
| `status` | Done | read-only; disclaims upstream knowledge |
| `diff` | Done | per-file, with `--verbose` unified diff |
| `sync` | Done | full truth table; conflicts exit 3 |
| `sync --adopt-from` | **Not done** | exit 12; the comparison it needs is implemented and tested |
| `rollback` | Done | refuses to discard post-transaction edits |
| `doctor` | Done | read-only; reports unfinished transactions and broken links |
| `migrate` | **Not done** | exit 12; the journalled pipeline it would use is built and tested |
| `update`, `update --check` | **Not done** | exit 12; needs the git and http backends |
| `export` | **Not done** | exit 12 |
| `import` | **Not done** | exit 12 |

| Semantics | Status | Evidence |
| --- | --- | --- |
| `--all-skills` selects packages, `--all-detected` selects destinations, never one `--all` | Done | `all_skills_and_all_detected_are_separate_flags` |
| `--skill` repeatable | Done | `skill_is_repeatable` |
| Interactive destination offer showing exact paths | Done | `prompt_for_agents`; the label includes the shared-root warning. Not auto-tested (needs a TTY) |
| Noninteractive missing selection fails with instructions | Done | `no_destination_in_a_noninteractive_session_fails_with_instructions` |
| Configured default satisfies a noninteractive run | Done | `a_configured_default_agent_satisfies_a_noninteractive_run` |
| `--yes` does not authorize conflicts, insecure transport, or deletion | Done | `yes_does_not_authorize_a_conflict_overwrite`, `plain_http_is_refused_without_the_explicit_opt_in_and_yes_does_not_grant_it` |
| Explicit agent accepted when undetected, condition reported | Done | `an_undetected_but_named_agent_is_honoured_and_reported` |
| Stable documented exit codes | Done | `codes_are_stable_and_unique`; `--help` epilogue generated from the table |
| Schema-versioned JSON, no interleaved progress, stderr diagnostics | Done | `json_output_is_a_single_object_with_no_interleaved_progress` |
| Import mode offer | **Not done** | `import` is not implemented |

## 3. Package discovery and compatibility

| Requirement | Status | Evidence |
| --- | --- | --- |
| Preserve all package content, modes, layout | Done | `a_full_package_is_copied_with_its_modes_and_layout` |
| Do not copy an unrelated repository or home directory | Done | `a_package_subtree_is_not_split_into_more_packages`, `skips_vcs_and_build_directories`, `does_not_follow_links_while_surveying` |
| Directories, collections, standalone files | Done | `src/pkg/scan.rs` tests |
| `SKILL.md` inside a package imports the package boundary | Done | `pointing_at_skill_md_imports_the_whole_package` |
| Standalone file warns about relative references, collects no neighbours | Done | `a_standalone_markdown_file_warns_about_unresolved_references` |
| Validate frontmatter, preserve unknown fields | Done | `preserves_unknown_fields_verbatim` |
| Reject unsafe names, traversal, reserved, cross-platform collisions | Done | `rejects_unsafe_install_names`, `rejects_names_the_agents_reserve` |
| Internal identity from source plus selector, not display name | Done | `identity_distinguishes_same_name_different_origin` |
| Never silently replace a same-named skill from another origin | Done | `the_same_name_from_a_different_origin_is_refused` |
| Adapter-owned discovery, scopes, precedence, capabilities | Done | per-adapter tests |
| Detection evidence, not a leftover directory | Done | `detection_separates_executable_from_leftover_directory` |
| Resolve physical aliases, do not write twice | Done | `two_agents_resolving_to_one_directory_are_written_once` |
| Disclose shared-root visibility, prefer specific paths | Done | `installing_for_codex_alone_discloses_gemini_visibility` |
| Classify plugin, org, built-in, account-synced separately | Done | `classifies_the_documented_special_directories` and the per-adapter equivalents |
| Require explicit adoption for unmanaged | Done | `an_existing_unmanaged_destination_is_never_overwritten` |
| Unknown provenance stays unknown | Done | asserted in `classifies_the_documented_special_directories` |
| Flag agent-specific frontmatter, distinguish confirmed from heuristic | Done | `Finding::confirmed`; `missing_name_warns_globally_but_blocks_gemini` |
| Never rewrite prompts or translate permissions | Done | `SKILL.md` is never written; only read and hashed |
| Flag hooks, MCP, hardcoded paths | Partial | frontmatter fields and Gemini name rewriting are flagged. Body-level scanning for MCP requirements and absolute paths is **not** implemented |

## 4. Sources and transports

| Transport | Status | Note |
| --- | --- | --- |
| Filesystem: relative, absolute, `file://`, mounted shares, UNC | Done | `src/source/fs.rs`; mounted shares labelled as mounted-path support, not native SMB |
| Locator parsing for every transport | Done | 14 tests in `src/source/locator.rs`, including Windows drive vs SCP-style disambiguation |
| Reject unsupported schemes by name | Done | `rejects_unsupported_schemes_by_name` |
| Bare `owner/repo` is **not** silently expanded to a URL | Done | `a_bare_owner_repo_stays_a_path` |
| Git: HTTPS, SSH, SCP-style, subdirectories, refs, pins | **Not done** | parsed and classified; acquisition exits 12 |
| HTTP(S): files and archives, content sniffing | **Not done** | parsed; `--allow-http` gate and offline refusal already enforced before the backend is reached |
| SMB: native `smb://` acquisition | **Not done** | URL fully decomposed and tested; acquisition exits 12. `smb2` selected and wired as an optional default feature |
| Credentials never on a command line, in a locator, log, or export | Done | `locator_sanitisation_drops_credentials`, `an_smb_url_keeps_the_username_but_never_the_password` |
| Plain HTTP needs `--allow-http`, no silent HTTPS downgrade | Done | enforced in `install`; redirect policy is part of the unbuilt http backend |
| Package hashes, not validators or timestamps | Done | `mtime_alone_does_not_change_the_digest` |
| Offline makes no network access | Done | `offline_refuses_a_network_source_with_its_own_exit_code` |

## 5. Store, provenance, state

| Requirement | Status | Evidence |
| --- | --- | --- |
| Default store `~/skills`, configurable | Done | `defaults_put_the_store_in_the_home_directory` |
| Home via platform API, not `$HOME` | Done | `detect_resolves_home_from_the_platform_api`. The one override is the namespaced `SKILL_AGENT_HOME`, documented as such |
| Platform config, state, cache directories | Done | `config::Paths` |
| State kept out of skill directories | Done | `state_never_lives_inside_the_store` |
| Schema-versioned transactional store | Done | `src/state/schema.rs`; `refuses_a_newer_schema_instead_of_guessing` |
| Track id, names, locator, type, path, ref, revision, pin policy | Done | `PackageRecord`; `round_trips_a_package` |
| Pristine upstream and canonical snapshots kept separately | Done | `keeps_the_three_baselines_apart` |
| Per-destination baselines | Done | `deployment_baselines_are_kept_per_destination` |
| Per-file hashes, paths, kinds, modes | Done | `snapshot_file` table |
| Transaction history, backups, ownership | Done | `txn`, `backup` tables |
| Imported-origin trust flag | Done | `trusted_origin`; defaults false for a bundle |
| Deterministic whole-tree hashes | Done | 7 tests in `src/pkg/tree.rs` |
| Detect concurrent modification rather than hashing one version and deploying another | Done | `a_target_changed_since_planning_aborts_instead_of_overwriting` |
| Untrusted imported manifests | Partial | the trust flag and the policy exist; `import` itself is not implemented |

## 6. Update and synchronization

| Truth-table row | Status | Evidence |
| --- | --- | --- |
| Canonical changed, deployment unchanged: update | Done | `the_full_sync_truth_table_behaves_as_documented` |
| Deployment changed, canonical unchanged: preserve and report | Done | same |
| Both changed differently: conflict, write nothing | Done | same |
| Both changed identically: refresh baseline, no rewrite | Done | same |
| Deployment missing: report, never infer removal | Done | same |
| Neither changed: no-op | Done | same |
| File deletions participate in conflict checks | Done | `a_file_deletion_inside_a_package_counts_as_a_change` |
| Whole-package removal never inferred from a missing directory | Done | asserted in the truth-table test |
| Every destructive resolution needs a specific decision | Done | `yes_does_not_authorize_a_conflict_overwrite` |
| Link edits are canonical edits; sync validates the link | Done | `build_sync_plan` link branch; `link_creates_a_symlink_to_the_store_not_to_the_source` |
| Linking unavailable fails with a copy alternative | Done | the `SymlinkSupport::Unsupported` path in `install` |
| Upstream compared against the pristine baseline | Partial | the baseline is recorded and the comparison is implemented and tested; `update` is not wired up |
| `--adopt-from` promotes one disambiguated destination | **Not done** | exit 12 |

## 7. Transactions, migration, security

| Requirement | Status | Evidence |
| --- | --- | --- |
| One reused pipeline | Done | `src/txn/mod.rs`, used by copy, link, sync, rollback |
| Per-target atomic replacement, staged on the target filesystem | Done | `safepath::replace_dir` |
| Operation journal, locking, truthful partial failure | Done | `src/txn/journal.rs`; `an_interrupted_mutation_is_identified_with_its_backup` |
| Recoverable across crashes | Done | `a_truncated_final_line_is_recoverable`, `an_unfinished_transaction_blocks_mutation_and_is_reported_by_doctor` |
| Revalidate fingerprints before mutation | Done | `a_target_changed_since_planning_aborts_instead_of_overwriting` |
| Rollback must not destroy later edits | Done | `rollback_refuses_to_discard_edits_made_after_the_transaction` |
| Never execute downloaded content | Done | no process is ever spawned from package content; `docs/security.md` |
| Defend extraction and copying | Done | 8 tests in `src/safepath.rs`, plus `a_package_containing_an_escaping_symlink_is_refused` |
| Enforce containment including ancestor symlinks | Done | `containment_rejects_symlink_escape` |
| Bound bytes, count, depth, redirects, timeouts | Done | `Limits`; `enforces_the_file_count_limit`, `enforces_depth_limit` |
| Refuse unsafe special files and permission bits | Done | `refuses_setuid_content` |
| Private permissions for state and backups | Done | `opening_a_file_backed_store_creates_private_state`, `a_journal_is_private` |
| Never overwrite unmanaged destinations | Done | `an_existing_unmanaged_destination_is_never_overwritten` |
| Redacted logs, no telemetry | Done | no network call is made that was not requested |
| Hash proves integrity, not trust | Done | documented in `docs/security.md` |
| `--dry-run` changes nothing | Done | `dry_run_leaves_managed_state_byte_for_byte` compares the store, deployments, and the database byte for byte |
| Migration removes only verified sources, after success | **Not done** | `migrate` exits 12 |

## 8. Repository deliverables

| Deliverable | Status | Note |
| --- | --- | --- |
| Cargo metadata | Done | one deliberate deviation, below |
| `Cargo.lock` committed | Done | `.gitignore` no longer excludes it |
| MSRV declared and tested | Done | **1.88**, not the 1.85 originally planned: transitive `icu_*` and `time` require 1.88. Verified with `cargo +1.88 check --locked` and gated in CI |
| Typed errors, no `unwrap` on user input | Done | `src/error.rs`; `unwrap`/`expect` appear only in tests, plus one `expect` on a static literal that cannot fail (`src/state/mod.rs`) |
| No `unsafe` | Done | none in `src/` |
| GPLv3 licence text | Done | pre-existing `LICENSE`, dual-licensed with the commercial track |
| README with every required section | Done | |
| Makefile with build/install/run/test/clean/fmt/lint | Done | upgraded to the Pro variant, plus `msrv`, `audit`, `ci` |
| Multi-stage Dockerfile, non-root, CA certs, `skill` entrypoint, `--help` default | Done | no git or samba packages needed: `gix` and `smb2` are pure Rust. Container-discovery and host-symlink hazards documented in the file |
| GitHub Actions: fmt, lint, test, three-platform builds | Done | plus MSRV and feature-matrix jobs |
| Artifacts on every successful build | Done | the tag-only condition was removed |
| Releases only on `v*`, after the matrix, with checksums and tag validation | Done | tag is validated against `Cargo.toml` |
| `contents: write` only on the release job, PR jobs read-only | Done | top-level `permissions: contents: read` |
| `docs/architecture.md`, `compatibility.md`, `security.md`, checklist | Done | this file |
| Example config | Done | `examples/config.toml` |
| Tests and fixtures | Done | fixtures are generated by the harness rather than committed, so they cannot drift |

### Deliberate deviations from the prompt, and why

1. **`actions/checkout@v7`, not `@v4`.** The repository had already upgraded its
   actions; downgrading would regress maintained versions. Confirmed with the
   user.
2. **`repository = "https://github.com/gbiagomba/Skills.rs"`.** The prompt
   specified `github.com/gbiagomba/skill`, which does not exist; this is the
   actual remote. Confirmed with the user.
3. **MSRV 1.88, not 1.85.** Forced by transitive dependencies, as above.
4. **Missing `name` is a warning, not an error.** Reasoned in
   `docs/compatibility.md`.
5. **`SKILL_AGENT_HOME` exists.** Without it there is no way to run an
   end-to-end test without writing into the developer's real agent
   installation. It is our own namespaced variable; `$HOME` is still never
   honoured for this.
6. **Exit code 12 added** for "documented but not implemented in this build", so
   an unfinished command can never be mistaken for success.

## 9. Acceptance tests

| Area | Status |
| --- | --- |
| Isolated temporary homes, stores, agent roots | Done. `tests/common/mod.rs` builds a full fake machine; `env_clear` plus an allowlist means the developer's environment cannot affect or be affected by a test |
| Command parsing, aliases, selection, JSON, exit codes | Done, `tests/cli.rs` |
| Full-package copy and link to the adapters | Done |
| Discovery precedence and overrides | Done, unit level |
| Shared and identical destination dedup and warnings | Done |
| Multiple skills per source, standalone, collisions | Done |
| Local Git repositories, branch refresh, pins, subpaths | **Not done**, the git backend is absent |
| HTTP archives, redirect and size bounds, denied plain HTTP | Partial: the plain-HTTP refusal is tested; a fixture server is not needed until the backend exists |
| Native SMB through an isolated fixture | **Not done**. The `smb-it` feature gate exists; no fixture, and **no SMB behaviour is claimed as verified** |
| Mounted shares tested separately | Partial: path classification is unit tested; no real mount was exercised |
| Full sync truth table, identical edits, deletions, missing deployments | Done |
| Upstream change against clean and modified canonical | Partial: the comparison is unit tested; the end-to-end path needs `update` |
| Migration success and failure | **Not done** |
| Unmanaged refusal, plugin exclusion, symlink escape, malicious archive, corrupt state, schema migration | Done, except archive extraction (no archive backend) |
| Concurrent invocation, concurrent modification, interrupted transaction, rollback, later edits | Done |
| Export/import round trip | **Not done** |
| Idempotency and dry-run byte-for-byte | Done |

## 10. Honest summary

**What works end to end today:** discovering the three agents with evidence,
acquiring a skill from a local path or `file://` URI (single package, collection,
standalone file, or a `SKILL.md` inside a package), installing it as independent
copies or per-skill symlinks into a canonical store with full provenance, listing
and inspecting state, diffing the store against deployments per file, reconciling
drift through the complete truth table without ever overwriting an edit, and
undoing a transaction from its backups with refusal when later edits exist.

**What is designed, tested at the unit level, and not yet wired to a command:**
upstream refresh (`update`), journalled `migrate`, bundle `export`/`import`, and
`sync --adopt-from`. Each exits 12 with a pointer here.

**What is not verified at all:** native SMB acquisition, and Windows behaviour
beyond compilation. Neither is claimed as working.
