# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> **Note on version history.** Versions 1.0.0 through 1.0.2 in this repository's
> earlier history belonged to the project **template** this repository was created
> from, not to `skill`. The 1.0.0 entry below is `skill`'s own first version.

## [Unreleased]

### Fixed

- Two tests hardcoded POSIX paths and failed on the Windows CI runner.
  `classifies_the_documented_special_directories` now asks `managed_roots()` for
  the platform's managed-settings location instead of asserting the Linux one,
  and `parses_a_file_uri` uses a drive-qualified URI on Windows plus a new
  `a_driveless_file_uri_is_refused_on_windows` test documenting that a driveless
  `file://` URI is correctly refused there. The implementation was right in both
  cases; only the tests assumed Unix

### Planned

- Git backend: HTTPS, SSH, and SCP-style addresses, repository subdirectories,
  requested ref versus resolved commit, and immutable pins
- HTTP(S) backend: standalone files and `.zip`/`.tar`/`.tar.gz`/`.tgz` archives
  with content sniffing and bounded redirects
- Native SMB acquisition via the `smb2` backend, plus an isolated integration fixture
- `update`, `migrate`, `export`, `import`, and `sync --adopt-from`

## [1.0.0] - 2026-10-07

First version of `skill`, a cross-agent skill manager.

### Added

- **Agent adapters** for Claude Code, Codex, and Gemini CLI, each owning its own
  discovery roots, precedence order, capabilities, and provenance classification.
  Accepts the `claude-code` and `gemini-cli` aliases. Precedence is per adapter
  because the three agents genuinely disagree: Claude Code resolves enterprise
  over personal over project, Gemini CLI lets the highest tier win, and Codex
  keeps both same-named skills
- **Shared-root disclosure.** `~/.agents/skills` is Codex's documented user root
  and simultaneously one of Gemini CLI's, and Codex has no non-deprecated
  agent-specific alternative. Every write into a shared root discloses which other
  agents can see it and states that isolation is unavailable, rather than implying
  otherwise. Two agents resolving to one physical directory are written once
- **Detection by evidence**, not a boolean: a leftover skills directory is
  reported as "directory present, executable absent"
- **Canonical store** at `~/skills`, with manager state kept in the platform data
  directory so it is never hashed as package content
- **`copy`** and **`link`** from local paths and `file://` URIs, covering single
  packages, collections, standalone Markdown files, and a `SKILL.md` inside a
  package (which imports the whole package boundary)
- **Three-way reconciliation** over separate upstream, canonical, and
  per-destination baselines, implementing the full documented truth table.
  `status`, `diff`, and `sync` share one pure comparison function
- **`doctor`** and **`rollback`**, with journalled transactions, per-target atomic
  replacement, backups before any destructive step, crash recovery, and an
  exclusive lock so concurrent runs cannot race
- **Schema-versioned SQLite state store** recording stable package identity,
  sanitized locators, requested ref and resolved revision, pin policy,
  per-destination deployment baselines with per-file digests and modes,
  transaction history, and backups
- **Hardened path handling**: traversal, absolute entries, drive letters and
  alternate data streams, reserved Windows device names, trailing dots and
  spaces, case collisions, escaping symlinks, ancestor symlinks and reparse
  points, device and FIFO nodes, setuid/setgid/sticky bits, and bounded size,
  count, and depth. Containment is re-verified immediately before each mutation
- **Stable documented exit codes** (0 to 12), with the `--help` epilogue generated
  from the same table so documentation cannot drift from behaviour
- **Schema-versioned JSON output**: one object on stdout, diagnostics on stderr,
  and a `dry_run` field so a plan cannot be mistaken for a performed operation
- **Layered configuration**: defaults, config file, `SKILL_*` environment, then
  flags. See `examples/config.toml`
- Documentation: `docs/architecture.md`, `docs/compatibility.md` (every vendor
  claim cited with its 2026-10-07 retrieval date), `docs/security.md`,
  `docs/checklist.md` (honest per-requirement status), and `ATTRIBUTION.md`
- 240 tests: 201 unit, 18 CLI acceptance, 21 safety acceptance. Every test builds
  an isolated fake machine and never touches a real agent installation

### Security

- **Nothing acquired is ever executed.** No scripts, hooks, or install steps run
  during acquisition or validation
- An existing **unmanaged** destination is never overwritten (exit 11), and a
  destination we **do** manage is not overwritten when it has been edited since we
  wrote it (exit 3). `--yes` overrides neither
- Plugin-managed, org-managed, built-in, and account-synced skills are classified
  separately and left alone. Unknown provenance stays unknown
- HTTPS verification is always on; plain HTTP requires `--allow-http`, which
  `--yes` does not grant; an HTTPS to HTTP redirect is always refused
- Credentials never appear on a command line, in a stored locator, a manifest, an
  export, a log, or an error message. Userinfo, query, and fragment are stripped
  before anything is persisted
- State, journals, and backups are created mode 0600 in 0700 directories
- `rollback` refuses to discard edits made after the transaction it is undoing
- No telemetry, and no network call that was not requested
- Documented plainly: a digest proves integrity against a recorded value, not
  publisher trust or skill safety

### Changed

- Replaced the project template's placeholders with real content: `Cargo.toml`,
  `Makefile` (upgraded to the Pro variant, plus `msrv` and `audit`), `Dockerfile`
  (multi-stage, non-root, no git or samba packages needed since both backends are
  pure Rust), the CI workflow, `README.md`, and the install scripts
- `.gitignore` no longer excludes `Cargo.lock`: this is a binary crate and
  reproducible builds require it committed
- CI: added MSRV and feature-matrix jobs, tag-versus-`Cargo.toml` validation,
  artifact upload on every successful build rather than only on tags, and a
  top-level read-only permission with `contents: write` scoped to the release job

### Removed

- `ChatGPT_AGENTS.md`, `Claude_AGENTS.md`, and `Orginal_AGENTS.md`, which
  `AGENT.md` marked as reference-only and superseded
- `.version-tracking-template.md`, whose only purpose was to seed
  `.version-tracking.md`
- A committed `.greprules/plugin-data/` tool log, now git-ignored

### Known limitations

- `migrate`, `update`, `export`, `import`, and `sync --adopt-from` are documented
  and designed but **not implemented**; they exit **12** and point at
  `docs/checklist.md`. They never report success
- The Git, HTTP(S), and native SMB backends are **not implemented**; their
  locators parse and classify correctly, and acquisition exits 12
- Native SMB is **not verified**. Mounted shares work through the filesystem
  backend and are deliberately labelled mounted-path support, not native SMB
- Windows behaviour is verified only to the extent that it compiles in CI;
  adversarial path tests have run on macOS only

---

**AGENT NOTE:** Update this file before EVERY release as per RULE 5
