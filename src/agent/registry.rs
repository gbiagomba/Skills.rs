//! Agent lookup, alias resolution, and destination planning.
//!
//! This is where the shared-root problem is actually handled. Two selected agents
//! can resolve to one physical directory, and writing it twice would be wasteful
//! at best and destructive at worst (a later `migrate` could delete a directory
//! another agent still depends on). So destinations are canonicalised and
//! deduplicated here, and a deployment that lands in a directory an unselected
//! agent also reads is disclosed rather than quietly made visible.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::claude::ClaudeCode;
use super::codex::Codex;
use super::gemini::GeminiCli;
use super::{Agent, Detection, Host, Provenance, Scope, SkillRoot};
use crate::error::{Error, Result};
use crate::pkg::identity::InstallName;

/// Every adapter this build supports, in a stable order.
pub fn all() -> Vec<&'static dyn Agent> {
    // The adapters are zero-sized and stateless, so static references are fine
    // and no allocation or plugin machinery is needed.
    static CLAUDE: ClaudeCode = ClaudeCode;
    static CODEX: Codex = Codex;
    static GEMINI: GeminiCli = GeminiCli;
    vec![&CLAUDE, &CODEX, &GEMINI]
}

/// Canonical ids, for error messages.
pub fn known_ids() -> String {
    all().iter().map(|a| a.id()).collect::<Vec<_>>().join(", ")
}

/// Resolve an id or alias to an adapter.
///
/// Matching is case-insensitive, because an operator typing `Claude` means
/// `claude` and failing on that would be pedantry rather than safety.
pub fn resolve(name: &str) -> Result<&'static dyn Agent> {
    let wanted = name.trim().to_lowercase();
    for agent in all() {
        if agent.id() == wanted || agent.aliases().iter().any(|a| *a == wanted) {
            return Ok(agent);
        }
    }

    // Offer the closest known name rather than only listing everything.
    let hint = all()
        .iter()
        .find(|a| a.id().starts_with(wanted.chars().next().unwrap_or('\0')))
        .map(|a| format!("{}, or one of {}", a.id(), known_ids()))
        .unwrap_or_else(known_ids);

    Err(Error::UnknownAgent {
        name: name.to_string(),
        known: hint,
    })
}

/// Resolve several names, rejecting duplicates so a plan cannot double-count.
pub fn resolve_many(names: &[String]) -> Result<Vec<&'static dyn Agent>> {
    let mut out: Vec<&'static dyn Agent> = Vec::with_capacity(names.len());
    for name in names {
        let agent = resolve(name)?;
        if out.iter().any(|a| a.id() == agent.id()) {
            // Naming the same agent twice is a mistake worth pointing out, since
            // it usually means an alias was used alongside the canonical id.
            continue;
        }
        out.push(agent);
    }
    Ok(out)
}

/// Detect every agent, returning adapters paired with their evidence.
pub fn detect_all(host: &Host) -> Vec<(&'static dyn Agent, Detection)> {
    all().into_iter().map(|a| (a, a.detect(host))).collect()
}

/// Only the agents whose executable was actually found.
pub fn detected(host: &Host) -> Vec<&'static dyn Agent> {
    detect_all(host)
        .into_iter()
        .filter(|(_, d)| d.installed)
        .map(|(a, _)| a)
        .collect()
}

/// One place a package will be deployed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Destination {
    pub agent: String,
    pub scope: Scope,
    /// The skills root this deployment sits in.
    pub root: PathBuf,
    /// The full deployment path, `root/<install-name>`.
    pub path: PathBuf,
    /// Agents other than `agent` that also read this location.
    pub also_visible_to: Vec<String>,
    /// Other selected agents that resolved to this very path.
    ///
    /// Non-empty means the write is shared and must happen once, not once per
    /// agent.
    pub shared_with_selected: Vec<String>,
}

impl Destination {
    /// True when something other than the owning agent can see this deployment.
    pub fn is_shared(&self) -> bool {
        !self.also_visible_to.is_empty() || !self.shared_with_selected.is_empty()
    }
}

/// A resolved set of destinations plus everything the operator should be told.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// Deduplicated destinations: one entry per distinct physical path.
    pub destinations: Vec<Destination>,
    /// Disclosures about shared visibility and deduplication.
    pub disclosures: Vec<String>,
    /// Agents that were requested but not detected on this machine.
    pub undetected: Vec<String>,
}

/// Resolve where a package should go for the given agents and scope.
///
/// Destinations are keyed by their *physical* path. When a path already exists we
/// canonicalise it; when it does not, we canonicalise the deepest existing
/// ancestor and re-append the remainder, so two agents whose roots are different
/// strings but the same directory (a symlinked `~/.claude/skills`, say) collapse
/// into one write.
pub fn plan_destinations(
    agents: &[&'static dyn Agent],
    host: &Host,
    scope: Scope,
    name: &InstallName,
) -> Result<Plan> {
    let mut plan = Plan::default();
    // Physical path to the destination that owns it.
    let mut by_physical: BTreeMap<PathBuf, usize> = BTreeMap::new();

    for agent in agents {
        let detection = agent.detect(host);
        if !detection.installed {
            // An explicitly named agent is still configured, which the prompt
            // requires us to honour, but the condition is reported.
            plan.undetected.push(agent.id().to_string());
            plan.disclosures.push(format!(
                "{} was named explicitly but not detected on this machine ({}); the deployment \
                 will be written anyway",
                agent.display_name(),
                detection.summary()
            ));
        }

        let root = agent.write_root(host, scope)?;
        let path = root.path.join(name.as_str());
        let physical = physical_key(&path);

        if let Some(&existing) = by_physical.get(&physical) {
            // Two selected agents resolve to one directory. Write once.
            let owner: &mut Destination = &mut plan.destinations[existing];
            owner.shared_with_selected.push(agent.id().to_string());
            plan.disclosures.push(format!(
                "{} and {} both resolve to {}, so it is written once rather than twice",
                owner.agent,
                agent.id(),
                path.display()
            ));
            continue;
        }

        let also_visible_to: Vec<String> = root
            .shared_with
            .iter()
            .filter(|other| !agents.iter().any(|a| a.id() == **other))
            .map(|s| (*s).to_string())
            .collect();

        if !also_visible_to.is_empty() {
            plan.disclosures.push(shared_root_disclosure(
                agent.display_name(),
                &root,
                &also_visible_to,
            ));
        }

        by_physical.insert(physical, plan.destinations.len());
        plan.destinations.push(Destination {
            agent: agent.id().to_string(),
            scope,
            root: root.path.clone(),
            path,
            also_visible_to,
            shared_with_selected: Vec::new(),
        });
    }

    Ok(plan)
}

/// Build the disclosure text for a deployment landing in a shared root.
fn shared_root_disclosure(display: &str, root: &SkillRoot, others: &[String]) -> String {
    format!(
        "{display} will be installed into {}, which {} also read{}. This skill will be visible \
         to {} even though {} not selected, and {display} offers no alternative location, so \
         isolation is not available here.",
        root.path.display(),
        others.join(" and "),
        if others.len() == 1 { "s" } else { "" },
        others.join(" and "),
        if others.len() == 1 {
            "it was"
        } else {
            "they were"
        },
    )
}

/// A comparable key for a path that may not exist yet.
///
/// Resolves the deepest existing ancestor through the filesystem and re-appends
/// the rest, so `~/.claude/skills/x` and a symlinked equivalent produce the same
/// key without requiring either to exist.
pub fn physical_key(path: &Path) -> PathBuf {
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path.to_path_buf();

    loop {
        if current.exists() {
            if let Ok(real) = std::fs::canonicalize(&current) {
                let mut out = real;
                for part in tail.iter().rev() {
                    out.push(part);
                }
                return out;
            }
        }
        match current.file_name() {
            Some(name) => {
                tail.push(name.to_os_string());
                match current.parent() {
                    Some(parent) if parent != current => current = parent.to_path_buf(),
                    _ => break,
                }
            }
            None => break,
        }
    }

    path.to_path_buf()
}

/// Classify a deployment path by asking the owning agent.
pub fn classify_for(agent_id: &str, host: &Host, path: &Path) -> Result<Provenance> {
    Ok(resolve(agent_id)?.classify(host, path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn name() -> InstallName {
        InstallName::parse("my-skill").unwrap()
    }

    #[test]
    fn resolves_ids_and_documented_aliases() {
        assert_eq!(resolve("claude").unwrap().id(), "claude");
        assert_eq!(resolve("claude-code").unwrap().id(), "claude");
        assert_eq!(resolve("gemini").unwrap().id(), "gemini");
        assert_eq!(resolve("gemini-cli").unwrap().id(), "gemini");
        assert_eq!(resolve("codex").unwrap().id(), "codex");
        // Case and surrounding space should not matter.
        assert_eq!(resolve("  Claude-Code ").unwrap().id(), "claude");
    }

    #[test]
    fn rejects_an_unknown_agent_and_lists_the_known_ones() {
        let err = resolve("cursor").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("claude"), "must list options: {text}");
        assert!(matches!(err, Error::UnknownAgent { .. }));
    }

    #[test]
    fn resolve_many_collapses_an_alias_and_its_canonical_id() {
        let agents = resolve_many(&["claude".to_string(), "claude-code".to_string()]).unwrap();
        assert_eq!(agents.len(), 1, "the same agent must not be planned twice");
    }

    #[test]
    fn codex_user_scope_discloses_gemini_visibility() {
        let tmp = tempfile::tempdir().unwrap();
        let host = Host::for_test(tmp.path());
        let agents = resolve_many(&["codex".to_string()]).unwrap();

        let plan = plan_destinations(&agents, &host, Scope::User, &name()).unwrap();
        assert_eq!(plan.destinations.len(), 1);
        assert_eq!(plan.destinations[0].also_visible_to, vec!["gemini"]);

        let disclosure = plan.disclosures.join(" ");
        assert!(
            disclosure.contains("visible to gemini"),
            "shared visibility must be disclosed: {disclosure}"
        );
        assert!(
            disclosure.contains("isolation is not available"),
            "we must not imply isolation Codex cannot provide: {disclosure}"
        );
    }

    #[test]
    fn selecting_both_codex_and_gemini_does_not_double_report_visibility() {
        // Gemini is selected, so "also visible to gemini" is not news.
        let tmp = tempfile::tempdir().unwrap();
        let host = Host::for_test(tmp.path());
        let agents = resolve_many(&["codex".to_string(), "gemini".to_string()]).unwrap();

        let plan = plan_destinations(&agents, &host, Scope::User, &name()).unwrap();
        assert!(plan.destinations[0].also_visible_to.is_empty());
        // And they land in different directories, because Gemini prefers its own.
        assert_eq!(plan.destinations.len(), 2);
        assert!(plan.destinations[1]
            .path
            .ends_with(".gemini/skills/my-skill"));
    }

    #[test]
    fn two_agents_resolving_to_one_directory_are_written_once() {
        let tmp = tempfile::tempdir().unwrap();
        // Make Claude's personal root a symlink to Codex's shared root, which is
        // the real-world shape that would otherwise be written twice.
        let shared = tmp.path().join(".agents/skills");
        fs::create_dir_all(&shared).unwrap();
        fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&shared, tmp.path().join(".claude/skills")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&shared, tmp.path().join(".claude/skills")).unwrap();

        let host = Host::for_test(tmp.path());
        let agents = resolve_many(&["claude".to_string(), "codex".to_string()]).unwrap();
        let plan = plan_destinations(&agents, &host, Scope::User, &name()).unwrap();

        assert_eq!(
            plan.destinations.len(),
            1,
            "a shared physical target must be one write, not two"
        );
        assert_eq!(plan.destinations[0].shared_with_selected, vec!["codex"]);
        assert!(plan
            .disclosures
            .iter()
            .any(|d| d.contains("written once rather than twice")));
    }

    #[test]
    fn an_undetected_but_named_agent_is_honoured_and_reported() {
        let tmp = tempfile::tempdir().unwrap();
        // No exec dirs, so nothing is detected.
        let host = Host::for_test(tmp.path());
        let agents = resolve_many(&["claude".to_string()]).unwrap();

        let plan = plan_destinations(&agents, &host, Scope::User, &name()).unwrap();
        assert_eq!(plan.undetected, vec!["claude"]);
        assert_eq!(plan.destinations.len(), 1, "it must still be planned");
        assert!(plan
            .disclosures
            .iter()
            .any(|d| d.contains("not detected on this machine")));
    }

    #[test]
    fn physical_key_collapses_a_symlinked_root() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&real, &link).unwrap();

        // Neither leaf exists; the key must still match.
        assert_eq!(
            physical_key(&link.join("pkg")),
            physical_key(&real.join("pkg"))
        );
    }

    #[test]
    fn project_scope_without_a_project_dir_fails_actionably() {
        let tmp = tempfile::tempdir().unwrap();
        let host = Host::for_test(tmp.path());
        let agents = resolve_many(&["claude".to_string()]).unwrap();

        let err = plan_destinations(&agents, &host, Scope::Project, &name()).unwrap_err();
        let text = err.to_string();
        assert!(matches!(err, Error::Unsupported { .. }), "{text}");
        assert!(
            text.contains("--scope user"),
            "must suggest a way out: {text}"
        );
    }
}
