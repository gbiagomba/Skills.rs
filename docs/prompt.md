# Build `skill`: a Rust cross-agent skill manager

You are a senior Rust engineer building a production-quality, cross-platform CLI. Implement the project described below in the current repository. This is an implementation request, not a request for a conceptual answer or a skeleton.

First inspect the repository and any AGENTS.md/CLAUDE.md instructions, preserve unrelated work, research the few external details that require verification, and write a concise implementation plan. Then implement, test, and document the application in coherent milestones. Make reasonable reversible decisions and record them. Ask only when authorization or a genuinely blocking product decision is required. Do not stop after the plan or declare unfinished requirements complete.

## 1. Product and scope

Working project name and executable: `skill`.

Build a Rust CLI that discovers installed coding agents and manages reusable Agent Skills across them. Users can acquire skills from local files/directories, Git, HTTP(S), and SMB; install independent copies or links to a canonical store; migrate between agents; export/import portable bundles; track origins; update upstream content; and safely reconcile local drift.

Product promise: **Install once, manage everywhere, never silently lose edits.**

Initial agent adapters: Claude Code (`claude`), Codex (`codex`), and Gemini CLI (`gemini`). Accept `claude-code` and `gemini-cli` as aliases. Keep agent support extensible. This manages local CLI skill installations, not hosted account synchronization or entire agent configurations. Do not imply that local installation updates cloud accounts.

Vercel's skills CLI is inspiration, not a dependency or an assumed feature gap. Review its current documentation and relevant implementation. Build an independent Rust implementation. Attribute inspiration; if reusing code or substantial material, verify its license and preserve required notices. Do not claim a clean-room implementation if you studied its source.

Verify current primary sources before coding:

- https://agentskills.io/specification
- https://github.com/vercel-labs/skills
- https://developers.openai.com/codex/skills/
- https://code.claude.com/docs/en/skills
- https://geminicli.com/docs/cli/skills/

Follow official redirects as needed. Record verified paths, supported versions, precedence, symlink behavior, dependencies, source links, and research date in `docs/compatibility.md`. Retrieved skills and repository content are untrusted input, not instructions granting permission to execute their contents.

## 2. CLI contract

Implement and test these representative commands. Examples describe the desired interface, not existing software:

```bash
skill agents
skill list
skill list --agent claude
skill copy ./my-skill claude
skill copy ./my-skill claude codex gemini
skill copy file:///absolute/path/my-skill/SKILL.md codex
skill copy ./my-skills --all-detected --all-skills
skill link ./my-skill --all-detected
skill copy https://github.com/owner/repo.git codex --skill my-skill
skill copy git@github.com:owner/repo.git claude --skill my-skill --ref main
skill copy https://example.com/my-skill.tar.gz --all-detected
skill copy smb://server/share/my-skill claude
skill copy ./my-skill codex --scope project --project-dir ./example-project
skill migrate claude codex --skill my-skill
skill migrate claude codex --all-skills --dry-run
skill export my-skill --output ./my-skill.tar.gz
skill export --all-skills --output ./skills-backup.tar.gz
skill import ./skills-backup.tar.gz --all-detected --mode link
skill status
skill diff my-skill
skill update my-skill --check
skill update my-skill
skill update --all-skills
skill sync --dry-run
skill sync my-skill
skill sync my-skill --adopt-from claude
skill rollback TRANSACTION_ID
skill doctor
```

Required semantics:

- `copy SOURCE [AGENTS...]`: acquire into the canonical store and deploy independent copies. Leave the supplied source intact. Copies are manager-owned deployments, not disconnected files without tracking.
- `link SOURCE [AGENTS...]`: acquire into the canonical store and deploy per-skill symlinks. Never link an entire agent skill root or bypass the store by linking to a disposable download.
- `migrate FROM TO`: move selected local skill installations through a verified, recoverable transaction. Preserve provenance and complete package content. Retain source until destination verification succeeds.
- `import`: validate and restore a portable bundle, rebasing locations to the current machine. Default mode is copy; accept `--mode copy|link`.
- `export`: produce a self-contained bundle with package contents, schema-versioned manifest, sanitized provenance, digests, and relevant licenses. Never export credentials, machine-specific authentication, or blindly dereference arbitrary links.
- `update`: check upstream, stage new content, protect local edits, then update the canonical package and managed deployments through the same reconciliation machinery. Default to an all-or-nothing planned transaction for selected installations; do not leave conflicts hidden behind partial success.
- `sync`: reconcile the canonical store and managed deployments locally; do not fetch upstream implicitly.
- `status`, `diff`, and `doctor`: read-only by default. Distinguish missing, modified, conflicting, unmanaged, unsupported, and broken-link states. Do not claim upstream is current without a check.
- `rollback`: use recorded backups, but refuse to overwrite edits made after the original transaction without explicit conflict handling.

Common options where relevant: `--scope user|project` (default user), `--project-dir`, `--store`, `--config`, `--dry-run`, `--yes`, `--json`, `--verbose`, and `--offline`. Include `--help` and `--version`. `--skill` is repeatable. Use `--all-skills` for package selection and `--all-detected` for destination selection; do not overload one `--all` flag with both meanings.

Without agent arguments, offer detected destinations interactively and show the exact paths. Without a mode decision in an import interaction, offer copy/link. In noninteractive use, missing selections must fail with actionable instructions unless configured defaults exist. `--yes` only accepts a fully determined plan: it does not authorize conflict overwrites, insecure transports, or arbitrary deletion. Explicit target agents may be configured even if their executable is absent; report that condition.

Define stable documented exit codes, including conflict, invalid package, authentication/network failure, and incomplete recovery. JSON output must be schema-versioned, machine-readable, and free of interleaved progress text; send diagnostics to stderr.

## 3. Package discovery and agent compatibility

A skill is a package directory containing `SKILL.md` and potentially scripts, references, assets, templates, and other files. Preserve all legitimate package content, executable bits where supported, and relative layout. Do not copy an entire unrelated repository or a home directory merely because a file was supplied.

Accept skill directories, collections, repositories, archives, and standalone Markdown skill files. When given a local `SKILL.md` within a recognized package, import that package boundary. For a truly standalone file, create a package and warn about unresolved relative references; do not recursively collect arbitrary neighboring files. A standalone remote file does not authorize following every URL in its instructions.

Validate frontmatter and required fields against the current specification. Preserve unknown fields. Package names are untrusted; reject unsafe path components, traversal, reserved names, and cross-platform collisions. Use an internal stable identity derived from source plus package selector, not display name alone. Never silently replace same-named skills from different origins; require an explicit destination alias or selection.

Each agent adapter owns discovery, scopes, path overrides, precedence, alias locations, and capabilities. Report detection evidence instead of treating a leftover directory as proof of installation. Resolve physical-path aliases and duplicate targets so the same directory is not written twice.

Account for shared discovery roots. If a shared path makes a skill visible to unselected agents, disclose that visibility; prefer agent-specific paths where supported, and never promise isolation that the agent's discovery rules cannot provide.

Classify plugin-managed, organization-managed, built-in, and account-synced skills separately. Do not modify or remove those as ordinary personal installations. For existing user-authored unmanaged skills, require explicit adoption before ongoing management; selecting one for migration may authorize scoped adoption after showing the plan. Unknown provenance remains unknown rather than being guessed.

Compatibility checks should flag known agent-specific frontmatter, tool references, hooks, MCP requirements, hardcoded paths, and unsupported symlink behavior. Distinguish confirmed incompatibility from heuristic warnings. Do not silently rewrite prompts or translate security permissions. Byte preservation is not a guarantee that a skill behaves identically in another agent.

## 4. Sources and transports

Provide pluggable backends with capabilities such as fetch, check revision, and authentication support:

1. Filesystem: relative/absolute paths, `file://` URIs, mounted shares, and supported native Windows/UNC paths. Parse each correctly.
2. Git: HTTPS, SSH, and SCP-style Git addresses; repository subdirectories; branch/tag/commit selectors. Store both the requested ref and resolved commit. Distinguish tracking refs from immutable pins; updates must not silently advance a pinned commit.
3. HTTP(S): standalone skills and `.zip`, `.tar`, `.tar.gz`, or `.tgz` packages. Verify content rather than trusting extensions. HTTPS verification stays enabled. Plain HTTP requires explicit `--allow-http`; never silently downgrade HTTPS on redirects.
4. SMB: actual `smb://server/share/path` acquisition, not merely URL acceptance. Prefer a maintainable secure implementation; a clearly documented optional dependency/backend is acceptable if necessary. Support mounted shares immediately, but do not call mounted-path handling native SMB support. Report exact platform limitations and test evidence.

Do not implement every conceivable protocol. Reject unsupported schemes clearly. Clarify ambiguous sources instead of running arbitrary protocol helpers. Use existing configured Git/SSH credentials and secure OS/backend authentication mechanisms. Do not put passwords on command lines, in URLs, manifests, exports, logs, or crash reports. Avoid shell interpolation. Do not enable SMB1 or guest/insecure fallback automatically. Make recursive Git submodules and LFS execution opt-in, not implicit.

HTTP validators and SMB timestamps are optimization hints, not proof of content identity. Compute package hashes. Offline mode must not attempt network access and must explain unavailable cached revisions.

## 5. Store, provenance, and state

Default canonical store: `~/skills`, configurable through CLI/config. Resolve the home directory using platform APIs; do not repurpose the HOME environment variable. Use appropriate platform config/state/cache directories for manager metadata. Keep state out of skill directories.

Persist a schema-versioned transactional state store (SQLite is a reasonable default; justify another choice). Track:

- Stable package ID, display/install names, sanitized source locator and source type.
- Repository-relative package path; requested ref; resolved revision; pin/tracking policy.
- Last fetched pristine upstream snapshot and digest.
- Current canonical snapshot and digest, including local edits.
- Each destination's agent, scope, resolved path, copy/link mode, and deployment baseline.
- Per-file hashes, relative paths, relevant file types/modes, timestamps for auditing only.
- Acquisition/check/deployment times, transaction history, backups, and ownership.
- Whether an imported origin is trusted/approved for future network refresh.

Use deterministic hashes over complete package trees, not only SKILL.md or modification times. Preserve raw file bytes. Retain pristine upstream and per-destination baselines separately: they solve different three-way comparison problems. Stage a coherent snapshot and detect concurrent modification rather than hashing one version and deploying another.

Imported manifests are untrusted. Do not obey embedded absolute destinations, automatically trust their source URLs, restore credentials, or execute anything they reference. Export with clear warnings about potentially private skill content and source metadata.

## 6. Update and synchronization rules

Implement conflict-aware comparisons, not last-writer-wins:

| Compared with the last shared baseline | Default behavior |
| --- | --- |
| Canonical changed; deployed copy unchanged | Safely update deployed copy |
| Deployed copy changed; canonical unchanged | Preserve edits; report drift; require explicit adoption or restore decision |
| Both changed differently | Conflict; do not overwrite either |
| Both changed to identical content | Refresh baseline without needless rewriting |
| Deployment missing | Report missing; require explicit repair rather than infer global deletion |
| Neither changed | No-op |

Use the pristine upstream baseline to compare upstream updates with canonical local edits. Never overwrite edits just because upstream has a newer timestamp or tag. V1 may stop on conflicting packages instead of implementing automatic textual merges; reliable conflict handling is mandatory, automatic merging is not.

`sync --adopt-from AGENT` explicitly promotes selected destination content into the canonical store, then reconciles other targets while preserving their independent conflicts. If scope/path or multiple source copies are ambiguous, require disambiguation. Do not promote one agent's edits to all others automatically.

For links, edits through a destination already edit canonical content. Sync validates link targets and ownership; update still compares local edits against pristine upstream. If linking is unavailable, fail with an actionable copy alternative; never silently change installation mode.

Define deletion semantics explicitly. Whole-package removal is never inferred from a missing directory. File deletions within a changed package count as changes and participate in conflict checks. Every destructive resolution needs a specific decision, not blanket `--yes`.

## 7. Transactions, migration, and security

Plan, acquire, validate, compare, stage, back up, apply, verify, and commit state. Reuse this pipeline across commands.

Use per-target atomic replacement where supported, staging on the target filesystem as necessary. Multiple directories/filesystems are not globally atomic: implement an operation journal, locking, failure recovery, and truthful partial-failure reporting. Filesystem and database commits must be recoverable across crashes. Revalidate target fingerprints before mutation to detect changes since planning. Recovery and rollback must not destroy subsequent user edits.

Migration removes only verified selected source installations after successful destination deployment. Moving a symlink removes the link, not its canonical target. Never delete a shared discovery directory to hide a skill from one agent. If both agents resolve to the same physical target, report a no-op/shared-target limitation instead of deleting it. Preserve licenses and supporting resources, and retain recoverable source backups.

Security requirements:

- Never execute downloaded skills, scripts, hooks, or install commands during acquisition/validation.
- Defend extraction and copying against traversal, absolute paths, escaping symlinks, hardlinks, device files, archive bombs, duplicate entries, case collisions, reserved Windows paths, and link cycles.
- Establish and enforce package and destination containment, including existing ancestor symlinks/reparse points. Test adversarial path changes where feasible; document residual platform limits.
- Bound bytes, extracted size, file count, depth, redirects, retries, and timeouts.
- Preserve safe internal links only when validated and supported; otherwise reject clearly. Never follow links into unrelated private files.
- Treat supplied hosts as explicit fetch targets, not permission to forward credentials across redirects or contact arbitrary endpoints embedded in a package. Support intentional private Git/SMB sources without blanket private-address blocking.
- Refuse unsupported special files and unsafe permission bits. Use private permissions for state and backups.
- Never overwrite unmanaged destinations by default. Never delete broad agent/config/home directories.
- Keep structured operational/audit logs with redaction; do not log full skill contents or secrets by default. No telemetry by default.
- A hash proves integrity relative to a recorded value, not publisher trust or skill safety. Document that distinction.

`--dry-run` must never change installations, canonical content, persistent state, backups, or config. For remote sources it may use isolated temporary staging to compute a plan, disclose network activity, and clean it up. Never present an unperformed operation as verified.

## 8. Rust implementation and repository deliverables

Use current stable Rust-compatible dependency versions verified at implementation time. Include Cargo.lock, declare and test an MSRV, and avoid obsolete API assumptions. Prefer well-maintained dependencies and a small dependency surface. Use typed errors and actionable diagnostics; avoid unwrap/expect in normal user-input paths. Do not add unsafe Rust unless necessary and justified.

Suggested module boundaries: CLI, configuration, package validation, source backends, agent adapters, state/provenance, diff/reconciliation, transactions/recovery, security/path operations, and output/logging. A single crate with clear modules is fine; avoid unnecessary micro-crates or plugin machinery.

Cargo package metadata:

- name: `skill` (working name; do not publish without approval)
- version: `1.0.0` target; do not claim release readiness until acceptance passes
- authors: `Gilles Biagomba <gilles.infosec@gmail.com>`
- edition: `2021`
- license: `GPL-3.0-only`
- description: `A cross-agent skill manager with safe copying, linking, migration, updates, and synchronization.`
- repository: `https://github.com/gbiagomba/skill` (intended URL, not a claim that it exists)
- appropriate validated Cargo keywords/categories
- binary name: `skill`; path: `src/main.rs`

Deliver complete source, Cargo.toml, Cargo.lock, .gitignore, GPLv3 license text, relevant attribution notices, tests/fixtures, example config, and:

1. README with Background/Lore, Table of Contents, Features, Installation (GitHub Releases/Cargo/Source), Flags, Usage, Running Tests, Docker, Makefile, Contributing, and License. Include source examples, copy/link/migration distinctions, update/sync conflicts, credentials, supported-agent/platform matrix, recovery, and limitations. Do not imply Cargo publication or release downloads exist before they do.
2. Makefile: build, install, run with ARGS, test, clean, fmt, lint. Use PROJECT_NAME := skill. Keep direct Cargo commands documented for platforms without make.
3. Multi-stage Dockerfile with a Rust builder, slim supported Debian runtime, CA certificates, non-root runtime user, `skill` entrypoint, and default `--help`. Install Git/SMB helpers only when the chosen backends need them. Explain that container agent discovery sees the container, not the host; require deliberate volume/path mappings and warn about host-invalid symlinks.
4. GitHub Actions using actions/checkout@v4 as explicitly requested. Run formatting, lint, and tests, plus builds on Linux/macOS/Windows. Use currently supported artifact/release action versions verified at implementation time. Upload build artifacts on every successful CI build. Create public releases for `v*` tags after the complete matrix passes; do not publish a release on every PR. Name assets by OS/architecture, include SHA-256 checksums, validate tag/package version, and grant write permissions only to the release job. Keep untrusted PR jobs read-only. Do not actually push tags or publish releases without approval.
5. docs/architecture.md, docs/compatibility.md, docs/security.md, and a concise implementation checklist with completed versus pending requirements.

## 9. Acceptance tests

Tests must use isolated temporary homes, stores, agent roots, local Git repositories, and fixture HTTP servers. Never mutate the developer's real installations or use personal credentials. Do not overwrite HOME in your working shell; use injected directory configuration or tightly scoped child-process environments for tests.

Cover at minimum:

- All documented command parsing, aliases, selection behavior, JSON, and exit codes.
- Full-package copies and links to all three adapters; agent discovery/path precedence and overrides.
- Same physical/shared destination deduplication and visibility warnings.
- Multiple skills per source; standalone file handling; missing assets; same-name collisions.
- Git branch refresh, immutable pins, package subpaths, unknown origins, unavailable source, and offline behavior.
- HTTP archives, bounded redirects/downloads, denied plain HTTP without opt-in, and safe authentication handling.
- Native SMB acquisition through an isolated optional integration fixture, including authentication failure; mounted shares tested separately. Do not mark native SMB verified solely from mocks.
- Complete sync truth table, identical edits, deleted files, missing deployments, divergent destinations, and explicit adoption.
- Upstream changes with clean canonical content and with local canonical modifications, including linked edits.
- Successful and failed migration; destination failure leaves source intact; shared-path migration cannot delete canonical/shared data.
- Unmanaged destination refusal, plugin-managed exclusion, symlink escape, malicious archive, corrupt state, and schema migration.
- Concurrent invocation, concurrent file modification, interrupted transaction, rollback, and edits after a transaction.
- Export/import round trip preserving contents/modes/provenance while excluding credentials and rebasing paths.
- Repeating a successful operation is idempotent; dry-run preserves managed state byte-for-byte.

Run cargo fmt --check, cargo clippy --all-targets with warnings denied, and cargo test --locked. Test feature combinations intentionally; do not require mutually incompatible/platform-specific features together. Verify supported targets through CI rather than claiming local cross-platform execution. Add dependency/license checks where practical and document findings honestly.

## 10. Execution milestones and final handoff

Implement in this order, continuing through the full scope when possible:

1. CLI, state, validation, isolated fixtures, local sources, three agent adapters, copy/link.
2. Git/HTTP(S), provenance, immutable pins, safe downloads/extraction.
3. Drift-aware update/sync, journaled migration, recovery, rollback.
4. Portable import/export and native SMB backend/integration checks.
5. Documentation, packaging, CI, and end-to-end acceptance.

If a dependency, platform, approval, or network restriction blocks a milestone, finish independent authorized work and document the exact blocker. Do not substitute placeholder success, silently drop SMB, or call the whole product complete when only local copying works. Do not publish, push, change real agent installations, or start background watchers as part of testing.

Final response: summarize implemented behavior, exact validation commands and results, unverified platforms/backends, any pending requirements, and a short safe quickstart. Keep documentation and --help consistent with actual behavior. Begin with repository inspection and the implementation plan, then build.
