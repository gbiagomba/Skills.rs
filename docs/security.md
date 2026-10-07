# Security model

## What a hash does and does not prove

A digest proves **integrity relative to a value we recorded**. If `skill` recorded
`abc…` for a package and later computes `abc…` again, the bytes are unchanged
since we saw them.

It proves nothing else. In particular it does **not** establish that the publisher
is who they claim to be, that the content was ever reviewed, or that the skill is
safe to use. There is no signature scheme in the Agent Skills specification, so
there is nothing for `skill` to verify a publisher against, and it does not
pretend otherwise.

Byte preservation is likewise not a behavioural guarantee. See
`docs/compatibility.md`.

## Nothing acquired is ever executed

`skill` never runs anything it fetched: no `scripts/`, no hooks, no install step,
no build command, no post-install action named in frontmatter or a sidecar.
Acquisition and validation read bytes and compute digests.

This is the single most important property, because a skill package is ordinary
untrusted input from a network or a share. A retrieved `SKILL.md` that *instructs*
an agent to run something is still just text to `skill`.

Consequence worth stating plainly: a skill is executed later, by the agent, when
a user invokes it. `skill` installing a package safely says nothing about what
that package will do once an agent reads it. Review what you install.

## Path and extraction defences

Implemented in `src/safepath.rs` and exercised by the tests named beside each item.

| Hazard | Defence |
| --- | --- |
| `../` traversal | `sanitize_relative` rejects any `..` component (`rejects_traversal_and_absolute_paths`) |
| Absolute entries | rejected, including `/x` and `C:\x` |
| Drive letters, NTFS alternate data streams | any `:` in a relative entry is rejected |
| Reserved Windows device names | `NUL`, `nul.txt`, `COM1`, `AUX`, `LPT9`, … rejected **on every platform**, so a package cannot extract on Linux and fail on Windows |
| Trailing dot or space | rejected, because Windows silently strips them and the resulting name is not the one we checked |
| NUL and control bytes | rejected |
| Case collisions | a package holding both `README.md` and `readme.md` is refused rather than silently losing a file on macOS or Windows |
| Escaping symlinks | `validate_internal_link` keeps only links that stay inside the package; anything else is refused with a reason, never silently dropped |
| Ancestor symlinks and reparse points | `verify_containment` resolves real ancestors through the filesystem, so a planted link above the target cannot redirect a write |
| Time-of-check to time-of-use | containment is re-verified **immediately before each mutation**, not only at planning time |
| Device nodes, FIFOs, sockets | `classify` refuses anything that is not a file, directory, or internal symlink |
| setuid, setgid, sticky bits | `check_mode_safe` refuses them. Only the executable bit is reproduced, normalised to 0755 or 0644, so an author's loose umask cannot widen permissions on the installing machine |
| Archive bombs | bounded total bytes, per-file bytes, entry count, and depth |
| Link cycles | the walker does not follow links, and depth is bounded |

Default bounds, all configurable: 32 MiB download, 128 MiB extracted, 32 MiB per
file, 4096 files, depth 32, 5 redirects, 30 second timeout.

## Transport

- **HTTPS verification is always on** and is never disabled.
- **Plain HTTP requires `--allow-http`.** `--yes` does not grant it, and an
  HTTPS to HTTP redirect is refused regardless.
- Credentials are **never forwarded across a redirect**, and never sent to a host
  named inside package content. A supplied host is a fetch target, not permission
  to contact whatever a package mentions.
- Credentials come only from existing Git and SSH configuration and from OS
  mechanisms. `skill` never prompts for, stores, or transmits a password.
- No password or token ever appears on a command line, in a stored locator, in a
  manifest, in an export, in a log, or in an error message.
  `sanitize_locator` strips userinfo, query, and fragment before anything is
  persisted, and it is applied on the way in rather than on the way out.
- No shell is ever invoked with interpolated input.
- Git submodules and LFS are opt-in, never implicit.
- SMB1 and guest or anonymous fallback are never enabled.
- `--offline` makes **no** network call and explains which cached revision is
  missing rather than failing opaquely.
- HTTP validators (`ETag`, `Last-Modified`) and SMB timestamps are treated as
  optimisation hints only. Identity always comes from a computed digest.

## Destinations we refuse to touch

`skill` modifies only what it installed. Everything else is classified and left
alone:

| Classification | Why it is refused |
| --- | --- |
| `org-managed` | deployed by an organisation through managed settings |
| `plugin-managed` | owned by an agent plugin or extension lifecycle |
| `built-in` | ships with the agent |
| `account-synced` | synchronised from a hosted account, not a local install |
| `unmanaged` | a personal skill we did not create; needs explicit adoption |
| `unknown` | origin could not be determined, so it **stays** unknown rather than being assumed personal |

An existing unmanaged destination is never overwritten (exit code 11). A
destination we *do* manage is still not overwritten when it has been edited since
we wrote it (exit code 3).

`skill` never deletes a broad agent, config, or home directory, and never deletes
a shared discovery directory to hide a skill from one agent.

## What `--yes` does not authorize

`--yes` accepts a plan that is already fully determined. It is not a force flag.
It does **not** authorize:

- overwriting a conflict or a drifted destination;
- plain HTTP;
- adopting an unmanaged or non-personal destination;
- any deletion.

Each of those needs its own specific decision.

## State, logs, and privacy

- The state database, transaction journals, and backups are created mode 0600,
  and their directories 0700. They record source locators and file inventories.
- Structured logs are redacted. Full skill content and secrets are not logged by
  default.
- **There is no telemetry.** `skill` makes no network call that the operator did
  not ask for.
- Imported manifests are untrusted. `skill` does not obey an embedded absolute
  destination, does not automatically trust an embedded source URL, does not
  restore credentials, and does not execute anything a manifest references. An
  imported origin starts untrusted for network refresh.
- Exports carry a warning: skill content and source metadata may be private.

## Concurrency and recovery

- A process-wide advisory lock means a second `skill` run fails with exit code 9
  rather than racing.
- Target fingerprints are re-verified immediately before mutation; a target
  changed since planning aborts the apply.
- Each target is replaced atomically by staging on the destination filesystem and
  renaming. Several destinations may live on several filesystems, so there is
  **no global atomic apply**; partial failure is reported as partial (exit 8) and
  never as success.
- A backup exists before any destructive step, and an append-only journal records
  intent before and outcome after each mutation, each entry synced to disk.
- A mutating command refuses to start while a previous run is unfinished.
- `rollback` refuses to discard edits made after the transaction it is undoing.

## Residual limitations, stated honestly

- **Windows path semantics are only partially verifiable here.** The rules are
  implemented and unit tested, and CI builds on Windows, but the adversarial path
  tests have been executed on macOS only in this working session.
- **Native SMB is not implemented in this build.** `smb://` parsing and the
  backend seam exist; acquisition returns exit code 12. Mounted shares work today
  through the filesystem backend, and that is deliberately labelled mounted-path
  support rather than native SMB.
- A fully concurrent attacker with write access to a destination's **parent**
  directory can still win a race in principle. We narrow the window by
  re-verifying containment and fingerprints immediately before mutation, but
  POSIX offers no portable atomic "open this path only if no component is a
  symlink".
- Case-collision detection compares ASCII-folded names. Unicode case folding
  differs between filesystems and is not fully modelled.
