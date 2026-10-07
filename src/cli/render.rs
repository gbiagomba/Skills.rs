//! Output rendering.
//!
//! Two rules govern everything here:
//!
//! * **stdout carries data, stderr carries diagnostics.** A `--json` run writes
//!   exactly one JSON object to stdout and nothing else, so a caller can pipe it
//!   into a parser without stripping progress text first.
//! * **JSON is schema-versioned.** Every payload is wrapped in an envelope naming
//!   the schema and the command, so a consumer can detect a format change rather
//!   than silently mis-parse.

use std::io::Write;

use serde::Serialize;

use crate::exit::ExitCode;

/// The schema identifier every JSON payload carries.
pub const JSON_SCHEMA: &str = "skill.v1";

/// The envelope wrapping all machine-readable output.
#[derive(Debug, Serialize)]
pub struct Envelope<T: Serialize> {
    /// Schema identifier. Changes only on a breaking format change.
    pub schema: &'static str,
    /// The subcommand that produced this payload.
    pub command: String,
    /// True when the operation completed as requested.
    pub ok: bool,
    /// The documented exit-code slug.
    pub status: &'static str,
    /// Set when `--dry-run` was in force, so a consumer cannot mistake a plan
    /// for a performed operation.
    pub dry_run: bool,
    pub data: T,
    /// Disclosures and warnings. Also written to stderr for human readers.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// An error, rendered as JSON.
#[derive(Debug, Serialize)]
pub struct ErrorPayload {
    pub message: String,
    pub status: &'static str,
    pub exit_code: i32,
}

/// Where output goes, and in which format.
#[derive(Debug, Clone, Copy)]
pub struct Output {
    pub json: bool,
    pub verbose: bool,
    pub dry_run: bool,
}

impl Output {
    /// Write the successful result of `command`.
    pub fn emit<T: Serialize>(
        &self,
        command: &str,
        data: T,
        notes: &[String],
    ) -> crate::Result<()> {
        if self.json {
            let envelope = Envelope {
                schema: JSON_SCHEMA,
                command: command.to_string(),
                ok: true,
                status: ExitCode::Success.slug(),
                dry_run: self.dry_run,
                data,
                notes: notes.to_vec(),
            };
            let text = serde_json::to_string_pretty(&envelope).map_err(|err| {
                crate::Error::Internal(format!("could not serialise the result: {err}"))
            })?;
            println!("{text}");
            // Notes also go to stderr so a human watching a piped run sees them.
            for note in notes {
                eprintln!("note: {note}");
            }
        } else {
            for note in notes {
                eprintln!("note: {note}");
            }
        }
        Ok(())
    }

    /// Write an error, in whichever format was requested.
    pub fn emit_error(&self, command: &str, err: &crate::Error) {
        let code = err.exit_code();
        if self.json {
            let envelope = Envelope {
                schema: JSON_SCHEMA,
                command: command.to_string(),
                ok: false,
                status: code.slug(),
                dry_run: self.dry_run,
                data: ErrorPayload {
                    message: err.to_string(),
                    status: code.slug(),
                    exit_code: code.code(),
                },
                notes: Vec::new(),
            };
            // Even a failure writes its single object to stdout, so a caller
            // parsing stdout always finds a parseable envelope.
            if let Ok(text) = serde_json::to_string_pretty(&envelope) {
                println!("{text}");
            }
        }
        eprintln!("error: {err}");
    }

    /// A human-readable line on stdout. Suppressed entirely under `--json`.
    pub fn line(&self, text: impl AsRef<str>) {
        if !self.json {
            println!("{}", text.as_ref());
        }
    }

    /// A detail line, shown only with `--verbose`.
    pub fn detail(&self, text: impl AsRef<str>) {
        if !self.json && self.verbose {
            println!("  {}", text.as_ref());
        }
    }

    /// A warning. Always stderr, so it never pollutes parsed output.
    pub fn warn(&self, text: impl AsRef<str>) {
        eprintln!("warning: {}", text.as_ref());
    }

    /// Flush stdout, surfacing a broken pipe rather than swallowing it.
    pub fn flush(&self) {
        let _ = std::io::stdout().flush();
    }
}

/// Render a simple left-aligned table for human output.
///
/// Hand-rolled rather than taken as a dependency: the whole requirement is
/// "pad columns to the widest cell", and a table crate would be a larger
/// surface than the problem.
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let columns = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();

    for row in rows {
        for (index, cell) in row.iter().enumerate().take(columns) {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }

    let mut out = String::new();
    for (index, header) in headers.iter().enumerate() {
        if index + 1 == columns {
            out.push_str(header);
        } else {
            out.push_str(&format!("{:width$}  ", header, width = widths[index]));
        }
    }
    out.push('\n');

    for (index, width) in widths.iter().enumerate() {
        let rule = "-".repeat(*width);
        if index + 1 == columns {
            out.push_str(&rule);
        } else {
            out.push_str(&rule);
            out.push_str("  ");
        }
    }
    out.push('\n');

    for row in rows {
        for (index, width) in widths.iter().enumerate() {
            let cell = row.get(index).map(String::as_str).unwrap_or("");
            if index + 1 == columns {
                out.push_str(cell);
            } else {
                out.push_str(&format!("{cell:width$}  ", width = *width));
            }
        }
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_envelope_is_schema_versioned_and_states_dry_run() {
        let envelope = Envelope {
            schema: JSON_SCHEMA,
            command: "list".to_string(),
            ok: true,
            status: ExitCode::Success.slug(),
            dry_run: true,
            data: vec!["a"],
            notes: vec!["a note".to_string()],
        };
        let text = serde_json::to_string(&envelope).unwrap();
        assert!(text.contains("\"schema\":\"skill.v1\""));
        assert!(text.contains("\"command\":\"list\""));
        assert!(
            text.contains("\"dry_run\":true"),
            "a consumer must be able to tell a plan from a performed operation"
        );
    }

    #[test]
    fn an_error_payload_carries_the_documented_code() {
        let err = crate::Error::Conflict {
            count: 1,
            hint: "h".into(),
        };
        let payload = ErrorPayload {
            message: err.to_string(),
            status: err.exit_code().slug(),
            exit_code: err.exit_code().code(),
        };
        assert_eq!(payload.status, "conflict");
        assert_eq!(payload.exit_code, 3);
    }

    #[test]
    fn table_pads_to_the_widest_cell() {
        let out = table(
            &["AGENT", "PATH"],
            &[
                vec!["claude".into(), "/a".into()],
                vec!["gemini-cli".into(), "/bb".into()],
            ],
        );
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("AGENT     "), "{:?}", lines[0]);
        assert!(lines[2].starts_with("claude    "), "{:?}", lines[2]);
        assert!(lines[3].starts_with("gemini-cli"), "{:?}", lines[3]);
    }

    #[test]
    fn an_empty_table_renders_nothing() {
        assert!(table(&["A"], &[]).is_empty());
    }
}
