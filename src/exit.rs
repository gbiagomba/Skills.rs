//! Stable, documented process exit codes.
//!
//! These numbers are part of the public contract of the `skill` binary. They are
//! documented in the README and in `--help`, and scripts are expected to branch on
//! them, so a value must never be reused for a different meaning.

/// Exit status returned by the `skill` binary.
///
/// `Usage` is produced by `clap` itself, which is why it keeps the conventional
/// value `2` rather than following the rest of the sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum ExitCode {
    /// The requested operation completed and was verified.
    Success = 0,
    /// A bug or an unclassified failure. Always worth reporting.
    Internal = 1,
    /// The command line could not be parsed, or the arguments are contradictory.
    Usage = 2,
    /// Divergent content needs an explicit decision before anything is written.
    Conflict = 3,
    /// A package is missing `SKILL.md`, fails validation, or is unsafe to extract.
    InvalidPackage = 4,
    /// A source could not be reached, authenticated to, or trusted.
    Source = 5,
    /// A destination agent is unknown, undetected, or cannot do what was asked.
    Destination = 6,
    /// The state database is corrupt or was written by an incompatible schema.
    State = 7,
    /// Some targets were changed and some were not; recovery is incomplete.
    Partial = 8,
    /// Another `skill` process holds the lock.
    Locked = 9,
    /// The operation needs the network but `--offline` was requested.
    OfflineRequired = 10,
    /// Refused: the destination is unmanaged or not a personal installation.
    Refused = 11,
    /// The command is specified and documented but not implemented in this build.
    ///
    /// A distinct code so a caller can tell "this build cannot do that yet" from
    /// both success and a genuine failure. See `docs/checklist.md`.
    NotImplemented = 12,
}

impl ExitCode {
    /// The numeric status handed to the operating system.
    pub const fn code(self) -> i32 {
        self as i32
    }

    /// A short, stable, machine-readable slug for `--json` consumers.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Internal => "internal",
            Self::Usage => "usage",
            Self::Conflict => "conflict",
            Self::InvalidPackage => "invalid_package",
            Self::Source => "source",
            Self::Destination => "destination",
            Self::State => "state",
            Self::Partial => "partial",
            Self::Locked => "locked",
            Self::OfflineRequired => "offline_required",
            Self::Refused => "refused",
            Self::NotImplemented => "not_implemented",
        }
    }

    /// One-line description, used to build the `--help` epilogue so that the
    /// documentation and the implementation cannot drift apart.
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Internal => "unexpected internal error",
            Self::Usage => "usage error",
            Self::Conflict => "conflict requiring an explicit decision",
            Self::InvalidPackage => "invalid or unsafe package",
            Self::Source => "source, network, or authentication failure",
            Self::Destination => "destination agent unknown, undetected, or unsupported",
            Self::State => "state corrupt or schema version unsupported",
            Self::Partial => "partial failure, recovery incomplete",
            Self::Locked => "lock held by a concurrent invocation",
            Self::OfflineRequired => "operation requires network but --offline is set",
            Self::Refused => "refused: destination needs explicit adoption",
            Self::NotImplemented => "not implemented in this build",
        }
    }

    /// Every code, in numeric order. Used to render the `--help` epilogue and
    /// asserted by tests so a new variant cannot be forgotten.
    pub const ALL: [Self; 13] = [
        Self::Success,
        Self::Internal,
        Self::Usage,
        Self::Conflict,
        Self::InvalidPackage,
        Self::Source,
        Self::Destination,
        Self::State,
        Self::Partial,
        Self::Locked,
        Self::OfflineRequired,
        Self::Refused,
        Self::NotImplemented,
    ];
}

#[cfg(test)]
mod tests {
    use super::ExitCode;

    #[test]
    fn codes_are_stable_and_unique() {
        // Guards the published contract. Changing any number here is a breaking change.
        let expected = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let actual: Vec<i32> = ExitCode::ALL.iter().map(|c| c.code()).collect();
        assert_eq!(actual, expected);

        let mut slugs: Vec<&str> = ExitCode::ALL.iter().map(|c| c.slug()).collect();
        slugs.sort_unstable();
        let before = slugs.len();
        slugs.dedup();
        assert_eq!(before, slugs.len(), "exit code slugs must be unique");
    }
}
