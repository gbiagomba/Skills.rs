//! `SKILL.md` frontmatter parsing and validation.
//!
//! # Why validation is layered rather than pass/fail
//!
//! The Agent Skills specification (retrieved 2026-10-07, and carrying no version
//! string of its own) defines six frontmatter fields and says nothing about what a
//! client should do with any other key. The two implementations we checked
//! disagree: Claude Code silently ignores an unknown field, while the claude.ai
//! upload path and `package_skill.py` reject it with a hard error. A single
//! verdict would therefore be wrong for somebody, so findings carry a
//! [`Severity`] and the caller decides.
//!
//! `SKILL.md` is never rewritten. Unknown keys are retained verbatim in
//! [`Frontmatter::extra`] so that an export or a migration reproduces the file
//! byte for byte.

use std::collections::BTreeMap;

use serde_yaml_ng::Value;

/// Hard upper bounds taken from the specification.
pub const MAX_NAME_LEN: usize = 64;
pub const MAX_DESCRIPTION_LEN: usize = 1024;
pub const MAX_COMPATIBILITY_LEN: usize = 500;

/// The six keys the specification defines. Everything else is "unknown".
///
/// The parser matches these arms explicitly rather than consulting this list, so
/// `spec_fields_match_the_parser` guards the two against drifting apart.
pub const SPEC_FIELDS: [&str; 6] = [
    "name",
    "description",
    "license",
    "compatibility",
    "metadata",
    "allowed-tools",
];

/// Frontmatter fields that only Claude Code understands.
///
/// Flagged when a package is deployed to, or migrated toward, another agent. The
/// list is taken from the Claude Code skills documentation rather than guessed.
pub const CLAUDE_ONLY_FIELDS: [&str; 14] = [
    "when_to_use",
    "disable-model-invocation",
    "user-invocable",
    "disallowed-tools",
    "argument-hint",
    "arguments",
    "model",
    "effort",
    "context",
    "agent",
    "background",
    "hooks",
    "paths",
    "shell",
];

/// How seriously to treat a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Documented by the agent as unsupported, or unusable as a package.
    Error,
    /// Valid, but something will behave differently somewhere.
    Warning,
    /// Informational: worth disclosing, nothing is wrong.
    Note,
}

/// One validation or compatibility observation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Finding {
    /// Stable machine-readable identifier, for `--json` consumers and tests.
    pub code: &'static str,
    pub severity: Severity,
    /// Operator-facing explanation.
    pub message: String,
    /// Set when the finding only applies to one agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// True when an agent documents this as unsupported, false when we inferred it.
    ///
    /// The prompt requires confirmed incompatibility to be distinguishable from a
    /// heuristic guess, and operators need that distinction to triage.
    pub confirmed: bool,
}

impl Finding {
    fn error(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Error,
            message: message.into(),
            agent: None,
            confirmed: true,
        }
    }

    fn warn(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Warning,
            message: message.into(),
            agent: None,
            confirmed: true,
        }
    }

    fn note(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Note,
            message: message.into(),
            agent: None,
            confirmed: true,
        }
    }

    /// Mark this finding as applying to a single agent.
    pub fn for_agent(mut self, agent: impl Into<String>, confirmed: bool) -> Self {
        self.agent = Some(agent.into());
        self.confirmed = confirmed;
        self
    }
}

/// The parsed frontmatter of a `SKILL.md`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Frontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub license: Option<String>,
    pub compatibility: Option<String>,
    /// The specification types this as a string-to-string map.
    pub metadata: BTreeMap<String, String>,
    pub allowed_tools: Option<String>,
    /// Every key outside the six spec fields, retained as parsed.
    #[serde(skip)]
    pub extra: BTreeMap<String, Value>,
    /// True when the file had no `---` block at all.
    pub absent: bool,
}

impl Frontmatter {
    /// Keys present in the file but not defined by the specification.
    pub fn unknown_keys(&self) -> Vec<&str> {
        self.extra.keys().map(String::as_str).collect()
    }
}

/// Outcome of splitting a `SKILL.md` into frontmatter and body.
#[derive(Debug, Clone)]
pub struct ParsedSkill {
    pub frontmatter: Frontmatter,
    /// The Markdown body, excluding the frontmatter block.
    pub body: String,
    /// Findings raised while parsing, before field validation.
    pub findings: Vec<Finding>,
}

/// Split the leading `---` delimited YAML block from a `SKILL.md`.
///
/// Mirrors the rule Claude Code documents: the block counts only when the opening
/// `---` is the very first line. Otherwise the whole file, markers included, is
/// body text. A leading UTF-8 byte-order mark is tolerated, because editors on
/// Windows add one and it would otherwise make the first line not match.
fn split_frontmatter(content: &str) -> (Option<&str>, &str) {
    let text = content.strip_prefix('\u{feff}').unwrap_or(content);

    let mut lines = text.char_indices();
    // The first line must be exactly `---` (trailing whitespace tolerated).
    let first_line_end = text.find('\n').unwrap_or(text.len());
    if text[..first_line_end].trim_end() != "---" {
        return (None, text);
    }
    let _ = lines.next();

    let after_open = (first_line_end + 1).min(text.len());
    let rest = &text[after_open..];

    // Find a closing line that is exactly `---` or `...`.
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']).trim_end();
        if trimmed == "---" || trimmed == "..." {
            let yaml = &rest[..offset];
            let body_start = (offset + line.len()).min(rest.len());
            return (Some(yaml), &rest[body_start..]);
        }
        offset += line.len();
    }

    // Unterminated block: treat the file as body, which is what Claude Code does.
    (None, text)
}

/// Coerce a YAML scalar to a string without inventing a value.
///
/// Numbers and booleans are accepted because YAML will happily parse
/// `description: 2026` as an integer, and refusing that would be unhelpful.
fn scalar_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// `allowed-tools` is a space-separated string in the specification, but Claude
/// Code also accepts a YAML list. Both are normalised to one string here, and the
/// original is still available in `extra` for anything that needs it.
fn tools_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Sequence(items) => {
            let parts: Vec<String> = items.iter().filter_map(scalar_to_string).collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(" "))
            }
        }
        _ => None,
    }
}

/// Parse a `SKILL.md`, returning the frontmatter, body, and any parse findings.
///
/// Parsing never fails outright: an unreadable frontmatter block becomes a finding
/// so the caller can report every problem with a package at once rather than one
/// per run.
pub fn parse(content: &str) -> ParsedSkill {
    let (yaml, body) = split_frontmatter(content);
    let mut findings = Vec::new();

    let Some(yaml) = yaml else {
        return ParsedSkill {
            frontmatter: Frontmatter {
                absent: true,
                ..Frontmatter::default()
            },
            body: body.to_string(),
            findings: vec![Finding::error(
                "frontmatter.absent",
                "SKILL.md has no YAML frontmatter block starting on its first line",
            )],
        };
    };

    let parsed: Value = match serde_yaml_ng::from_str(yaml) {
        Ok(v) => v,
        Err(err) => {
            return ParsedSkill {
                frontmatter: Frontmatter {
                    absent: false,
                    ..Frontmatter::default()
                },
                body: body.to_string(),
                findings: vec![Finding::error(
                    "frontmatter.unparseable",
                    format!("frontmatter is not valid YAML: {err}"),
                )],
            };
        }
    };

    let mut fm = Frontmatter::default();

    let Value::Mapping(map) = parsed else {
        // An empty block parses as null; anything else is a structural mistake.
        if !matches!(parsed, Value::Null) {
            findings.push(Finding::error(
                "frontmatter.not_a_mapping",
                "frontmatter must be a YAML mapping of keys to values",
            ));
        } else {
            findings.push(Finding::error(
                "frontmatter.empty",
                "frontmatter block is empty",
            ));
        }
        return ParsedSkill {
            frontmatter: fm,
            body: body.to_string(),
            findings,
        };
    };

    for (key, value) in map {
        let Some(key) = scalar_to_string(&key) else {
            findings.push(Finding::warn(
                "frontmatter.non_string_key",
                "ignoring a frontmatter key that is not a string",
            ));
            continue;
        };

        match key.as_str() {
            "name" => match scalar_to_string(&value) {
                Some(s) => fm.name = Some(s),
                None => findings.push(Finding::error(
                    "frontmatter.name_not_scalar",
                    "`name` must be a string",
                )),
            },
            "description" => match scalar_to_string(&value) {
                Some(s) => fm.description = Some(s),
                None => findings.push(Finding::error(
                    "frontmatter.description_not_scalar",
                    "`description` must be a string",
                )),
            },
            "license" => fm.license = scalar_to_string(&value),
            "compatibility" => fm.compatibility = scalar_to_string(&value),
            "allowed-tools" => fm.allowed_tools = tools_to_string(&value),
            "metadata" => match &value {
                Value::Mapping(entries) => {
                    for (mk, mv) in entries {
                        let Some(mk) = scalar_to_string(mk) else {
                            continue;
                        };
                        match scalar_to_string(mv) {
                            Some(mvs) => {
                                fm.metadata.insert(mk, mvs);
                            }
                            None => findings.push(Finding::warn(
                                "frontmatter.metadata_nested",
                                format!(
                                    "metadata key `{mk}` is not a string value; the specification \
                                     types metadata as a string-to-string map"
                                ),
                            )),
                        }
                    }
                }
                _ => findings.push(Finding::error(
                    "frontmatter.metadata_not_a_map",
                    "`metadata` must be a mapping of string keys to string values",
                )),
            },
            _ => {
                fm.extra.insert(key, value);
            }
        }
    }

    ParsedSkill {
        frontmatter: fm,
        body: body.to_string(),
        findings,
    }
}

/// Validate frontmatter against the specification and the agents we support.
///
/// `dir_name` is the package directory name, which the specification says `name`
/// must match.
///
/// Note on `name`: the specification lists it as required, but Claude Code and
/// Codex both fall back to the directory name when it is absent, so a package
/// without one is in real use today. Rejecting it would make `skill` the only
/// tool that cannot handle those packages, so a missing `name` is a warning plus a
/// confirmed Gemini CLI incompatibility (Gemini requires it and drops the skill).
/// A missing `description` is a hard error, because all three agents either drop
/// the skill or cannot decide when to use it.
pub fn validate(fm: &Frontmatter, dir_name: &str) -> Vec<Finding> {
    let mut findings = Vec::new();

    match &fm.name {
        None => {
            findings.push(Finding::warn(
                "name.missing",
                format!(
                    "no `name` in frontmatter; Claude Code and Codex will fall back to the \
                     directory name {dir_name:?}"
                ),
            ));
            findings.push(
                Finding::error(
                    "name.missing",
                    "Gemini CLI requires `name` and silently skips a skill without one",
                )
                .for_agent("gemini", true),
            );
        }
        Some(name) => findings.extend(validate_name(name, dir_name)),
    }

    match &fm.description {
        None => findings.push(Finding::error(
            "description.missing",
            "`description` is required: it is how an agent decides when to use the skill",
        )),
        Some(d) if d.trim().is_empty() => findings.push(Finding::error(
            "description.empty",
            "`description` must not be empty",
        )),
        Some(d) if d.chars().count() > MAX_DESCRIPTION_LEN => findings.push(Finding::error(
            "description.too_long",
            format!(
                "`description` is {} characters; the specification allows at most {MAX_DESCRIPTION_LEN}",
                d.chars().count()
            ),
        )),
        Some(_) => {}
    }

    if let Some(compat) = &fm.compatibility {
        if compat.chars().count() > MAX_COMPATIBILITY_LEN {
            findings.push(Finding::error(
                "compatibility.too_long",
                format!(
                    "`compatibility` is {} characters; the specification allows at most \
                     {MAX_COMPATIBILITY_LEN}",
                    compat.chars().count()
                ),
            ));
        }
    }

    if fm.allowed_tools.is_some() {
        findings.push(Finding::note(
            "allowed_tools.experimental",
            "`allowed-tools` is marked Experimental in the specification; support varies by agent",
        ));
    }

    // Unknown keys are preserved, but they are a portability hazard in one
    // direction only, so say exactly where.
    let unknown = fm.unknown_keys();
    if !unknown.is_empty() {
        let claude_only: Vec<&str> = unknown
            .iter()
            .copied()
            .filter(|k| CLAUDE_ONLY_FIELDS.contains(k))
            .collect();
        let other: Vec<&str> = unknown
            .iter()
            .copied()
            .filter(|k| !CLAUDE_ONLY_FIELDS.contains(k))
            .collect();

        if !claude_only.is_empty() {
            findings.push(Finding::warn(
                "frontmatter.claude_only_fields",
                format!(
                    "{} is Claude Code specific and is ignored by Codex and Gemini CLI: {}",
                    if claude_only.len() == 1 {
                        "field"
                    } else {
                        "fields"
                    },
                    claude_only.join(", ")
                ),
            ));
        }
        if !other.is_empty() {
            findings.push(Finding::warn(
                "frontmatter.unknown_fields",
                format!(
                    "unknown frontmatter field(s) preserved as-is: {}. Claude Code ignores \
                     these, but claude.ai upload and package_skill.py reject them outright",
                    other.join(", ")
                ),
            ));
        }
    }

    findings
}

/// Check `name` against the specification's naming rules.
///
/// The specification states the rules in prose and publishes no regular
/// expression, so they are encoded here literally: 1 to 64 characters, lowercase
/// ASCII letters, digits, and hyphens, no leading or trailing hyphen, and no
/// consecutive hyphens.
pub fn validate_name(name: &str, dir_name: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    let len = name.chars().count();

    if len == 0 {
        findings.push(Finding::error("name.empty", "`name` must not be empty"));
        return findings;
    }
    if len > MAX_NAME_LEN {
        findings.push(Finding::error(
            "name.too_long",
            format!("`name` is {len} characters; the specification allows at most {MAX_NAME_LEN}"),
        ));
    }
    if name
        .chars()
        .any(|c| !matches!(c, 'a'..='z' | '0'..='9' | '-'))
    {
        findings.push(Finding::error(
            "name.charset",
            format!("`name` {name:?} may only contain lowercase letters, digits, and hyphens"),
        ));
    }
    if name.starts_with('-') || name.ends_with('-') {
        findings.push(Finding::error(
            "name.edge_hyphen",
            "`name` must not start or end with a hyphen",
        ));
    }
    if name.contains("--") {
        findings.push(Finding::error(
            "name.double_hyphen",
            "`name` must not contain consecutive hyphens",
        ));
    }
    if name != dir_name {
        findings.push(Finding::warn(
            "name.dir_mismatch",
            format!(
                "`name` is {name:?} but the package directory is {dir_name:?}; the specification \
                 requires them to match"
            ),
        ));
    }

    // Gemini CLI rewrites these characters rather than refusing, so an install can
    // succeed under a name the operator did not choose. Predict it up front.
    const GEMINI_REWRITTEN: [char; 8] = [':', '\\', '/', '<', '>', '*', '?', '"'];
    if name.chars().any(|c| GEMINI_REWRITTEN.contains(&c)) || name.contains('|') {
        let rewritten: String = name
            .chars()
            .map(|c| {
                if GEMINI_REWRITTEN.contains(&c) || c == '|' {
                    '-'
                } else {
                    c
                }
            })
            .collect();
        findings.push(
            Finding::warn(
                "name.gemini_rewrite",
                format!("Gemini CLI will silently rename this skill to {rewritten:?}"),
            )
            .for_agent("gemini", true),
        );
    }

    findings
}

/// True when any finding is an error.
pub fn has_errors(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.severity == Severity::Error)
}

/// Errors that apply to every agent, excluding agent-scoped ones.
pub fn blocking_errors(findings: &[Finding]) -> Vec<&Finding> {
    findings
        .iter()
        .filter(|f| f.severity == Severity::Error && f.agent.is_none())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str =
        "---\nname: my-skill\ndescription: Does a thing when asked.\n---\n\n# Body\n";

    #[test]
    fn parses_a_well_formed_skill() {
        let parsed = parse(GOOD);
        assert_eq!(parsed.frontmatter.name.as_deref(), Some("my-skill"));
        assert_eq!(
            parsed.frontmatter.description.as_deref(),
            Some("Does a thing when asked.")
        );
        assert!(parsed.body.contains("# Body"));
        assert!(parsed.findings.is_empty(), "{:?}", parsed.findings);
        assert!(validate(&parsed.frontmatter, "my-skill").is_empty());
    }

    #[test]
    fn frontmatter_must_start_on_the_first_line() {
        // Claude Code documents this exact rule, so we follow it.
        let content = "\n---\nname: x\ndescription: y\n---\n";
        let parsed = parse(content);
        assert!(parsed.frontmatter.absent);
        assert_eq!(parsed.findings[0].code, "frontmatter.absent");
    }

    #[test]
    fn tolerates_a_byte_order_mark() {
        let parsed = parse(&format!("\u{feff}{GOOD}"));
        assert_eq!(parsed.frontmatter.name.as_deref(), Some("my-skill"));
    }

    #[test]
    fn preserves_unknown_fields_verbatim() {
        let content =
            "---\nname: my-skill\ndescription: d\nmy-custom: 42\nwhen_to_use: later\n---\n";
        let parsed = parse(content);
        let mut keys = parsed.frontmatter.unknown_keys();
        keys.sort_unstable();
        assert_eq!(keys, vec!["my-custom", "when_to_use"]);

        let findings = validate(&parsed.frontmatter, "my-skill");
        // The Claude-only field and the genuinely unknown one are reported apart,
        // because only one of them is a cross-agent portability problem.
        assert!(findings
            .iter()
            .any(|f| f.code == "frontmatter.claude_only_fields"));
        assert!(findings
            .iter()
            .any(|f| f.code == "frontmatter.unknown_fields"));
        assert!(!has_errors(&findings));
    }

    #[test]
    fn missing_description_is_an_error() {
        let parsed = parse("---\nname: my-skill\n---\n");
        let findings = validate(&parsed.frontmatter, "my-skill");
        assert!(findings
            .iter()
            .any(|f| f.code == "description.missing" && f.severity == Severity::Error));
    }

    #[test]
    fn missing_name_warns_globally_but_blocks_gemini() {
        let parsed = parse("---\ndescription: d\n---\n");
        let findings = validate(&parsed.frontmatter, "dir-name");

        // Not a blocking error overall: Claude Code and Codex fall back to the dir.
        assert!(blocking_errors(&findings).is_empty(), "{findings:?}");
        // But it is a confirmed, agent-scoped error for Gemini.
        let gemini = findings
            .iter()
            .find(|f| f.agent.as_deref() == Some("gemini"))
            .expect("expected a gemini finding");
        assert_eq!(gemini.severity, Severity::Error);
        assert!(gemini.confirmed);
    }

    #[test]
    fn enforces_the_naming_rules() {
        let cases = [
            ("Upper-Case", "name.charset"),
            ("-leading", "name.edge_hyphen"),
            ("trailing-", "name.edge_hyphen"),
            ("double--hyphen", "name.double_hyphen"),
            ("has space", "name.charset"),
        ];
        for (name, expected) in cases {
            let findings = validate_name(name, name);
            assert!(
                findings.iter().any(|f| f.code == expected),
                "{name:?} should raise {expected}, got {findings:?}"
            );
        }

        let long = "a".repeat(MAX_NAME_LEN + 1);
        assert!(validate_name(&long, &long)
            .iter()
            .any(|f| f.code == "name.too_long"));
    }

    #[test]
    fn flags_name_directory_mismatch() {
        let findings = validate_name("other-name", "dir-name");
        assert!(findings
            .iter()
            .any(|f| f.code == "name.dir_mismatch" && f.severity == Severity::Warning));
    }

    #[test]
    fn description_length_cap_is_enforced() {
        let long = "x".repeat(MAX_DESCRIPTION_LEN + 1);
        let fm = Frontmatter {
            name: Some("s".into()),
            description: Some(long),
            ..Frontmatter::default()
        };
        assert!(validate(&fm, "s")
            .iter()
            .any(|f| f.code == "description.too_long"));
    }

    #[test]
    fn metadata_must_be_a_string_map() {
        let parsed = parse("---\nname: s\ndescription: d\nmetadata: not-a-map\n---\n");
        let all: Vec<&Finding> = parsed.findings.iter().collect();
        assert!(all
            .iter()
            .any(|f| f.code == "frontmatter.metadata_not_a_map"));

        let ok = parse(
            "---\nname: s\ndescription: d\nmetadata:\n  author: me\n  version: \"1.0\"\n---\n",
        );
        assert_eq!(
            ok.frontmatter.metadata.get("author").map(String::as_str),
            Some("me")
        );
        // `version` is not a spec field; it is only conventional inside metadata.
        assert_eq!(
            ok.frontmatter.metadata.get("version").map(String::as_str),
            Some("1.0")
        );
    }

    #[test]
    fn allowed_tools_accepts_a_list_or_a_string() {
        let s = parse("---\nname: s\ndescription: d\nallowed-tools: Read Bash\n---\n");
        assert_eq!(s.frontmatter.allowed_tools.as_deref(), Some("Read Bash"));

        let l = parse("---\nname: s\ndescription: d\nallowed-tools:\n  - Read\n  - Bash\n---\n");
        assert_eq!(l.frontmatter.allowed_tools.as_deref(), Some("Read Bash"));

        assert!(validate(&s.frontmatter, "s")
            .iter()
            .any(|f| f.code == "allowed_tools.experimental"));
    }

    #[test]
    fn spec_fields_match_the_parser() {
        // Every spec field must be consumed into a typed slot, never left in
        // `extra`. If a field is added to SPEC_FIELDS without a parser arm, or an
        // arm is removed, this fails.
        for field in SPEC_FIELDS {
            let value = match field {
                "metadata" => "\n  k: v".to_string(),
                _ => " value".to_string(),
            };
            let content = format!("---\nname: s\ndescription: d\n{field}:{value}\n---\n");
            let parsed = parse(&content);
            assert!(
                !parsed.frontmatter.unknown_keys().contains(&field),
                "{field:?} is a spec field but the parser left it in `extra`"
            );
        }
    }

    #[test]
    fn unparseable_yaml_is_reported_not_panicked() {
        let parsed = parse("---\nname: [unclosed\n---\n");
        assert_eq!(parsed.findings[0].code, "frontmatter.unparseable");
    }

    #[test]
    fn handles_an_unterminated_block_as_body() {
        let parsed = parse("---\nname: x\n");
        assert!(parsed.frontmatter.absent);
    }
}
