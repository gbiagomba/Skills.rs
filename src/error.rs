//! Typed errors and their mapping onto the documented exit codes.
//!
//! Every user-facing failure funnels through [`Error`]. The mapping to
//! [`ExitCode`] lives here so that no call site can invent a status, and the
//! `Display` text is written to be actionable: it says what was refused and what
//! the operator can do next.

use std::path::PathBuf;

use crate::exit::ExitCode;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Every failure `skill` can report.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    // ---- usage -----------------------------------------------------------
    /// Arguments parsed but do not describe a runnable operation.
    #[error("{0}")]
    Usage(String),

    /// A selection is required and could not be inferred without a terminal.
    #[error("{what} is required in a non-interactive session.\nhint: {hint}")]
    NeedsSelection { what: String, hint: String },

    // ---- packages --------------------------------------------------------
    /// No `SKILL.md` was found where a package was expected.
    #[error("no SKILL.md found under {}: this is not a skill package", .0.display())]
    NotAPackage(PathBuf),

    /// Frontmatter is missing, unparseable, or violates the specification.
    #[error("invalid skill package{}: {reason}", name.as_ref().map(|n| format!(" '{n}'")).unwrap_or_default())]
    InvalidPackage {
        name: Option<String>,
        reason: String,
    },

    /// A package name cannot be used as a directory name on a supported platform.
    #[error("unsafe skill name {name:?}: {reason}")]
    UnsafeName { name: String, reason: String },

    /// Two distinct origins want the same install name.
    #[error(
        "'{name}' is already installed from a different origin ({existing}).\n\
         hint: choose another install name with --as <name>, or select the existing package explicitly"
    )]
    NameCollision { name: String, existing: String },

    // ---- containment and safety -----------------------------------------
    /// A path escaped, or would escape, its containment root.
    #[error("refusing to write outside {}: {reason}", root.display())]
    Containment { root: PathBuf, reason: String },

    /// An archive entry or source file is a kind we will not materialise.
    #[error("refusing unsupported entry {entry:?}: {reason}")]
    UnsafeEntry { entry: String, reason: String },

    /// A bound (size, count, depth, redirects) was exceeded.
    #[error("{what} exceeded its limit of {limit} ({actual} seen)\nhint: {hint}")]
    LimitExceeded {
        what: String,
        limit: String,
        actual: String,
        hint: String,
    },

    // ---- sources ---------------------------------------------------------
    /// The source string does not name a transport we support.
    ///
    /// The field is `locator`, not `source`: `thiserror` reserves `source` for the
    /// underlying error cause.
    #[error("unsupported source {locator:?}: {reason}")]
    UnsupportedSource { locator: String, reason: String },

    /// A source could not be fetched.
    #[error("could not acquire {locator}: {reason}")]
    SourceUnavailable { locator: String, reason: String },

    /// Credentials were missing or rejected.
    ///
    /// `reason` is always a transport-level summary. Secrets never reach it.
    #[error("authentication failed for {locator}: {reason}\nhint: {hint}")]
    Authentication {
        locator: String,
        reason: String,
        hint: String,
    },

    /// Plain HTTP was requested without the explicit opt-in.
    #[error(
        "refusing plain HTTP for {url}: traffic would be unauthenticated and readable in transit\n\
         hint: re-run with --allow-http if you accept that, or use an https:// URL"
    )]
    InsecureTransport { url: String },

    /// The operation needs the network but offline mode is in force.
    #[error("--offline is set, so {what} cannot be performed\nhint: {hint}")]
    OfflineRequired { what: String, hint: String },

    /// This build was compiled without the backend the source needs.
    #[error("{transport} support is not compiled into this binary\nhint: rebuild with --features {feature}")]
    BackendUnavailable { transport: String, feature: String },

    // ---- destinations ----------------------------------------------------
    /// An agent id or alias is not one we know.
    #[error("unknown agent {name:?}\nhint: known agents are {known}")]
    UnknownAgent { name: String, known: String },

    /// The agent is known but was not found on this machine.
    #[error("agent '{agent}' was not detected on this machine: {evidence}\nhint: {hint}")]
    AgentNotDetected {
        agent: String,
        evidence: String,
        hint: String,
    },

    /// The agent cannot do what the command asked of it.
    #[error("agent '{agent}' cannot {what}: {reason}\nhint: {hint}")]
    Unsupported {
        agent: String,
        what: String,
        reason: String,
        hint: String,
    },

    /// The destination exists but we do not manage it.
    #[error(
        "{} is not managed by skill ({kind})\n\
         hint: {hint}", path.display()
    )]
    Refused {
        path: PathBuf,
        kind: String,
        hint: String,
    },

    // ---- reconciliation --------------------------------------------------
    /// Divergent content needs a human decision.
    #[error("{count} package(s) have conflicting changes; nothing was written\nhint: {hint}")]
    Conflict { count: usize, hint: String },

    // ---- state and transactions -----------------------------------------
    /// The state database is unusable.
    #[error("state store at {}: {reason}", path.display())]
    State { path: PathBuf, reason: String },

    /// The schema is newer than this binary understands.
    #[error(
        "state schema v{found} was written by a newer skill; this binary supports v{supported}\n\
         hint: upgrade skill, or point --store at a different location"
    )]
    SchemaTooNew { found: u32, supported: u32 },

    /// Another process holds the lock.
    #[error("another skill process holds the lock on {}\nhint: wait for it to finish, then retry", path.display())]
    Locked { path: PathBuf },

    /// A previous transaction did not finish.
    #[error(
        "transaction {id} did not finish, so the store may be mid-change\n\
         hint: run `skill doctor` to inspect it, then `skill rollback {id}` to undo it"
    )]
    IncompleteTransaction { id: String },

    /// Some targets changed and some did not.
    #[error(
        "{applied} of {planned} target(s) were changed before the failure: {reason}\nhint: {hint}"
    )]
    PartialFailure {
        applied: usize,
        planned: usize,
        reason: String,
        hint: String,
    },

    /// A target changed between planning and applying.
    #[error(
        "{} changed while skill was planning, so the plan is stale and was abandoned\n\
         hint: re-run the command to plan against the current contents", path.display()
    )]
    ConcurrentModification { path: PathBuf },

    // ---- plumbing --------------------------------------------------------
    /// An I/O failure, annotated with the path we were working on.
    #[error("{context} ({}): {source}", path.display())]
    Io {
        context: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// An I/O failure with no meaningful path.
    #[error("{context}: {source}")]
    IoBare {
        context: String,
        #[source]
        source: std::io::Error,
    },

    /// A documented command that this build does not implement yet.
    ///
    /// Reported as its own failure rather than a silent no-op, so an unfinished
    /// command is never mistaken for a completed one.
    #[error("`skill {command}` is not implemented in this build\nhint: {hint}")]
    NotImplemented { command: String, hint: String },

    /// A bug: an invariant this code is supposed to maintain did not hold.
    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    /// The documented exit code for this failure.
    pub fn exit_code(&self) -> ExitCode {
        match self {
            Self::Usage(_) | Self::NeedsSelection { .. } => ExitCode::Usage,

            Self::NotAPackage(_)
            | Self::InvalidPackage { .. }
            | Self::UnsafeName { .. }
            | Self::NameCollision { .. }
            | Self::Containment { .. }
            | Self::UnsafeEntry { .. }
            | Self::LimitExceeded { .. } => ExitCode::InvalidPackage,

            Self::UnsupportedSource { .. }
            | Self::SourceUnavailable { .. }
            | Self::Authentication { .. }
            | Self::InsecureTransport { .. }
            | Self::BackendUnavailable { .. } => ExitCode::Source,

            Self::OfflineRequired { .. } => ExitCode::OfflineRequired,

            Self::UnknownAgent { .. }
            | Self::AgentNotDetected { .. }
            | Self::Unsupported { .. } => ExitCode::Destination,

            Self::Refused { .. } => ExitCode::Refused,
            Self::Conflict { .. } => ExitCode::Conflict,

            Self::State { .. } | Self::SchemaTooNew { .. } => ExitCode::State,
            Self::Locked { .. } => ExitCode::Locked,

            Self::IncompleteTransaction { .. }
            | Self::PartialFailure { .. }
            | Self::ConcurrentModification { .. } => ExitCode::Partial,

            Self::NotImplemented { .. } => ExitCode::NotImplemented,

            Self::Io { .. } | Self::IoBare { .. } | Self::Internal(_) => ExitCode::Internal,
        }
    }

    /// Build an [`Error::Io`] with the path that failed attached.
    pub fn io(
        context: impl Into<String>,
        path: impl Into<PathBuf>,
        source: std::io::Error,
    ) -> Self {
        Self::Io {
            context: context.into(),
            path: path.into(),
            source,
        }
    }

    /// Build an [`Error::IoBare`] for failures with no single relevant path.
    pub fn io_bare(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::IoBare {
            context: context.into(),
            source,
        }
    }

    /// Convenience constructor for an invalid package with no known name yet.
    pub fn invalid(reason: impl Into<String>) -> Self {
        Self::InvalidPackage {
            name: None,
            reason: reason.into(),
        }
    }
}

/// Attach a path and a human context to an [`std::io::Result`].
pub trait IoContext<T> {
    /// Convert into our error type, recording what we were trying to do and where.
    fn ctx(self, context: impl Into<String>, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn ctx(self, context: impl Into<String>, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|source| Error::io(context, path, source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_maps_to_its_documented_code() {
        let err = Error::Conflict {
            count: 2,
            hint: "run `skill diff`".into(),
        };
        assert_eq!(err.exit_code(), ExitCode::Conflict);
        assert_eq!(err.exit_code().code(), 3);
    }

    #[test]
    fn refusal_is_distinct_from_conflict() {
        // These are different operator decisions, so they must not share a code.
        let refused = Error::Refused {
            path: PathBuf::from("/tmp/x"),
            kind: "unmanaged".into(),
            hint: "adopt it".into(),
        };
        assert_eq!(refused.exit_code(), ExitCode::Refused);
        assert_ne!(refused.exit_code(), ExitCode::Conflict);
    }

    #[test]
    fn insecure_transport_message_names_the_opt_in() {
        let err = Error::InsecureTransport {
            url: "http://example.com/s.tar.gz".into(),
        };
        let text = err.to_string();
        assert!(
            text.contains("--allow-http"),
            "message must name the flag: {text}"
        );
        assert_eq!(err.exit_code(), ExitCode::Source);
    }

    #[test]
    fn offline_has_its_own_code() {
        let err = Error::OfflineRequired {
            what: "checking upstream".into(),
            hint: "drop --offline".into(),
        };
        assert_eq!(err.exit_code().code(), 10);
    }
}
