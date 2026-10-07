//! Records persisted in the state store.

use std::path::PathBuf;

use crate::agent::Scope;
use crate::pkg::identity::{InstallName, PackageId, SourceType};
use crate::pkg::tree::PackageTree;

/// Whether `update` may advance this package's revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinPolicy {
    /// The requested ref is a branch or tag, so `update` follows it.
    Tracking,
    /// The requested ref resolved to an immutable commit, so `update` never
    /// advances it. Moving off a pin is an explicit operator decision.
    Pinned,
}

impl PinPolicy {
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Tracking => "tracking",
            Self::Pinned => "pinned",
        }
    }

    pub fn from_slug(raw: &str) -> Self {
        match raw {
            "pinned" => Self::Pinned,
            _ => Self::Tracking,
        }
    }
}

/// Which of the three baselines a snapshot represents.
///
/// Keeping these apart is what makes conflict detection possible: `update`
/// compares new upstream content against [`Self::UpstreamPristine`] and
/// [`Self::Canonical`], while `sync` compares [`Self::Canonical`] against each
/// destination's [`Self::DeploymentBaseline`]. One baseline could not answer both
/// questions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotKind {
    /// Exactly what upstream last gave us, before any local edit.
    UpstreamPristine,
    /// The current store content, including deliberate local edits.
    Canonical,
    /// What we last wrote to one destination.
    DeploymentBaseline,
}

impl SnapshotKind {
    pub const fn slug(self) -> &'static str {
        match self {
            Self::UpstreamPristine => "upstream_pristine",
            Self::Canonical => "canonical",
            Self::DeploymentBaseline => "deployment_baseline",
        }
    }

    pub fn from_slug(raw: &str) -> Option<Self> {
        match raw {
            "upstream_pristine" => Some(Self::UpstreamPristine),
            "canonical" => Some(Self::Canonical),
            "deployment_baseline" => Some(Self::DeploymentBaseline),
            _ => None,
        }
    }
}

/// How a deployment is materialised at its destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployMode {
    /// An independent copy of the canonical package.
    Copy,
    /// A per-skill symlink to the canonical package.
    ///
    /// An edit made through a link edits canonical content directly, which is why
    /// `sync` treats a link differently from a copy.
    Link,
}

impl DeployMode {
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Link => "link",
        }
    }

    pub fn from_slug(raw: &str) -> Option<Self> {
        match raw {
            "copy" => Some(Self::Copy),
            "link" => Some(Self::Link),
            _ => None,
        }
    }
}

impl std::fmt::Display for DeployMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.slug())
    }
}

/// A package tracked in the store.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PackageRecord {
    pub id: PackageId,
    /// Directory name in the store and at each destination.
    pub install_name: InstallName,
    /// The `name` the frontmatter declared, when it declared one.
    pub display_name: Option<String>,
    pub source_type: SourceType,
    /// Credential-free source locator. Never holds a password or token.
    pub locator: String,
    /// Path of the package inside its source, `""` when the source is the package.
    pub selector: String,
    /// The ref the operator asked for, such as a branch name.
    pub requested_ref: Option<String>,
    /// The immutable revision that ref resolved to.
    pub resolved_revision: Option<String>,
    pub pin_policy: PinPolicy,
    /// Whether an imported origin may be contacted for a future refresh.
    ///
    /// An imported bundle's source URL is untrusted, so this starts false and
    /// only an explicit operator decision sets it.
    pub trusted_origin: bool,
    pub acquired_at: String,
    pub last_checked_at: Option<String>,
}

impl PackageRecord {
    /// True when a refresh would need the network.
    pub fn refresh_needs_network(&self) -> bool {
        self.source_type.needs_network()
    }
}

/// A recorded snapshot and its file list.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotRecord {
    pub id: i64,
    pub package_id: PackageId,
    pub kind: SnapshotKind,
    pub digest: String,
    pub recorded_at: String,
    pub tree: PackageTree,
}

/// One destination we deployed to.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DeploymentRecord {
    pub id: i64,
    pub package_id: PackageId,
    pub agent: String,
    pub scope: Scope,
    /// The resolved deployment path as written.
    pub path: PathBuf,
    pub mode: DeployMode,
    /// The snapshot describing what we last wrote here.
    pub baseline_snapshot_id: Option<i64>,
    pub deployed_at: String,
    pub updated_at: String,
}

/// Outcome of a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TxnStatus {
    /// Started and not yet finished. Found on disk, this means a crash.
    Open,
    Committed,
    /// Rolled back cleanly; nothing was left changed.
    RolledBack,
    /// Some targets changed and recovery did not complete.
    Failed,
}

impl TxnStatus {
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Committed => "committed",
            Self::RolledBack => "rolled_back",
            Self::Failed => "failed",
        }
    }

    pub fn from_slug(raw: &str) -> Self {
        match raw {
            "committed" => Self::Committed,
            "rolled_back" => Self::RolledBack,
            "failed" => Self::Failed,
            _ => Self::Open,
        }
    }

    /// True when this transaction needs operator attention.
    pub const fn needs_recovery(self) -> bool {
        matches!(self, Self::Open | Self::Failed)
    }
}

/// A transaction record.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TxnRecord {
    pub id: String,
    pub command: String,
    pub status: TxnStatus,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// Journal file for this transaction, when one was written.
    pub journal: Option<PathBuf>,
}

/// A backup taken before a destructive step.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BackupRecord {
    pub id: i64,
    pub txn_id: String,
    /// The path whose previous contents were saved.
    pub original_path: PathBuf,
    /// Where the previous contents now live.
    pub backup_path: PathBuf,
    /// Digest of the saved content, which a restore puts back.
    pub digest_before: String,
    /// Digest the transaction then wrote to `original_path`.
    ///
    /// `rollback` compares the destination against this to tell "untouched since
    /// the transaction" from "edited afterwards". `None` means the write never
    /// completed, so the target may be half-written.
    pub digest_after: Option<String>,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_round_trip() {
        for kind in [
            SnapshotKind::UpstreamPristine,
            SnapshotKind::Canonical,
            SnapshotKind::DeploymentBaseline,
        ] {
            assert_eq!(SnapshotKind::from_slug(kind.slug()), Some(kind));
        }
        for mode in [DeployMode::Copy, DeployMode::Link] {
            assert_eq!(DeployMode::from_slug(mode.slug()), Some(mode));
        }
        for policy in [PinPolicy::Tracking, PinPolicy::Pinned] {
            assert_eq!(PinPolicy::from_slug(policy.slug()), policy);
        }
        assert_eq!(SnapshotKind::from_slug("nonsense"), None);
    }

    #[test]
    fn open_and_failed_transactions_need_recovery() {
        assert!(TxnStatus::Open.needs_recovery());
        assert!(TxnStatus::Failed.needs_recovery());
        assert!(!TxnStatus::Committed.needs_recovery());
        assert!(!TxnStatus::RolledBack.needs_recovery());
    }
}
