# Attribution

## vercel-labs/skills

The command-line interface of `skill` was **inspired by**
[`vercel-labs/skills`](https://github.com/vercel-labs/skills), reviewed at
version 1.7.1 (2026-10-06).

That project is licensed MIT, `Copyright (c) 2026 Vercel, Inc.`

**We read its source** while designing this tool, specifically its source
parsing, installer, and lockfile modules. This is therefore **not** a clean-room
implementation, and this project does not claim to be one.

No code, README prose, or other copyrightable material from that project has been
copied into this repository. What was taken is factual and conceptual: which
commands a skill manager needs, the `--skill` and `--agent` selection shape, the
existence of a canonical directory with per-agent links, and the published
filesystem paths of the agents it supports. Factual paths and interface
vocabulary are not themselves copyrightable; its README's particular wording is,
and it has not been reused.

If code or substantial material from that project is ever incorporated, the MIT
copyright notice and permission notice above must travel with it, as the MIT
licence requires, and this file must say exactly what was reused.

`skill` differs in substance rather than only in language. It adds a canonical
store with provenance, a transactional SQLite state store, three-way conflict
detection against separate upstream and per-deployment baselines, journalled
transactions with rollback, and refusal to overwrite content it did not write.
See `docs/compatibility.md` for what the inspiration does and does not do, stated
as dated observation.

## Agent documentation

Path tables, precedence rules, and capability facts in `docs/compatibility.md`
were taken from vendor documentation on 2026-10-07 and are cited there
individually with their URLs. Those are facts about third-party software, quoted
and attributed, not material incorporated into this project.

## Dependencies

Third-party Rust crates are listed in `Cargo.toml` with exact versions in
`Cargo.lock`. Each carries its own licence; run `cargo tree` to enumerate them
and `cargo license` or `cargo deny` to review their terms.

The native SMB backend is designed against
[`smb2`](https://crates.io/crates/smb2) (MIT OR Apache-2.0), chosen over
`pavao` because it is pure Rust and needs no `libsmbclient` system library.
