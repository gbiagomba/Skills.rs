# Architecture

## The problem this shape solves

A skill manager that only copies files is easy. What makes it hard is that the
same package exists in up to five places at once, and all of them can change
independently:

1. upstream, wherever the package came from;
2. the canonical store, which the operator may have edited deliberately;
3. each agent's deployed copy, which the operator may also have edited;
4. the agent's own idea of where skills live, which differs per agent;
5. whatever a previous interrupted run left behind.

Every design decision below exists to keep those five distinguishable, because
the moment they blur, a tool starts overwriting work.

## Module map

```
src/main.rs            thin: parse argv, dispatch, map a typed error to an exit code
src/lib.rs             module tree and the public engine API

src/exit.rs            the stable exit-code contract
src/error.rs           typed errors, each mapped to exactly one exit code
src/safepath.rs        containment, entry classification, atomic replacement
src/config.rs          layered configuration and platform directories

src/pkg/frontmatter.rs SKILL.md parsing and layered validation
src/pkg/identity.rs    PackageId derivation and install-name safety
src/pkg/scan.rs        package-boundary discovery
src/pkg/tree.rs        the package inventory and its deterministic digest

src/source/locator.rs  source-string parsing into a transport and selector
src/source/fs.rs       local paths, file://, mounted shares, UNC
src/source/mod.rs      the Backend trait and the hardened copy path

src/agent/mod.rs       the Agent trait, Host, Provenance, Capabilities
src/agent/{claude,codex,gemini}.rs
src/agent/registry.rs  alias resolution, physical-path dedup, shared-root disclosure

src/state/schema.rs    DDL and append-only forward migrations
src/state/models.rs    persisted record types
src/state/mod.rs       the Store API

src/recon/mod.rs       the three-way truth table, plans, and diffs
src/txn/journal.rs     the append-only operation journal
src/txn/mod.rs         lock, apply pipeline, rollback

src/cli/mod.rs         the clap surface
src/cli/render.rs      human and JSON output
src/cli/commands.rs    command implementations
```

Two consolidations against the original plan, both because the concerns turned
out to be one concern rather than two: tree hashing lives with the tree inventory
(a digest over anything less than the whole tree is the bug it exists to
prevent), and the comparison, plan, and diff types live in one `recon` module
because they are meaningless apart.

## Why three baselines

This is the core of the design. `skill` records three snapshots per package:

| Snapshot | What it is | Who asks |
| --- | --- | --- |
| `upstream_pristine` | exactly what upstream last gave us | `update` |
| `canonical` | the store copy, including deliberate local edits | both |
| `deployment_baseline` | what we last wrote to one destination | `sync` |

Two baselines would not do. `update` needs to know whether a change in the store
came from upstream or from the operator, which needs `upstream_pristine`. `sync`
needs to know whether a change at a destination came from us or from the
operator, which needs that destination's own baseline. A single "last known
state" collapses those two questions and makes the answer "overwrite".

Both reduce to one function, `recon::compare(baseline, source, target)`, because
both are the same shape: a common ancestor and two descendants. That function is
pure over digests, so the whole truth table is tested without a filesystem.

| Compared with the baseline | Result | Default action |
| --- | --- | --- |
| neither changed | `Unchanged` | nothing |
| source changed, target did not | `SourceAhead` | write |
| target changed, source did not | `TargetDrifted` | preserve, report |
| both changed, differently | `Conflict` | write nothing |
| both changed, to identical content | `ConvergedIdentically` | refresh the baseline only |
| target absent | `TargetMissing` | report; create only on explicit repair |
| no baseline recorded | `NoBaseline` | report; adoption is explicit |

Provenance is consulted **before** drift: a managed, built-in, plugin, or
account-synced destination is never written however its content compares.

## Digests, and why not timestamps

A tree digest is SHA-256 over a canonical manifest, one line per entry sorted by
path:

```
<kind>\t<exec>\t<path>\t<content-digest>\n
```

Three decisions in that line:

- **The whole tree, not `SKILL.md`.** Otherwise an edit to `scripts/run.sh` or a
  deleted `references/` file is invisible, and those are exactly the changes
  reconciliation exists to catch.
- **The executable bit, not the full mode.** Only "is it executable" is
  reproduced on deployment, so including the whole mode would make two
  byte-identical packages differ because their authors had different umasks.
- **No modification time.** An mtime is not evidence of content. Treating it as
  such is precisely how tools overwrite edits, and a test asserts that rewriting
  identical bytes does not move the digest.

## Transactions

One pipeline, reused by every mutating command:

```
plan -> acquire -> validate -> compare -> stage -> backup -> apply -> verify -> commit
```

- A **process-wide advisory lock** (`fs4`) means a second run fails with exit
  code 9 rather than racing. Non-blocking on purpose: waiting silently would make
  a stuck run look like a slow one.
- **Fingerprints are re-verified immediately before each mutation.** If a target
  changed since planning, the apply aborts rather than overwriting work that
  appeared in between.
- **Each target is replaced atomically**: stage on the destination's own
  filesystem, move any existing tree aside into the transaction's backup, rename
  the staged tree in. The move-aside happens on every platform because Windows
  cannot rename over an existing directory.
- **There is no global atomic apply.** Several destinations may be on several
  filesystems. What is guaranteed is per-target atomicity, a backup before any
  destructive step, and that partial failure is reported as partial (exit 8),
  never as success.
- An **append-only JSONL journal** records intent before and outcome after each
  mutation, synced to disk each time. A `BeforeMutate` with no matching
  `AfterMutate` is the crash signature, and it carries the backup path, so
  recovery knows exactly which target may be half-written.
- Every mutating command **refuses to start** while a previous run is unfinished.

### The two digests on a backup row

A backup records both the content it saved (`digest_before`) and what the
transaction then wrote (`digest_after`). `rollback` compares the destination
against `digest_after`: equal means nobody has touched it since, so restoring is
safe; different means there are later edits, and restoring would destroy them, so
it refuses.

Comparing against `digest_before` instead would be wrong, and it was a real bug
caught by a test: after any successful write the destination never matches the
saved content, so every legitimate rollback would have been refused.

## Agent adapters

Nothing about agent behaviour is global, because the agents genuinely disagree:

| Agent | Same-name resolution |
| --- | --- |
| Claude Code | enterprise > personal > project |
| Gemini CLI | highest tier wins, warning emitted |
| Copilot CLI | first found wins, in its own documented location order |
| Codex | neither wins; both appear |

And the Agent Skills client guidance recommends project > user, the opposite of
Claude Code. So each adapter owns its roots, precedence, capabilities, and
classification, and `Capabilities` carries machine-readable facts the engine acts
on, including `isolated_user_root`, `max_discovery_depth`, and a list of
confirmed caveats.

`Detection` carries **evidence**, not a boolean. A leftover skills directory is
reported as "directory present, executable absent", never as proof of
installation.

`Host` is the seam that makes any of this testable. It resolves the home
directory through the platform API rather than `$HOME`, and
`Host::detect_with_home` plus the namespaced `SKILL_AGENT_HOME` variable let the
acceptance tests build a complete fake machine in a temporary directory without
touching a real installation.

## The shared-root constraint, and why sharing is computed

`~/.agents/skills` is read by three of the four agents, and Copilot CLI also
reads a project's `.claude/skills`. Codex has no non-deprecated agent-specific
alternative at all. So:

Each adapter originally declared a hardcoded list of peer agents that share a
root. Adding Copilot falsified two of those lists simultaneously: Codex's claimed
only Gemini, and Claude Code's project root claimed no peers. A stale list
**understates** exposure, which is the one direction that matters for a
disclosure. `registry::readers_of` therefore computes the set by comparing every
adapter's roots by physical path, and `SkillRoot` carries only `agent_specific`,
a fact an adapter genuinely knows about itself. Adding a fifth agent now requires
no change to the existing four, and a test asserts the Claude-project-to-Copilot
case that no adapter declares.

- `skill` prefers the agent-specific root where one exists;
- a write into a shared root discloses which other agents can see it, and says
  isolation is not available rather than implying it is;
- destinations are keyed by **physical** path, so two agents resolving to one
  directory are written once;
- the deprecated `~/.codex/skills` is read for discovery and never written,
  because buying isolation there would mean depending on a path the vendor has
  already deprecated.

## Why SQLite

The state store needs a real transaction boundary so a crash cannot leave half a
record, a durable schema version to migrate from, cheap indexed lookups by
install name and by physical path, and a single file to back up. SQLite has all
four, the `bundled` feature means no system library and clean cross-compilation,
and schema uniqueness constraints let the database itself enforce two invariants:
one package per case-folded install name, and one deployment row per physical
path.

A JSON lockfile was the alternative. It was rejected because a crash mid-write
corrupts the whole file, and "one deployment per physical path" would become
application logic that a concurrent writer could violate.

State lives in the platform data directory, never inside the store. A database
file inside a skill directory would be hashed as package content and reported as
drift on the next `sync`.

## Output

Human output goes to stdout; diagnostics, warnings, and disclosures go to stderr.
With `--json`, stdout carries exactly one schema-versioned envelope and nothing
else, so a caller can pipe it straight into a parser. The envelope names the
schema, the command, the status slug, and whether the run was a dry run, so a
plan can never be mistaken for a performed operation.

## Deliberate non-goals

- **No automatic textual merge.** V1 stops on a conflict. Reliable detection is
  the requirement; a bad merge is worse than a stop.
- **No plugin machinery.** Three adapters are static zero-sized types in a `Vec`
  of trait objects. Dynamic loading would be a larger attack surface than the
  problem.
- **No speculative extension points.** The `Backend` trait exists because there
  are genuinely several transports; nothing else is abstracted for a second
  implementation that does not exist.
