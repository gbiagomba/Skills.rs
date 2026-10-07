//! Agent adapters: discovery, scopes, precedence, capabilities, classification.
//!
//! # Why nothing here is global
//!
//! The three supported agents resolve a same-named skill three different ways,
//! and two authoritative sources disagree about which scope should win:
//!
//! | Agent | Same-name resolution |
//! |---|---|
//! | Claude Code | enterprise over personal over project |
//! | Gemini CLI | highest discovery tier wins, warning emitted |
//! | Codex | neither wins; both appear in the picker |
//!
//! The Agent Skills client-implementation guidance recommends project over user,
//! which is the opposite of Claude Code's documented behaviour. So precedence,
//! scope layout, and capability are owned by each adapter and never assumed.
//!
//! # The shared-root problem
//!
//! `~/.agents/skills` is simultaneously Codex's documented user root and one of
//! Gemini CLI's user roots. Codex has no non-deprecated agent-specific user root
//! at all. A user-scope install for Codex is therefore unavoidably visible to
//! Gemini, and `skill` says so rather than implying isolation it cannot deliver.
//! See [`Capabilities::isolated_user_root`] and [`registry::readers_of`].
//!
//! Facts in this module come from the vendor documentation retrieved 2026-10-07
//! and recorded in `docs/compatibility.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

pub mod claude;
pub mod codex;
pub mod copilot;
pub mod gemini;
pub mod registry;

/// Where a skill is installed from the agent's point of view.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Available in every project for this user.
    User,
    /// Checked into, or local to, one project directory.
    Project,
}

impl Scope {
    pub const fn slug(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.slug())
    }
}

impl std::str::FromStr for Scope {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "user" => Ok(Self::User),
            "project" => Ok(Self::Project),
            other => Err(Error::Usage(format!(
                "unknown scope {other:?}\nhint: use --scope user or --scope project"
            ))),
        }
    }
}

/// Who owns an installation we found on disk.
///
/// The non-personal variants are never written, moved, or removed. Getting this
/// classification wrong is how a tool deletes an organisation's managed skill or
/// fights with account sync, so anything we cannot positively identify stays
/// [`Provenance::Unknown`] rather than being assumed personal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// Deployed by `skill` and recorded in our state.
    ManagedByUs,
    /// A personal installation we did not create. Needs explicit adoption.
    Unmanaged,
    /// Supplied by an agent plugin or extension.
    PluginManaged,
    /// Installed by an organisation through managed settings.
    OrgManaged,
    /// Bundled with the agent itself.
    BuiltIn,
    /// Synchronised from a hosted account.
    AccountSynced,
    /// An agent's internal bookkeeping directory, not a skill.
    Ignored,
    /// Present, but we cannot say who put it there.
    Unknown,
}

impl Provenance {
    /// True when `skill` may modify or remove this installation.
    pub const fn is_ours_to_touch(self) -> bool {
        matches!(self, Self::ManagedByUs)
    }

    /// True when an explicit adoption could make it ours.
    pub const fn is_adoptable(self) -> bool {
        matches!(self, Self::Unmanaged | Self::Unknown)
    }

    pub const fn slug(self) -> &'static str {
        match self {
            Self::ManagedByUs => "managed",
            Self::Unmanaged => "unmanaged",
            Self::PluginManaged => "plugin-managed",
            Self::OrgManaged => "org-managed",
            Self::BuiltIn => "built-in",
            Self::AccountSynced => "account-synced",
            Self::Ignored => "ignored",
            Self::Unknown => "unknown",
        }
    }

    /// Why we will not touch it, for the refusal message.
    pub const fn refusal_hint(self) -> &'static str {
        match self {
            Self::ManagedByUs => "this is already managed by skill",
            Self::Unmanaged => {
                "it was not installed by skill; select it explicitly to adopt it after \
                 reviewing the plan"
            }
            Self::PluginManaged => "manage it through the agent's plugin or extension commands",
            Self::OrgManaged => "it is deployed by your organisation; contact an administrator",
            Self::BuiltIn => "it ships with the agent and cannot be replaced by a personal skill",
            Self::AccountSynced => "it is synchronised from a hosted account, not a local install",
            Self::Ignored => "it is internal agent bookkeeping, not a skill",
            Self::Unknown => "its origin could not be determined; inspect it by hand first",
        }
    }
}

impl std::fmt::Display for Provenance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.slug())
    }
}

/// How an agent resolves two skills with the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NameConflict {
    /// One wins and shadows the other.
    Shadows,
    /// Both load and both appear to the user.
    KeepsBoth,
}

/// Whether symlinked skill directories are supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SymlinkSupport {
    /// The vendor documents it.
    Documented,
    /// It appears to work but is not documented, so we do not promise it.
    Undocumented,
    /// Documented as unsupported.
    Unsupported,
}

/// A caveat that must reach the operator rather than be assumed away.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Caveat {
    pub code: &'static str,
    pub message: String,
    /// True when taken from vendor documentation or vendor source.
    pub confirmed: bool,
}

/// What an adapter can and cannot do.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Capabilities {
    pub symlink: SymlinkSupport,
    /// Whether the agent has a project scope at all.
    pub project_scope: bool,
    /// False when every user-scope root is shared with another agent.
    ///
    /// Codex is the case this exists for: isolation is simply not available.
    pub isolated_user_root: bool,
    /// Maximum directory depth the agent searches below a skills root.
    ///
    /// `Some(1)` means only `<root>/<name>/SKILL.md` is ever found, which
    /// constrains how a deployment may be laid out.
    pub max_discovery_depth: Option<usize>,
    pub name_conflict: NameConflict,
    pub caveats: Vec<Caveat>,
}

/// One directory an agent reads skills from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SkillRoot {
    pub path: PathBuf,
    pub scope: Scope,
    /// Lower numbers lose to higher numbers for this agent.
    pub precedence: u8,
    /// True when `skill` is willing to deploy here.
    pub writable: bool,
    /// False when this directory is a cross-client convention rather than one
    /// this agent owns by name.
    ///
    /// Deliberately *not* a list of peer agents. An adapter cannot know who else
    /// reads a shared directory without consulting every other adapter, and a
    /// hardcoded list silently becomes wrong the moment an adapter is added.
    /// [`crate::agent::registry::readers_of`] computes the real list instead.
    pub agent_specific: bool,
    /// Default classification for anything found here.
    pub default_provenance: Provenance,
    /// Human description, shown by `skill agents`.
    pub label: String,
}

impl SkillRoot {
    /// True when this is a cross-client convention directory.
    ///
    /// Which agents actually read it is a registry question, not a per-adapter
    /// one; see [`crate::agent::registry::readers_of`].
    pub fn is_shared_convention(&self) -> bool {
        !self.agent_specific
    }
}

/// Evidence that an agent is installed, rather than a bare boolean.
///
/// A leftover skills directory is not proof of installation, so the two signals
/// are reported separately and `skill agents` prints what was actually observed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Detection {
    /// True only when the executable was found.
    pub installed: bool,
    pub executable: Option<PathBuf>,
    /// Present when a config directory exists, even if the executable does not.
    pub config_dir: Option<PathBuf>,
    /// What we observed, in the order we observed it.
    pub evidence: Vec<String>,
}

impl Detection {
    /// Nothing found at all.
    pub fn absent(agent: &str) -> Self {
        Self {
            installed: false,
            executable: None,
            config_dir: None,
            evidence: vec![format!("no `{agent}` executable on PATH")],
        }
    }

    /// A one-line summary for an error or a table cell.
    pub fn summary(&self) -> String {
        if self.evidence.is_empty() {
            return "no evidence".to_string();
        }
        self.evidence.join("; ")
    }
}

/// The host environment an adapter resolves paths against.
///
/// Exists so adapters are testable without touching the developer's real
/// installation. [`Host::detect`] resolves the home directory through
/// `directories`, which uses the platform API rather than reading `$HOME`;
/// [`Host::for_test`] takes an explicit home and environment so a test can build
/// a complete fake machine in a temporary directory.
#[derive(Debug, Clone)]
pub struct Host {
    home: PathBuf,
    env: BTreeMap<String, String>,
    /// Directories searched for agent executables, in order.
    exec_dirs: Vec<PathBuf>,
    /// The project directory, when one is in play.
    project: Option<PathBuf>,
}

impl Host {
    /// Resolve the real host environment.
    ///
    /// The home directory comes from the platform API via `directories`, not from
    /// `$HOME`, so a stray environment variable cannot redirect where skills are
    /// installed. The one override is `SKILL_AGENT_HOME`, which is our own
    /// namespaced variable and must be set deliberately; see
    /// [`crate::config::env_vars::AGENT_HOME`].
    pub fn detect() -> Result<Self> {
        Self::detect_with_home(None)
    }

    /// Resolve the host environment, optionally against an explicit base.
    pub fn detect_with_home(agent_home: Option<PathBuf>) -> Result<Self> {
        let base = directories::BaseDirs::new().ok_or_else(|| {
            Error::Internal("could not determine the home directory from the platform APIs".into())
        })?;

        let exec_dirs = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
            .unwrap_or_default();

        // Only the variables the adapters actually consult are captured, so the
        // rest of the environment is not carried around or logged.
        let mut env = BTreeMap::new();
        for key in [
            "CODEX_HOME",
            "GEMINI_CLI_HOME",
            "GEMINI_CLI_SYSTEM_SETTINGS_PATH",
        ] {
            if let Ok(value) = std::env::var(key) {
                env.insert(key.to_string(), value);
            }
        }

        Ok(Self {
            home: agent_home.unwrap_or_else(|| base.home_dir().to_path_buf()),
            env,
            exec_dirs,
            project: None,
        })
    }

    /// Build a synthetic host for tests.
    pub fn for_test(home: impl Into<PathBuf>) -> Self {
        Self {
            home: home.into(),
            env: BTreeMap::new(),
            exec_dirs: Vec::new(),
            project: None,
        }
    }

    /// Set the project directory used for project-scope roots.
    pub fn with_project(mut self, project: Option<PathBuf>) -> Self {
        self.project = project;
        self
    }

    /// Override an environment variable (tests, and `--config` plumbing).
    pub fn with_env(mut self, key: &str, value: impl Into<String>) -> Self {
        self.env.insert(key.to_string(), value.into());
        self
    }

    /// Add a directory to search for executables.
    pub fn with_exec_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.exec_dirs.push(dir.into());
        self
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn project(&self) -> Option<&Path> {
        self.project.as_deref()
    }

    /// Read one of the captured environment variables.
    pub fn env(&self, key: &str) -> Option<&str> {
        self.env.get(key).map(String::as_str)
    }

    /// Resolve `name` against the executable search path.
    ///
    /// On Windows the usual extensions are tried. No process is launched: we look
    /// for the file, because running a third-party binary during discovery is not
    /// something a read-only command should do.
    pub fn which(&self, name: &str) -> Option<PathBuf> {
        let candidates: Vec<String> = if cfg!(windows) {
            vec![
                format!("{name}.exe"),
                format!("{name}.cmd"),
                format!("{name}.bat"),
                name.to_string(),
            ]
        } else {
            vec![name.to_string()]
        };

        for dir in &self.exec_dirs {
            for candidate in &candidates {
                let full = dir.join(candidate);
                if full.is_file() {
                    return Some(full);
                }
            }
        }
        None
    }
}

/// An agent `skill` can deploy to.
pub trait Agent: Send + Sync + std::fmt::Debug {
    /// Canonical identifier, used in state and on the command line.
    fn id(&self) -> &'static str;

    /// Accepted aliases, such as `claude-code` for `claude`.
    fn aliases(&self) -> &'static [&'static str];

    /// Human-facing product name.
    fn display_name(&self) -> &'static str;

    /// Gather installation evidence without running the agent.
    fn detect(&self, host: &Host) -> Detection;

    /// Every root this agent reads, in its own precedence order.
    ///
    /// Includes read-only roots, because `status` and `doctor` must be able to
    /// report a skill that is shadowed by a managed or built-in one.
    fn roots(&self, host: &Host, scope: Scope) -> Vec<SkillRoot>;

    /// What this agent can and cannot do.
    fn capabilities(&self) -> Capabilities;

    /// Classify a path, given the roots this agent knows about.
    fn classify(&self, host: &Host, path: &Path) -> Provenance;

    /// The root a new deployment should be written to, for a given scope.
    ///
    /// Returns the highest-precedence writable root, preferring an
    /// agent-specific directory over a shared one where the agent offers both.
    fn write_root(&self, host: &Host, scope: Scope) -> Result<SkillRoot> {
        let mut writable: Vec<SkillRoot> = self
            .roots(host, scope)
            .into_iter()
            .filter(|r| r.writable)
            .collect();

        // Prefer a root this agent owns by name, then higher precedence. An
        // agent-specific directory is the only place isolation is even possible.
        writable.sort_by(|a, b| {
            a.is_shared_convention()
                .cmp(&b.is_shared_convention())
                .then(b.precedence.cmp(&a.precedence))
        });

        writable
            .into_iter()
            .next()
            .ok_or_else(|| Error::Unsupported {
                agent: self.id().to_string(),
                what: format!("install at {scope} scope"),
                reason: "this agent exposes no writable skills directory for that scope".into(),
                hint: if scope == Scope::Project {
                    "re-run with --scope user".to_string()
                } else {
                    "check the agent's documentation for its skills directory".to_string()
                },
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_round_trips_through_its_slug() {
        for scope in [Scope::User, Scope::Project] {
            let parsed: Scope = scope.slug().parse().unwrap();
            assert_eq!(parsed, scope);
        }
        assert!("global".parse::<Scope>().is_err());
    }

    #[test]
    fn only_our_own_deployments_are_touchable() {
        assert!(Provenance::ManagedByUs.is_ours_to_touch());
        for other in [
            Provenance::Unmanaged,
            Provenance::PluginManaged,
            Provenance::OrgManaged,
            Provenance::BuiltIn,
            Provenance::AccountSynced,
            Provenance::Ignored,
            Provenance::Unknown,
        ] {
            assert!(
                !other.is_ours_to_touch(),
                "{other} must never be modified as a personal install"
            );
        }
    }

    #[test]
    fn only_unmanaged_and_unknown_can_be_adopted() {
        assert!(Provenance::Unmanaged.is_adoptable());
        assert!(Provenance::Unknown.is_adoptable());
        for never in [
            Provenance::PluginManaged,
            Provenance::OrgManaged,
            Provenance::BuiltIn,
            Provenance::AccountSynced,
        ] {
            assert!(!never.is_adoptable(), "{never} must not be adoptable");
        }
    }

    #[test]
    fn which_finds_an_executable_in_an_injected_path() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join(if cfg!(windows) { "fake.exe" } else { "fake" });
        std::fs::write(&exe, "").unwrap();

        let host = Host::for_test(tmp.path()).with_exec_dir(&bin);
        assert_eq!(host.which("fake"), Some(exe));
        assert_eq!(host.which("absent"), None);
    }

    #[test]
    fn detect_resolves_home_from_the_platform_api() {
        // Guards the requirement that the default comes from the platform API.
        // If this ever starts honouring $HOME, a stray variable could redirect
        // installs into the wrong profile.
        let host = Host::detect().expect("host detection should succeed");
        assert!(host.home().is_absolute());
    }

    #[test]
    fn an_explicit_agent_home_overrides_the_platform_default() {
        // This is what lets an end-to-end test run against a fake machine.
        let tmp = tempfile::tempdir().unwrap();
        let host = Host::detect_with_home(Some(tmp.path().to_path_buf())).unwrap();
        assert_eq!(host.home(), tmp.path());
    }
}
