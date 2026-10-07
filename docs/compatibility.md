# Agent and specification compatibility

Every claim here was verified against a primary source on the date given. Where a
source documents nothing, this file says "not documented" rather than guessing,
because a guess that looks like a fact is worse than an acknowledged gap.

**Research date: 2026-10-07.** Re-verify before relying on any of it: all five
sources are actively changing.

## Sources

| Source | URL | Note |
| --- | --- | --- |
| Agent Skills specification | <https://agentskills.io/specification> | No version string exists anywhere on the page |
| Claude Code skills | <https://code.claude.com/docs/en/skills> | |
| Codex skills | <https://developers.openai.com/codex/skills/> | **308 redirect** to <https://learn.chatgpt.com/docs/build-skills> |
| Copilot CLI skills | <https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference> | section "Skill locations"; the how-to pages omit the full table |
| Gemini CLI skills | <https://geminicli.com/docs/cli/skills/> | Confirmed official; matches `google-gemini/gemini-cli` `docs/cli/skills.md` on `main` |
| vercel-labs/skills | <https://github.com/vercel-labs/skills> | v1.7.1, 2026-10-06. MIT, Copyright (c) 2026 Vercel, Inc. |

## The specification has no version

The Agent Skills page carries no version string, revision, changelog, or
"last updated" date. `skill` therefore pins its claims to a retrieval date and
**never advertises conformance to a numbered specification**. The only
machine-checkable artifact the page offers is the `skills-ref validate` tool.

A `version` key is *not* a specification field. It appears only as a conventional
key inside `metadata`.

### The six defined fields

| Field | Required | Constraint |
| --- | --- | --- |
| `name` | yes | 1 to 64 chars, lowercase `a-z0-9-`, no leading or trailing hyphen, no `--`, must match the parent directory name |
| `description` | yes | 1 to 1024 chars |
| `license` | no | a license name or a bundled filename |
| `compatibility` | no | max 500 chars |
| `metadata` | no | string-to-string map |
| `allowed-tools` | no | space-separated string, marked **Experimental** |

No naming regular expression is published; the rules are prose only.
`src/pkg/frontmatter.rs` encodes them literally and says so.

### Unknown fields are undefined by the specification

The page states no rule for an unrecognised top-level key, and the two
implementations we checked disagree:

- **Claude Code** silently ignores an unknown field.
- **claude.ai upload, the Skills API, and `package_skill.py`** reject it with a
  hard error naming the allowed six.

`skill` preserves unknown keys verbatim and raises a warning that names the
asymmetry, because neither "fine" nor "fatal" is true everywhere.

### Deviation we make deliberately

The specification lists `name` as required. Claude Code and Codex both fall back
to the directory name when it is absent, so packages without one are in real use.
Rejecting them would make `skill` the only tool that cannot handle those
packages. So:

- missing `description` is a hard **error** (all three agents either drop the
  skill or cannot decide when to use it);
- missing `name` is a **warning** plus a confirmed, agent-scoped **error for
  Gemini CLI**, which requires it and silently skips the skill.

## The shared-root problem

This is the single most consequential compatibility fact, and it constrains what
`skill` can honestly promise.

`~/.agents/skills` is read by **three** of the four supported agents: it is
Codex's documented user root, one of Gemini CLI's, and (since Copilot CLI 1.0.11)
one of Copilot's. Project `.agents/skills` is shared the same way, and Copilot
additionally reads project `.claude/skills`.

| Agent | Agent-specific user root | Also reads | Isolation available? |
| --- | --- | --- | --- |
| Claude Code | `~/.claude/skills/` | none at user scope | yes at user scope; **no at project scope**, because Copilot reads `.claude/skills` |
| Codex | only the deprecated `~/.codex/skills` | `$HOME/.agents/skills` | **no** |
| GitHub Copilot CLI | `~/.copilot/skills/` | `~/.agents/skills/`, project `.github`/`.agents`/`.claude` | yes at user scope |
| Gemini CLI | `~/.gemini/skills/` | `~/.agents/skills/` | yes, by preferring the specific root |

**This is why sharing is computed rather than declared.** Each adapter used to
carry a hardcoded list of peer agents. Adding Copilot made two of those lists
wrong at once: Codex's said only `gemini`, and Claude Code's project root claimed
no peers at all. A stale list understates exposure, which is the one direction
that matters, so `registry::readers_of` now derives the real set by comparing
every adapter's roots by physical path. Adding a fifth agent requires no change
to the existing four.

Consequences, all implemented:

- `skill` writes the agent-specific root wherever one exists.
- A write into a shared root **discloses** which other agents can see it, and
  states that isolation is not available rather than implying it is.
- `skill` will **not** write the deprecated `~/.codex/skills` to buy isolation.
  Depending on a path the vendor has already marked deprecated would break
  quietly later.
- Two selected agents resolving to one physical directory are **written once**,
  and a `migrate` that would delete a shared directory reports a shared-target
  no-op instead.

## Per-agent detail

### Claude Code

| Aspect | Value |
| --- | --- |
| Executable | `claude` |
| User skills | `~/.claude/skills/<name>/SKILL.md` |
| Project skills | `.claude/skills/<name>/SKILL.md`, searched from the start directory up to the repository root |
| Nested | `<subdir>/.claude/skills/`, loaded lazily on first file access below `<subdir>` |
| Enterprise | `.claude/skills/` in the managed settings directory, for example `/etc/claude-code/.claude/skills/` on Linux |
| **Precedence** | **enterprise > personal > project.** Note the direction: a personal skill shadows a project one |
| Path override | **none.** `--add-dir` adds a location for one session; `permissions.additionalDirectories` explicitly does not load skills |
| Symlinks | **documented and supported**, and a skill loads once even when several locations point at the same target |
| Same-name | shadows |
| Reserved names | `synced` (any case), `anthropic-skills`, anything prefixed `anthropic-skills:` |

Not-personal classifications `skill` honours: enterprise/managed settings
(org-managed), `~/.claude/skills/synced/` (account-synced),
`~/.claude/skills/.trash/` (ignored), plugin `skills/` subdirectories and any
folder carrying `.claude-plugin/plugin.json` (plugin-managed), bundled (built-in).

Claude-only frontmatter, flagged on migration elsewhere: `when_to_use`,
`disable-model-invocation`, `user-invocable`, `disallowed-tools`,
`argument-hint`, `arguments`, `model`, `effort`, `context`, `agent`,
`background`, `hooks`, `paths`, `shell`. Claude-only body features:
`${CLAUDE_PLUGIN_ROOT}`, `${CLAUDE_PLUGIN_DATA}`, and `` !`cmd` `` injection.

### Codex

| Aspect | Value |
| --- | --- |
| Executable | `codex` (`codex --version`); config dir `~/.codex` or `$CODEX_HOME` |
| User skills (`USER`) | **`$HOME/.agents/skills`**, the shared convention directory |
| Project skills (`REPO`) | `.agents/skills` at the working directory, every ancestor, and the repository root |
| Admin (`ADMIN`) | `/etc/codex/skills`; `C:\ProgramData\codex\skills` on Windows |
| Bundled (`SYSTEM`) | `$CODEX_HOME/skills/.system`, a managed cache Codex wipes and rewrites |
| Deprecated | `~/.codex/skills`: read for discovery, **never written** |
| **Same-name** | **keeps both.** "Codex doesn't merge them; both can appear in skill selectors" |
| Symlinks | documented: Codex "follows the symlink target when scanning" |
| Config | `~/.codex/config.toml`, `[[skills.config]]` with `path`/`name` and `enabled`. No key relocates a skills directory |
| Env | `CODEX_HOME` relocates the deprecated and `.system` roots only |
| Frontmatter read | `name` (max 64, directory fallback), `description` (required, no cap enforced), `metadata.short-description`. Ignores `license`, `compatibility`, `allowed-tools` |
| Sidecar | `agents/openai.yaml` (`interface.*`, `policy.*`, `dependencies.tools[]`). Legitimate package content: preserved byte-for-byte, flagged as Codex-specific elsewhere |
| Verify loaded | `/skills` interactively. **There is no `codex skills` subcommand.** `codex doctor` reports load failures but no inventory |

Not-personal: `SYSTEM` bundled, `ADMIN`, plugin-managed, cloud/account-synced
(`skill://` identifiers with no local path), and ChatGPT workspace skills.

### Gemini CLI

| Aspect | Value |
| --- | --- |
| Executable | `gemini` (`gemini --version`); config dir `~/.gemini` |
| Discovery tiers, low to high | built-in, extension, `~/.gemini/skills/`, `~/.agents/skills/`, `<project>/.gemini/skills/`, `<project>/.agents/skills/` |
| Same-name | higher tier wins, with a conflict warning. Within a tier, `.agents/skills/` **outranks** `.gemini/skills/` |
| **Discovery depth** | **one level only.** The glob is `SKILL.md` and `*/SKILL.md`, so anything deeper is never found |
| **Folder trust** | **workspace skills are skipped entirely in an untrusted folder**, and trust defaults to untrusted |
| Env | `GEMINI_CLI_HOME` relocates both user roots |
| Settings | `~/.gemini/settings.json`: `skills.enabled` (default true), `skills.disabled[]`; `admin.skills.enabled` is an org override in the system settings file |
| Frontmatter read | `name` and `description` only, both required. **Rewrites `: \ / < > * ? " |` in `name` to `-`** |
| Symlinks | first class: `gemini skills link <path>`, using a junction on Windows |
| Verify loaded | `/skills list [all]` or `gemini skills list --all` |
| Account sync | **none** |

The folder-trust rule is why a successful `--scope project` write to Gemini is
reported as written-but-possibly-inactive, never as loaded. Gemini also ships its
own `gemini skills install|uninstall|link`, so skills it installed appear to
`skill` as unmanaged and need explicit adoption.

### GitHub Copilot CLI

The standalone `copilot` CLI (npm `@github/copilot`). GitHub states explicitly
that this implements the Agent Skills open standard. Skills shipped around CLI
0.0.371 (2025-12-18); project `.agents/skills` landed in 0.0.401 (2026-02-03) and
personal `~/.agents/skills` in 1.0.11 (2026-03-23), the latter explicitly for
cross-tool interoperability.

**Not the same product as `gh copilot`.** That extension was deprecated on
2025-10-25, only ever offered `suggest`/`explain`, and has no skills support.
`skill` therefore does **not** accept `gh-copilot` as an alias, since doing so
would imply it manages something it does not. (The separate newer `gh skill`
command is skills-aware, but that is gh CLI core, not the deprecated extension.)

Documented skill locations, in Copilot's own priority order, first found winning
for a duplicate name:

| Location | Scope | `skill` behaviour |
| --- | --- | --- |
| `.github/skills/` | project | **write target** for project scope |
| `.agents/skills/` | project | discovered, and written on request; shared, so disclosed |
| `.claude/skills/` | project | **discovered, never written.** A skill placed there would appear to be Claude Code's |
| parent `.github/skills/` | inherited | monorepo support; not written |
| `~/.copilot/skills/` | personal | **write target** for user scope, and agent-specific |
| `~/.agents/skills/` | personal | discovered; shared with Codex and Gemini, so disclosed |
| plugin directories | plugin | classified `plugin-managed`, never touched |
| `COPILOT_SKILLS_DIRS` | custom | not written |
| bundled | built-in | lowest priority, never touched |
| org/enterprise relay | remote | **no local file at all**; fetched on demand, so not manageable locally |

| Aspect | Value |
| --- | --- |
| Executable | `copilot` (`copilot version`); config dir `~/.copilot` or `COPILOT_HOME` |
| Same-name | shadows, first found wins. Two plugins may both provide one name and coexist under plugin-qualified invocation |
| Frontmatter read | `name` (max 64, may contain colons for namespacing), `description` (max 1024), `argument-hint`, `allowed-tools`, `user-invocable`, `disable-model-invocation`, `license`. Unknown fields load with a warning rather than being skipped |
| Symlinks | **undocumented.** Its changelog says they load from 1.0.62, but GitHub documents nothing and has an open issue (#3264) about Windows behaviour, so `skill` records `SymlinkSupport::Undocumented` and does not promise `link` here |
| Relocation | `COPILOT_HOME` / `--config-dir` move the config root. **Caveat:** per CLI 1.0.66, setting either stops Copilot reading `~/.agents/skills`, so a skill installed there becomes invisible to those sessions |
| Restriction settings | `skillDirectories`, `ignoredSkillsLocations`, `disabledSkills` in `~/.copilot/settings.json`; a repository's `disabledSkills` merges as a union the user cannot undo |
| Verify loaded | `copilot skill list --json` (rows carry `name`, `description`, `source`, `path`, `enabled`), or `/skills info NAME` in session |

Two further facts `skill` acts on:

- **`gh skill` writes provenance metadata into a skill's `SKILL.md` frontmatter**
  (source repository, ref, and tree SHA; the exact key names are not documented)
  and can pin a skill against updates. `skill` preserves unknown frontmatter keys
  verbatim, so that metadata survives a copy, but re-homing such a skill will
  break `gh skill update`. This is recorded as a caveat rather than handled.
- **Organization and enterprise skills have no local file.** They are projected
  through GitHub's relay and fetched on demand, so they are outside the reach of
  any local manager.

Copilot also supports skills in VS Code and JetBrains agent mode, the cloud
coding agent, and code review. Those surfaces are **out of scope**: this adapter
manages the local CLI installation only. Worth knowing if you author for both:
VS Code additionally reads `~/.claude/skills/`, which the CLI does not, and VS
Code enforces stricter `name` rules than the CLI (lowercase only, no colons, must
match the directory name), so a CLI-style namespaced `name: my-plugin:search`
silently fails to load in VS Code.

## Relationship to vercel-labs/skills

`skill` is an independent Rust implementation. Its interface was **inspired by**
that project, whose source we read. See `ATTRIBUTION.md`. We do **not** claim a
clean-room implementation.

Observed behaviour at v1.7.1, stated as dated observation rather than criticism,
because it is what motivates this tool's design:

- No `migrate`, `export`, `import`, `status`, `doctor`, or pinning command.
- **No local drift detection.** Hash comparison is one-directional ("is upstream
  newer?"), and the install path calls `cleanAndCreateDirectory()`, an `rm -rf`
  then recreate, so a locally edited skill is silently overwritten.
- No namespacing: same-named skills from different repositories collide in one
  flat `.agents/skills/<name>` directory.

Its documented bounds, which calibrated ours: 10 MiB download, 25 MiB extracted,
1000 archive files. `skill` uses 32 MiB, 128 MiB, and 4096 files.

## Deliberately out of scope

Named so an absence is not mistaken for a bug:
`.well-known/agent-skills/index.json` discovery, the skills.sh search API, npm
and `node_modules` sources, Notion, hosted-account synchronisation, and
telemetry of any kind.

## What byte preservation does not buy

`skill` reproduces package bytes, layout, and the executable bit. That is **not**
a guarantee that a skill behaves identically under another agent. The three
agents read different frontmatter fields, resolve conflicts three different ways,
search to different depths, and support different body features. Identical bytes
with different interpretation is the normal case, not the exception.
