# skill

> VERSION: 1.0.0
> DESCRIPTION: A cross-agent skill manager with safe copying, linking, migration, updates, and synchronization.
> AUTHOR: Gilles Biagomba
> LICENSE: GPL-3.0-only (dual-licensed, see [License](#-license))

**Install once, manage everywhere, never silently lose edits.**

---

## 🗿 Background / Lore

Agent Skills spread fast. A skill you wrote for Claude Code is useful in Codex and
in Gemini CLI, so you copy the directory across, and now you have three copies
drifting apart in three places with no record of where any of them came from. The
first tool that "helpfully" re-syncs them deletes the fix you made last Tuesday.

`skill` is the boring, paranoid alternative. It keeps one canonical copy, records
where it came from and exactly what it wrote to every destination, and compares
three separate baselines before it changes anything. When your edits and an
update disagree, it stops and tells you, instead of picking a winner.

It is named for what it manages and it does one job. It never runs the skills it
installs, and it will refuse rather than guess.

---

## 📖 Table of Contents

- [Background / Lore](#-background--lore)
- [Features](#-features)
- [What this build does and does not do](#-what-this-build-does-and-does-not-do)
- [Installation](#-installation)
  - [Using GitHub Releases](#using-github-releases)
  - [Using Cargo](#using-cargo)
  - [Compiling From Source](#compiling-from-source)
- [Flags](#-flags)
- [Usage](#-usage)
  - [Sources](#sources)
  - [Copy, link, and migrate](#copy-link-and-migrate)
  - [Update and sync conflicts](#update-and-sync-conflicts)
  - [Credentials](#credentials)
  - [Recovery](#recovery)
  - [Exit codes](#exit-codes)
  - [JSON output](#json-output)
  - [Configuration](#configuration)
- [Supported agents and platforms](#-supported-agents-and-platforms)
- [Limitations](#-limitations)
- [Running Tests](#-running-tests)
- [Using Docker](#-using-docker)
- [Using the Makefile](#-using-the-makefile)
- [Contributing](#-contributing)
- [License](#-license)

---

## ✨ Features

- **One canonical store.** Every managed skill lives once in `~/skills`; agents get
  independent copies or per-skill symlinks pointing at it.
- **Three-way conflict detection.** Separate baselines for upstream, the store,
  and each destination, so a local edit is never mistaken for stale content.
- **Refuses rather than guesses.** An unmanaged destination, a drifted
  deployment, a conflict, or an ambiguous selection stops the operation with an
  actionable message.
- **Honest agent modelling.** Each adapter encodes its own documented precedence,
  discovery depth, and caveats, because the three agents genuinely disagree.
- **Shared-root disclosure.** Codex's user root is shared with Gemini CLI;
  `skill` says so rather than implying isolation it cannot deliver.
- **Journalled transactions.** Per-target atomic replacement, a backup before any
  destructive step, crash recovery, and a `rollback` that refuses to discard work
  you did afterwards.
- **Nothing acquired is ever executed.** No scripts, hooks, or install steps run
  during acquisition or validation.
- **Machine-readable output.** `--json` emits one schema-versioned object on
  stdout, with diagnostics on stderr.

---

## 🚧 What this build does and does not do

Stated up front so nothing here overclaims. Full detail in
[`docs/checklist.md`](docs/checklist.md).

**Working end to end:** `agents`, `list`, `copy`, `link`, `status`, `diff`,
`sync`, `rollback`, `doctor`, from local paths and `file://` URIs.

**Documented, designed, and not implemented in this build:** `migrate`, `update`,
`export`, `import`, `sync --adopt-from`, and the Git, HTTP(S), and native SMB
backends. These exit with code **12** and a pointer to the checklist. They do not
silently no-op and they never report success.

**Not verified at all:** native SMB acquisition, and Windows behaviour beyond
compilation.

There are no published releases and the crate is not on crates.io yet, so the
release and `cargo install` instructions below describe what will work once
published, not something that exists today.

---

## 📦 Installation

### Using GitHub Releases

> No release has been published yet. Once one is, assets are named by OS and
> architecture with a SHA-256 checksum beside each.

| Platform | Architecture | Binary |
|----------|-------------|--------|
| Linux | x64 | `skill-linux-x64` |
| Linux | ARM64 | `skill-linux-aarch64` |
| macOS | x64 | `skill-macos-x64` |
| macOS | ARM64 | `skill-macos-aarch64` |
| Windows | x64 | `skill-windows-x64.exe` |
| Windows | ARM64 | `skill-windows-aarch64.exe` |

```bash
# Verify the checksum before trusting the binary.
sha256sum -c skill-linux-x64.sha256
chmod +x skill-linux-x64
sudo mv skill-linux-x64 /usr/local/bin/skill
```

### Using Cargo

> Not published to crates.io. Install from the repository:

```bash
cargo install --git https://github.com/gbiagomba/Skills.rs --locked
```

### Compiling From Source

Requires Rust **1.88** or newer (the declared MSRV, verified in CI).

```bash
git clone https://github.com/gbiagomba/Skills.rs
cd Skills.rs
cargo build --release --locked
# Binary at target/release/skill
```

No system libraries are needed. SQLite is compiled in, and the Git and SMB
backends are pure Rust.

### Using Install Scripts

```bash
# Linux/macOS/Unix
./scripts/install.sh
```
```powershell
# Windows, as Administrator
.\scripts\install.ps1
```

---

## 🚩 Flags

Global, accepted by every subcommand:

```
      --store <DIR>        Canonical store location (default: ~/skills)
      --config <FILE>      Configuration file to read
      --dry-run            Show what would change without changing anything
  -y, --yes                Accept a fully determined plan without prompting
      --json               Emit one schema-versioned JSON object on stdout
  -v, --verbose            Include per-file detail in human-readable output
      --offline            Make no network access at all
  -h, --help               Show help
  -V, --version            Show version
```

Destination and package selection:

```
      --scope <user|project>   Where the deployment goes (default: user)
      --project-dir <DIR>      Required with --scope project
      --skill <NAME>           Select a package. Repeatable
      --all-skills             Select every package in the source
      --all-detected           Select every detected agent as a destination
      --as <NAME>              Install under a different name, to resolve a collision
      --ref <REF>              Git branch, tag, or commit
      --allow-http             Permit plain HTTP. Never implied by --yes
      --adopt-from <AGENT>     (sync) Promote a destination into the store
      --check                  (update) Report upstream state, change nothing
      --mode <copy|link>       (import) Deployment mode
  -o, --output <FILE>          (export) Bundle to write
```

`--all-skills` selects **packages**; `--all-detected` selects **destinations**.
They are separate flags on purpose: one `--all` meaning both would be ambiguous
in exactly the cases where it matters.

---

## 🛠️ Usage

```bash
skill --help
skill agents                      # what is installed, and where each writes
skill copy ./my-skill claude      # acquire into the store, deploy a copy
skill status                       # what has drifted
```

### Sources

```bash
skill copy ./my-skill claude                                  # relative path
skill copy /abs/path/my-skill claude                          # absolute path
skill copy file:///abs/path/my-skill/SKILL.md codex           # file:// URI
skill copy ./my-skills --all-skills --all-detected            # a collection
skill copy 'C:\Users\me\skills\my-skill' claude                # Windows path
skill copy '\\server\share\my-skill' claude                    # UNC or mounted share

# Not implemented in this build (exit 12):
skill copy https://github.com/owner/repo.git codex --skill my-skill
skill copy git@github.com:owner/repo.git claude --skill my-skill --ref main
skill copy https://example.com/my-skill.tar.gz --all-detected
skill copy smb://server/share/my-skill claude
```

Pointing at a `SKILL.md` inside a real package imports **that whole package**,
not the single file. A standalone `.md` file becomes a one-file package and warns
that its relative references cannot be resolved. A bare `owner/repo` is treated
as a **path**, never silently expanded into a network fetch.

### Copy, link, and migrate

| Mode | What it does | When it fits |
|---|---|---|
| `copy` | Acquires into the store, then writes an **independent tree** per destination. Each is tracked with its own baseline. | You want agents to diverge, or an agent that mishandles links. |
| `link` | Acquires into the store, then creates a **per-skill symlink** to it. | You want one source of truth. An edit through the link *is* a store edit. |
| `migrate` | Moves selected installations between agents through a verified, recoverable transaction. **Not in this build.** | Consolidating onto one agent. |

`link` never links an entire agent skills root, and never links to the source you
handed it: the link always points into the store, so the store stays
authoritative. All three agents document symlink support.

### Update and sync conflicts

`sync` reconciles the store with its deployments **locally**. It never contacts
upstream; `update` does that. `status` says so explicitly rather than letting you
assume it checked.

| Store vs the last shared baseline | Deployment vs that baseline | What happens |
|---|---|---|
| unchanged | unchanged | nothing |
| changed | unchanged | deployment updated |
| unchanged | changed | **edits preserved**, drift reported |
| changed | changed, differently | **conflict, nothing written** (exit 3) |
| changed | changed, identically | baseline refreshed, nothing rewritten |
| any | missing | reported; repair is explicit |

A deleted file inside a package is a change and takes part in conflict detection.
A missing directory **never** implies the package was uninstalled.

Re-running `copy` over a deployment you have edited **refuses** (exit 3) rather
than discarding your edits, and `--yes` does not override that.

```bash
skill diff my-skill -v      # see both sides
skill sync --dry-run        # plan, change nothing
```

### Credentials

`skill` never prompts for, stores, or transmits a password.

- Git uses your existing configuration: `git credential` helpers for HTTPS and
  your system `ssh` for SSH and SCP-style addresses.
- SMB uses secure OS and backend mechanisms. SMB1 and guest fallback are never
  enabled.
- A password or token is **never** accepted on a command line, and any userinfo,
  query, or fragment is stripped from a locator before it is stored, exported, or
  logged.
- HTTPS verification is always on. Plain HTTP needs `--allow-http`, an HTTPS to
  HTTP redirect is always refused, and credentials are never forwarded across a
  redirect or to a host named inside package content.

### Recovery

```bash
skill doctor                   # unfinished transactions, broken links, missing packages
skill rollback <TRANSACTION_ID>
```

Every mutating run takes an exclusive lock (a second run exits 9), writes a
journal entry before and after each mutation, and backs up anything it is about
to replace. A command refuses to start while a previous run is unfinished.
`rollback` restores from those backups and **refuses** if the destination has
been edited since, so recovery cannot destroy later work.

Several destinations may live on several filesystems, so there is no globally
atomic apply. Each target is replaced atomically, and a partial result is
reported as partial (exit 8), never as success.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | unexpected internal error |
| 2 | usage error |
| 3 | conflict requiring an explicit decision |
| 4 | invalid or unsafe package |
| 5 | source, network, or authentication failure |
| 6 | destination agent unknown, undetected, or unsupported |
| 7 | state corrupt or schema version unsupported |
| 8 | partial failure, recovery incomplete |
| 9 | lock held by a concurrent invocation |
| 10 | operation requires network but `--offline` is set |
| 11 | refused: destination needs explicit adoption |
| 12 | documented but not implemented in this build |

### JSON output

`--json` writes exactly one object to stdout, with all diagnostics on stderr:

```json
{
  "schema": "skill.v1",
  "command": "list",
  "ok": true,
  "status": "success",
  "dry_run": false,
  "data": [ ... ],
  "notes": [ "..." ]
}
```

`dry_run` is present so a plan can never be mistaken for a performed operation.

### Configuration

See [`examples/config.toml`](examples/config.toml). Precedence, lowest to
highest: defaults, the config file, environment variables, command-line flags.

| Variable | Effect |
|---|---|
| `SKILL_STORE` | canonical store location |
| `SKILL_CONFIG` | config file path |
| `SKILL_STATE_DIR` | state, journals, backups |
| `SKILL_CACHE_DIR` | download and staging cache |
| `SKILL_OFFLINE` | `1` refuses all network access |
| `SKILL_NO_INTERACTIVE` | `1` never prompts |
| `SKILL_AGENT_HOME` | base that agent skill roots resolve against. Defaults to the platform home directory, resolved through the OS API rather than `$HOME`. Intended for testing and unusual profiles |

---

## 🤖 Supported agents and platforms

| Agent | id | Aliases | User skills path | Isolation |
|---|---|---|---|---|
| Claude Code | `claude` | `claude-code` | `~/.claude/skills/` | yes |
| Codex | `codex` | `codex-cli` | `$HOME/.agents/skills/` | **no, shared with Gemini CLI** |
| Gemini CLI | `gemini` | `gemini-cli` | `~/.gemini/skills/` | yes |

`~/.agents/skills` is Codex's documented user root **and** one of Gemini CLI's.
Codex has no non-deprecated agent-specific alternative, so a user-scope Codex
install is unavoidably visible to Gemini. `skill` discloses this on every such
write instead of promising isolation it cannot provide. Paths, precedence, and
per-agent caveats are cited with retrieval dates in
[`docs/compatibility.md`](docs/compatibility.md).

| Platform | Build | Behaviour verified |
|---|---|---|
| macOS ARM64 | yes | yes, the full suite ran here |
| Linux x64 | CI | by CI only |
| macOS x64 | CI | by CI only |
| Windows x64 / ARM64 | CI | **compilation only.** Path rules are implemented and unit tested, but adversarial path behaviour has not been exercised on Windows |

---

## ⚠️ Limitations

- **A hash proves integrity against a recorded value.** It says nothing about
  publisher identity or whether a skill is safe. There is no signature scheme in
  the Agent Skills specification to verify against.
- **Byte preservation is not behavioural equivalence.** The three agents read
  different frontmatter, resolve conflicts three different ways, and search to
  different depths. Identical bytes with different behaviour is the normal case.
- **The specification has no version.** Claims are pinned to a 2026-10-07
  retrieval date; `skill` never advertises conformance to a numbered spec.
- **Gemini CLI discovers only one level deep** and **skips workspace skills
  entirely in an untrusted folder**, so a correct project-scope write can still
  never load. `skill` reports it as written, not as loaded.
- **Codex keeps both** same-named skills rather than shadowing, so a collision
  there is a duplicate in the picker.
- **No automatic merge.** A conflict stops. Detection is the requirement; a bad
  merge is worse than a stop.
- **Mounted shares are mounted-path support, not native SMB.** The kernel does
  the protocol work and the mount's credentials apply.
- **No telemetry**, and no network call you did not ask for.

---

## 🧪 Running Tests

```bash
make test
# or
cargo test --locked --all-features
```

The full gate, which is what CI runs:

```bash
make ci
# cargo fmt --all -- --check
# cargo clippy --all-targets --all-features -- -D warnings
# cargo clippy --all-targets --no-default-features -- -D warnings
# cargo test --locked --all-features
# cargo test --locked --no-default-features
# cargo build --release
make msrv     # cargo +1.88 check --locked --all-targets
```

Every test builds a complete fake machine in a temporary directory with its own
store, state, cache, config, and agent home. The suite never reads or writes a
real agent installation, and never rewrites your `HOME`: isolation is injected
through `SKILL_*` variables on a scoped child process.

---

## 🐳 Using Docker

```bash
docker build -t skill .
docker run --rm skill --help
```

**Agent discovery inside a container sees the container**, not your host. To
manage a host installation you must map the paths in deliberately:

```bash
docker run --rm \
  -v "$HOME/.claude:/home/skill/.claude" \
  -v "$HOME/skills:/home/skill/skills" \
  -v "$PWD/my-skill:/src/my-skill:ro" \
  skill copy /src/my-skill claude
```

`skill link` writes symlinks using **container** paths, which will usually be
broken when the host reads them. Prefer `skill copy` through a container.

---

## ⚙️ Using the Makefile

```bash
make build                  # release binary
make run ARGS="--help"
make test
make lint
make ci                     # the full gate
make msrv                   # verify the declared MSRV
make help                   # list every target
```

Each target is a thin wrapper over `cargo`; the equivalent command is shown
above for platforms without `make`.

---

## 🤝 Contributing

Pull requests welcome. For a significant change, open an issue first.

1. Fork and branch from `dev` (`git checkout -b feature/thing`)
2. Add a test that fails before your change
3. Run `make ci`
4. Commit as `{type}: {description}` (`feat`/`fix`/`docs`/`refactor`/`test`/`chore`)
5. Open a pull request against `dev`

Two house rules worth knowing before you start: nothing acquired may ever be
executed, and no code path may overwrite content `skill` did not write. A change
that weakens either needs a very good argument in the pull request.

Contributions are accepted under the [CLA](CLA.md).

---

## 📄 License

This project is **dual-licensed**.

### Open Source License (GPLv3)

`skill` is available under the **GNU General Public License v3.0 only**
(`GPL-3.0-only`). Use under GPLv3 is subject to that licence's terms and
obligations, including its copyleft requirements. See [LICENSE](LICENSE).

### Commercial License

For organizations requiring **proprietary internal use** without GPLv3
obligations, a **Commercial License** is available. It allows internal
organizational use, private modification, and use by employees and contractors.
It does **not** allow redistribution or resale, SaaS or hosted or API offerings,
embedding into third-party products, or use of project branding without
permission. Terms are in [COMMERCIAL-EULA.md](COMMERCIAL-EULA.md).

### Choosing

- Building or distributing open-source software: **use GPLv3**
- Using internally and keeping modifications proprietary: **purchase a Commercial License**

Commercial licensing inquiries: 📧 gilles.infosec@gmail.com

Third-party attribution, including the project that inspired this interface, is
in [ATTRIBUTION.md](ATTRIBUTION.md).

---

**⚡ Built with Rust | 🛡️ Secured by Design | 📋 Status in [docs/checklist.md](docs/checklist.md)**
