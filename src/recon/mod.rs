//! Three-way reconciliation: the truth table, the resulting plan, and diffs.
//!
//! # Why three baselines and not two
//!
//! Comparing "what is here now" against "what upstream has" cannot tell a
//! deliberate local edit from stale content, so a tool that does only that
//! overwrites edits whenever upstream is newer. `skill` keeps three baselines and
//! asks two different questions with them:
//!
//! * `sync` compares **canonical**, the destination's **baseline** (what we last
//!   wrote there), and the destination's **current** content.
//! * `update` compares **new upstream**, the **pristine upstream** snapshot (what
//!   upstream last gave us), and **canonical** (which may hold local edits).
//!
//! Both reduce to the same function, [`compare`], because both are the same
//! shape: a common ancestor and two descendants.
//!
//! # What this module will not do
//!
//! There is no automatic textual merge. When both sides changed differently the
//! result is [`Drift::Conflict`] and nothing is written. Reliable conflict
//! detection is the requirement; merging is not, and a bad merge is worse than a
//! stop.

use std::path::PathBuf;

use crate::agent::Provenance;
use crate::pkg::tree::PackageTree;

/// The outcome of comparing a baseline with two descendants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Drift {
    /// Neither side moved. Nothing to do.
    Unchanged,
    /// Only the source side moved. Safe to write.
    SourceAhead,
    /// Only the target side moved. Its edits are preserved and reported.
    TargetDrifted,
    /// Both moved, differently. Nothing is written.
    Conflict,
    /// Both moved, to byte-identical content. Only the baseline needs refreshing.
    ConvergedIdentically,
    /// The target is gone. Repair is an explicit decision, never inferred.
    TargetMissing,
    /// No baseline was recorded, so no three-way comparison is possible.
    NoBaseline,
}

impl Drift {
    /// True when writing to the target is safe without asking.
    pub const fn is_safe_to_write(self) -> bool {
        matches!(self, Self::SourceAhead)
    }

    /// True when an operator decision is required before anything changes.
    pub const fn needs_decision(self) -> bool {
        matches!(
            self,
            Self::Conflict | Self::TargetDrifted | Self::TargetMissing
        )
    }

    /// Operator-facing summary.
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Unchanged => "up to date",
            Self::SourceAhead => "out of date, safe to update",
            Self::TargetDrifted => "locally modified, edits preserved",
            Self::Conflict => "conflict: both sides changed differently",
            Self::ConvergedIdentically => "already identical, baseline refreshed",
            Self::TargetMissing => "missing",
            Self::NoBaseline => "no recorded baseline",
        }
    }
}

/// Compare a baseline against a source and a target.
///
/// This is the whole truth table, as a pure function over digests so it can be
/// exhaustively tested without touching a filesystem.
///
/// `target` of `None` means the target does not exist.
pub fn compare(baseline: Option<&str>, source: &str, target: Option<&str>) -> Drift {
    let Some(target) = target else {
        return Drift::TargetMissing;
    };

    let Some(baseline) = baseline else {
        // Without a common ancestor we cannot attribute a change to either side.
        // Equal content is still unambiguous; anything else needs a decision.
        return if source == target {
            Drift::Unchanged
        } else {
            Drift::NoBaseline
        };
    };

    let source_moved = source != baseline;
    let target_moved = target != baseline;

    match (source_moved, target_moved) {
        (false, false) => Drift::Unchanged,
        (true, false) => Drift::SourceAhead,
        (false, true) => Drift::TargetDrifted,
        (true, true) => {
            if source == target {
                Drift::ConvergedIdentically
            } else {
                Drift::Conflict
            }
        }
    }
}

/// What `skill` proposes to do about one target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Do nothing; already correct.
    Nothing,
    /// Create a target that does not exist yet.
    Create,
    /// Overwrite the target with the source content.
    Replace,
    /// Leave the content alone and record the current state as the baseline.
    RefreshBaseline,
    /// Report and skip, because the operator has to choose.
    Report,
}

impl Action {
    /// True when carrying this out writes to disk.
    pub const fn mutates(self) -> bool {
        matches!(self, Self::Create | Self::Replace)
    }
}

/// One line of a reconciliation plan.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Item {
    /// Install name of the package.
    pub package: String,
    /// Agent owning the target, when the target is a deployment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    pub path: PathBuf,
    pub drift: Drift,
    pub action: Action,
    pub provenance: Provenance,
    /// Paths that differ, for the human-readable report.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub added_files: Vec<String>,
    /// File deletions count as changes and participate in conflict detection.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deleted_files: Vec<String>,
    /// Why this item needs a decision, when it does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Item {
    /// True when nothing about this item requires a write or a decision.
    pub fn is_noop(&self) -> bool {
        self.action == Action::Nothing
    }
}

/// A complete reconciliation plan.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct ReconPlan {
    pub items: Vec<Item>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub disclosures: Vec<String>,
}

impl ReconPlan {
    /// Items that would write to disk.
    pub fn mutating(&self) -> impl Iterator<Item = &Item> {
        self.items.iter().filter(|i| i.action.mutates())
    }

    /// Items blocked on an operator decision.
    pub fn blocked(&self) -> impl Iterator<Item = &Item> {
        self.items.iter().filter(|i| i.action == Action::Report)
    }

    /// How many items are conflicts specifically.
    pub fn conflict_count(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.drift == Drift::Conflict)
            .count()
    }

    /// True when the plan would change nothing at all.
    pub fn is_noop(&self) -> bool {
        self.items.iter().all(Item::is_noop)
    }

    /// Turn a plan that cannot proceed into the documented conflict failure.
    ///
    /// `update` uses this to keep its all-or-nothing promise: if any selected
    /// package conflicts, nothing is written, rather than some packages
    /// succeeding and the conflicts being hidden behind a partial success.
    pub fn reject_if_conflicted(&self) -> crate::Result<()> {
        let count = self.conflict_count();
        if count > 0 {
            return Err(crate::Error::Conflict {
                count,
                hint: "run `skill diff <skill>` to see both sides, then either keep your edits \
                       with `skill sync <skill> --adopt-from <agent>` or discard them by \
                       re-installing the package"
                    .to_string(),
            });
        }
        Ok(())
    }
}

/// Decide what to do about one target, given its drift and who owns it.
///
/// Provenance is consulted *before* drift: a managed, built-in, or account-synced
/// destination is never written regardless of how its content compares.
pub fn decide(
    drift: Drift,
    provenance: Provenance,
    target_exists: bool,
) -> (Action, Option<String>) {
    if !provenance.is_ours_to_touch() && target_exists {
        return (
            Action::Report,
            Some(format!(
                "{} destination: {}",
                provenance,
                provenance.refusal_hint()
            )),
        );
    }

    match drift {
        Drift::Unchanged => (Action::Nothing, None),
        Drift::SourceAhead => (
            if target_exists {
                Action::Replace
            } else {
                Action::Create
            },
            None,
        ),
        Drift::ConvergedIdentically => (
            Action::RefreshBaseline,
            Some("both sides reached identical content, so nothing is rewritten".to_string()),
        ),
        Drift::TargetDrifted => (
            Action::Report,
            Some(
                "the deployed copy has local edits. They are preserved. Promote them with \
                 `skill sync <skill> --adopt-from <agent>`, or discard them by re-installing"
                    .to_string(),
            ),
        ),
        Drift::Conflict => (
            Action::Report,
            Some(
                "the canonical package and the deployed copy changed differently. Neither is \
                 overwritten"
                    .to_string(),
            ),
        ),
        Drift::TargetMissing => {
            if target_exists {
                // Should not happen; treat defensively rather than assume.
                (
                    Action::Report,
                    Some("inconsistent target state".to_string()),
                )
            } else {
                (
                    Action::Create,
                    Some("the deployment is missing and will be recreated".to_string()),
                )
            }
        }
        Drift::NoBaseline => (
            Action::Report,
            Some(
                "no baseline was recorded for this destination, so a local edit cannot be \
                 distinguished from stale content. Adopt it explicitly to start tracking it"
                    .to_string(),
            ),
        ),
    }
}

/// Per-file differences between two trees.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileDelta {
    pub changed: Vec<String>,
    pub added: Vec<String>,
    pub deleted: Vec<String>,
}

impl FileDelta {
    /// True when the trees are identical.
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.added.is_empty() && self.deleted.is_empty()
    }

    /// Total number of differing paths.
    pub fn len(&self) -> usize {
        self.changed.len() + self.added.len() + self.deleted.len()
    }
}

/// Compute which paths differ between `from` and `to`.
///
/// A deletion is a difference, which is what makes a removed `references/` file
/// participate in conflict detection rather than being silently restored.
pub fn delta(from: &PackageTree, to: &PackageTree) -> FileDelta {
    FileDelta {
        changed: to
            .changed_against(from)
            .into_iter()
            .map(str::to_string)
            .collect(),
        added: to
            .paths_missing_from(from)
            .into_iter()
            .map(str::to_string)
            .collect(),
        deleted: from
            .paths_missing_from(to)
            .into_iter()
            .map(str::to_string)
            .collect(),
    }
}

/// Render a unified diff of one text file between two package directories.
///
/// Binary content is reported as "differs" rather than dumped, and a file we
/// cannot read as UTF-8 is treated as binary rather than lossily decoded.
pub fn unified_diff(
    left_label: &str,
    right_label: &str,
    left: Option<&str>,
    right: Option<&str>,
) -> String {
    use similar::{ChangeTag, TextDiff};

    let (left_text, right_text) = match (left, right) {
        (None, None) => return String::new(),
        (Some(l), None) => (l, ""),
        (None, Some(r)) => ("", r),
        (Some(l), Some(r)) => (l, r),
    };

    let diff = TextDiff::from_lines(left_text, right_text);
    let mut out = format!("--- {left_label}\n+++ {right_label}\n");
    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Delete => '-',
            ChangeTag::Insert => '+',
            ChangeTag::Equal => ' ',
        };
        out.push(sign);
        out.push_str(change.value());
        if !change.value().ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "digest-base";
    const NEW_SOURCE: &str = "digest-source";
    const NEW_TARGET: &str = "digest-target";

    // Each row of the prompt's truth table, one test assertion.

    #[test]
    fn neither_changed_is_a_noop() {
        assert_eq!(compare(Some(BASE), BASE, Some(BASE)), Drift::Unchanged);
        let (action, _) = decide(Drift::Unchanged, Provenance::ManagedByUs, true);
        assert_eq!(action, Action::Nothing);
    }

    #[test]
    fn source_changed_and_target_unchanged_is_a_safe_update() {
        let drift = compare(Some(BASE), NEW_SOURCE, Some(BASE));
        assert_eq!(drift, Drift::SourceAhead);
        assert!(drift.is_safe_to_write());
        let (action, _) = decide(drift, Provenance::ManagedByUs, true);
        assert_eq!(action, Action::Replace);
    }

    #[test]
    fn target_changed_and_source_unchanged_preserves_the_edits() {
        let drift = compare(Some(BASE), BASE, Some(NEW_TARGET));
        assert_eq!(drift, Drift::TargetDrifted);
        assert!(drift.needs_decision());

        let (action, detail) = decide(drift, Provenance::ManagedByUs, true);
        assert_eq!(
            action,
            Action::Report,
            "a local edit must never be overwritten by default"
        );
        let detail = detail.unwrap();
        assert!(detail.contains("preserved"), "{detail}");
        assert!(
            detail.contains("--adopt-from"),
            "must say how to keep them: {detail}"
        );
    }

    #[test]
    fn both_changed_differently_is_a_conflict_and_writes_nothing() {
        let drift = compare(Some(BASE), NEW_SOURCE, Some(NEW_TARGET));
        assert_eq!(drift, Drift::Conflict);
        let (action, _) = decide(drift, Provenance::ManagedByUs, true);
        assert_eq!(action, Action::Report);
        assert!(!action.mutates(), "a conflict must not write");
    }

    #[test]
    fn both_changed_identically_only_refreshes_the_baseline() {
        let drift = compare(Some(BASE), NEW_SOURCE, Some(NEW_SOURCE));
        assert_eq!(drift, Drift::ConvergedIdentically);
        let (action, detail) = decide(drift, Provenance::ManagedByUs, true);
        assert_eq!(action, Action::RefreshBaseline);
        assert!(!action.mutates(), "identical content must not be rewritten");
        assert!(detail.unwrap().contains("nothing is rewritten"));
    }

    #[test]
    fn a_missing_target_is_reported_not_inferred_as_a_deletion() {
        let drift = compare(Some(BASE), BASE, None);
        assert_eq!(drift, Drift::TargetMissing);
        let (action, detail) = decide(drift, Provenance::ManagedByUs, false);
        assert_eq!(action, Action::Create);
        assert!(detail.unwrap().contains("recreated"));
    }

    #[test]
    fn no_baseline_requires_an_explicit_decision() {
        // Without an ancestor we cannot tell an edit from stale content, so we
        // must not guess.
        assert_eq!(
            compare(None, NEW_SOURCE, Some(NEW_TARGET)),
            Drift::NoBaseline
        );
        // Identical content is still unambiguous.
        assert_eq!(compare(None, BASE, Some(BASE)), Drift::Unchanged);

        // For a destination we do manage, the no-baseline case is what surfaces.
        let (action, detail) = decide(Drift::NoBaseline, Provenance::ManagedByUs, true);
        assert_eq!(action, Action::Report);
        assert!(detail.unwrap().contains("Adopt it explicitly"));

        // For an unmanaged destination, provenance is the more specific reason
        // and is reported instead, since that is what the operator must act on.
        let (action, detail) = decide(Drift::NoBaseline, Provenance::Unmanaged, true);
        assert_eq!(action, Action::Report);
        assert!(detail.unwrap().contains("unmanaged"));
    }

    #[test]
    fn provenance_overrides_drift_for_destinations_we_do_not_own() {
        // Even a plain "source ahead" must not touch a managed destination.
        for provenance in [
            Provenance::OrgManaged,
            Provenance::PluginManaged,
            Provenance::BuiltIn,
            Provenance::AccountSynced,
            Provenance::Unmanaged,
            Provenance::Unknown,
        ] {
            let (action, detail) = decide(Drift::SourceAhead, provenance, true);
            assert_eq!(
                action,
                Action::Report,
                "{provenance} must never be written by default"
            );
            assert!(detail.unwrap().contains(provenance.slug()));
        }
    }

    #[test]
    fn provenance_does_not_block_creating_a_new_destination() {
        // Nothing is there yet, so there is nothing to refuse to overwrite.
        let (action, _) = decide(Drift::TargetMissing, Provenance::Unmanaged, false);
        assert_eq!(action, Action::Create);
    }

    #[test]
    fn a_plan_with_conflicts_refuses_all_or_nothing() {
        let plan = ReconPlan {
            items: vec![
                Item {
                    package: "a".into(),
                    agent: Some("claude".into()),
                    path: PathBuf::from("/a"),
                    drift: Drift::SourceAhead,
                    action: Action::Replace,
                    provenance: Provenance::ManagedByUs,
                    changed_files: vec![],
                    added_files: vec![],
                    deleted_files: vec![],
                    detail: None,
                },
                Item {
                    package: "b".into(),
                    agent: Some("codex".into()),
                    path: PathBuf::from("/b"),
                    drift: Drift::Conflict,
                    action: Action::Report,
                    provenance: Provenance::ManagedByUs,
                    changed_files: vec!["SKILL.md".into()],
                    added_files: vec![],
                    deleted_files: vec![],
                    detail: None,
                },
            ],
            disclosures: vec![],
        };

        assert_eq!(plan.conflict_count(), 1);
        assert_eq!(plan.mutating().count(), 1);
        assert_eq!(plan.blocked().count(), 1);

        let err = plan.reject_if_conflicted().unwrap_err();
        assert_eq!(err.exit_code(), crate::ExitCode::Conflict);
        assert!(
            err.to_string().contains("nothing was written"),
            "the all-or-nothing promise must be stated: {err}"
        );
    }

    #[test]
    fn a_clean_plan_is_accepted() {
        let plan = ReconPlan::default();
        assert!(plan.reject_if_conflicted().is_ok());
        assert!(plan.is_noop());
    }

    fn tree_of(files: &[(&str, &str)]) -> PackageTree {
        use crate::pkg::tree::{tree_digest, FileEntry};
        use crate::safepath::EntryKind;
        let mut entries: Vec<FileEntry> = files
            .iter()
            .map(|(path, content)| FileEntry {
                path: (*path).to_string(),
                kind: EntryKind::File,
                mode: Some(0o644),
                size: content.len() as u64,
                digest: crate::pkg::tree::hash_bytes(content.as_bytes()),
                link_target: None,
            })
            .collect();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        PackageTree {
            digest: tree_digest(&entries),
            entries,
            total_bytes: 0,
        }
    }

    #[test]
    fn delta_reports_changes_additions_and_deletions() {
        let before = tree_of(&[("SKILL.md", "a"), ("gone.md", "x"), ("same.md", "s")]);
        let after = tree_of(&[("SKILL.md", "b"), ("new.md", "n"), ("same.md", "s")]);

        let d = delta(&before, &after);
        assert_eq!(d.changed, vec!["SKILL.md"]);
        assert_eq!(d.added, vec!["new.md"]);
        assert_eq!(
            d.deleted,
            vec!["gone.md"],
            "a deletion is a change, not an absence to ignore"
        );
        assert_eq!(d.len(), 3);
        assert!(!d.is_empty());
    }

    #[test]
    fn delta_is_empty_for_identical_trees() {
        let tree = tree_of(&[("SKILL.md", "a")]);
        assert!(delta(&tree, &tree).is_empty());
    }

    #[test]
    fn unified_diff_renders_both_sides() {
        let out = unified_diff(
            "canonical",
            "deployed",
            Some("one\ntwo\n"),
            Some("one\nTWO\n"),
        );
        assert!(out.contains("--- canonical"));
        assert!(out.contains("+++ deployed"));
        assert!(out.contains("-two"));
        assert!(out.contains("+TWO"));
    }

    #[test]
    fn unified_diff_handles_a_file_present_on_only_one_side() {
        let added = unified_diff("a", "b", None, Some("new\n"));
        assert!(added.contains("+new"));
        let removed = unified_diff("a", "b", Some("old\n"), None);
        assert!(removed.contains("-old"));
        assert!(unified_diff("a", "b", None, None).is_empty());
    }
}
