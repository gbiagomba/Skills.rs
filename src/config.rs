//! Layered configuration and the platform directories `skill` uses.
//!
//! Precedence, lowest to highest: built-in defaults, then the config file, then
//! environment variables, then command-line flags. The flag layer is applied by
//! the CLI via [`Config::apply_overrides`] so that this module stays independent
//! of `clap`.
//!
//! # Where things live, and why they live apart
//!
//! * The **canonical store** defaults to `~/skills`. It holds package content and
//!   nothing else.
//! * **Manager metadata** (the state database, transaction journals, backups)
//!   lives in the platform data directory, and downloads in the platform cache
//!   directory. Keeping state out of the store matters: a skill directory is
//!   content we hash and compare, so a database file inside it would show up as
//!   package drift on the next `sync`.
//!
//! The home directory is resolved through the platform API, not `$HOME`. Tests
//! isolate themselves with `--store`, `--config`, and the `SKILL_*` variables on
//! a scoped child process rather than by rewriting the developer's environment.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agent::Scope;
use crate::error::{Error, IoContext, Result};
use crate::safepath::Limits;

/// Environment variables `skill` reads. Documented in the README.
pub mod env_vars {
    /// Override the canonical store location.
    pub const STORE: &str = "SKILL_STORE";
    /// Override the config file path.
    pub const CONFIG: &str = "SKILL_CONFIG";
    /// Override the directory holding state, journals, and backups.
    pub const STATE_DIR: &str = "SKILL_STATE_DIR";
    /// Override the download cache directory.
    pub const CACHE_DIR: &str = "SKILL_CACHE_DIR";
    /// Set to `1` to refuse all network access.
    pub const OFFLINE: &str = "SKILL_OFFLINE";
    /// Set to `1` to never prompt, as in CI.
    pub const NO_INTERACTIVE: &str = "SKILL_NO_INTERACTIVE";
    /// Base directory that agent skill roots resolve against.
    ///
    /// Defaults to the platform home directory, which is resolved through the
    /// OS API rather than `$HOME`. This override exists so the acceptance tests
    /// can build a complete fake machine in a temporary directory without
    /// touching a real agent installation, and so an unusual deployment can
    /// point `skill` at a different profile. It is namespaced deliberately:
    /// `skill` never honours `$HOME` for this.
    pub const AGENT_HOME: &str = "SKILL_AGENT_HOME";

    /// Every variable, for `skill doctor` and the README table.
    pub const ALL: [&str; 7] = [
        STORE,
        CONFIG,
        STATE_DIR,
        CACHE_DIR,
        OFFLINE,
        NO_INTERACTIVE,
        AGENT_HOME,
    ];
}

/// Resolved filesystem locations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Canonical package store, `~/skills` by default.
    pub store: PathBuf,
    /// The config file, which need not exist.
    pub config_file: PathBuf,
    /// State database, journals, and backups.
    pub state_dir: PathBuf,
    /// Scratch space for downloads and staging.
    pub cache_dir: PathBuf,
}

impl Paths {
    /// Resolve the default locations for this platform.
    pub fn detect() -> Result<Self> {
        let base = directories::BaseDirs::new().ok_or_else(|| {
            Error::Internal("could not determine the home directory from the platform APIs".into())
        })?;
        let dirs = directories::ProjectDirs::from("", "", "skill").ok_or_else(|| {
            Error::Internal("could not determine the platform application directories".into())
        })?;

        Ok(Self {
            store: base.home_dir().join("skills"),
            config_file: dirs.config_dir().join("config.toml"),
            state_dir: dirs.data_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
        })
    }

    /// Place every location under one directory. Used by tests and `--store`
    /// style isolation so a run cannot touch the real installation.
    pub fn rooted_at(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Self {
            store: root.join("store"),
            config_file: root.join("config/config.toml"),
            state_dir: root.join("state"),
            cache_dir: root.join("cache"),
        }
    }

    /// The state database file.
    pub fn database(&self) -> PathBuf {
        self.state_dir.join("state.sqlite3")
    }

    /// Directory holding transaction journals.
    pub fn journal_dir(&self) -> PathBuf {
        self.state_dir.join("transactions")
    }

    /// Directory holding pre-mutation backups.
    pub fn backup_dir(&self) -> PathBuf {
        self.state_dir.join("backups")
    }

    /// The advisory lock file guarding store and state mutation.
    pub fn lock_file(&self) -> PathBuf {
        self.state_dir.join("skill.lock")
    }

    /// Create the private directories, restricting them to the current user.
    ///
    /// The store itself is left at the platform default permissions, because it
    /// holds content the operator may well want to share or commit. State,
    /// journals, and backups are user-only.
    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(&self.store).ctx("creating the store", &self.store)?;

        for dir in [&self.state_dir, &self.cache_dir] {
            std::fs::create_dir_all(dir).ctx("creating a state directory", dir)?;
            crate::safepath::restrict_dir(dir)?;
        }
        for dir in [self.journal_dir(), self.backup_dir()] {
            std::fs::create_dir_all(&dir).ctx("creating a state directory", &dir)?;
            crate::safepath::restrict_dir(&dir)?;
        }
        if let Some(parent) = self.config_file.parent() {
            std::fs::create_dir_all(parent).ctx("creating the config directory", parent)?;
        }
        Ok(())
    }
}

/// The file-backed part of the configuration.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    /// Canonical store location. `~` is expanded against the real home.
    pub store: Option<String>,
    /// Agents to use when none are given on the command line.
    ///
    /// Having this set is what lets a non-interactive run succeed without an
    /// explicit destination list.
    #[serde(default)]
    pub default_agents: Vec<String>,
    /// Scope to use when `--scope` is absent.
    pub default_scope: Option<String>,
    /// Permit plain HTTP without `--allow-http`. Off unless set.
    pub allow_http: Option<bool>,
    /// Refuse all network access.
    pub offline: Option<bool>,
    /// Agents to configure even when their executable is absent.
    #[serde(default)]
    pub configured_agents: Vec<String>,
    #[serde(default)]
    pub limits: LimitsFile,
}

/// Overridable subset of [`Limits`].
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsFile {
    pub max_download_bytes: Option<u64>,
    pub max_extract_bytes: Option<u64>,
    pub max_file_bytes: Option<u64>,
    pub max_files: Option<usize>,
    pub max_depth: Option<usize>,
    pub max_redirects: Option<u32>,
    pub timeout_secs: Option<u64>,
}

impl LimitsFile {
    fn apply(&self, base: Limits) -> Limits {
        Limits {
            max_download_bytes: self.max_download_bytes.unwrap_or(base.max_download_bytes),
            max_extract_bytes: self.max_extract_bytes.unwrap_or(base.max_extract_bytes),
            max_file_bytes: self.max_file_bytes.unwrap_or(base.max_file_bytes),
            max_files: self.max_files.unwrap_or(base.max_files),
            max_depth: self.max_depth.unwrap_or(base.max_depth),
            max_redirects: self.max_redirects.unwrap_or(base.max_redirects),
            timeout_secs: self.timeout_secs.unwrap_or(base.timeout_secs),
        }
    }
}

/// Command-line values that override the file and the environment.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub store: Option<PathBuf>,
    pub config: Option<PathBuf>,
    pub scope: Option<Scope>,
    pub project_dir: Option<PathBuf>,
    pub allow_http: Option<bool>,
    pub offline: Option<bool>,
    pub yes: bool,
    pub dry_run: bool,
    pub json: bool,
    pub verbose: bool,
}

/// Fully resolved configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub paths: Paths,
    pub limits: Limits,
    pub default_agents: Vec<String>,
    pub configured_agents: Vec<String>,
    pub scope: Scope,
    pub project_dir: Option<PathBuf>,
    pub allow_http: bool,
    pub offline: bool,
    pub yes: bool,
    pub dry_run: bool,
    pub json: bool,
    pub verbose: bool,
    /// False when there is no terminal, or `SKILL_NO_INTERACTIVE` is set.
    pub interactive: bool,
    /// Set when a config file was found and read.
    pub config_file_loaded: Option<PathBuf>,
    /// Base for agent skill roots, when overridden. `None` means the platform home.
    pub agent_home: Option<PathBuf>,
}

impl Config {
    /// Resolve configuration from defaults, file, environment, and flags.
    ///
    /// `env` is passed in rather than read globally so tests can describe a whole
    /// environment without mutating the process.
    pub fn resolve(
        overrides: &Overrides,
        env: &BTreeMap<String, String>,
        interactive_tty: bool,
    ) -> Result<Self> {
        let mut paths = Paths::detect()?;

        // Environment layer for locations.
        if let Some(value) = env.get(env_vars::STORE) {
            paths.store = PathBuf::from(value);
        }
        if let Some(value) = env.get(env_vars::STATE_DIR) {
            paths.state_dir = PathBuf::from(value);
        }
        if let Some(value) = env.get(env_vars::CACHE_DIR) {
            paths.cache_dir = PathBuf::from(value);
        }
        if let Some(value) = env.get(env_vars::CONFIG) {
            paths.config_file = PathBuf::from(value);
        }
        // Flag layer for locations, which wins over the environment.
        if let Some(path) = &overrides.config {
            paths.config_file = path.clone();
        }

        let file = load_file(&paths.config_file)?;
        let config_file_loaded = file.as_ref().map(|_| paths.config_file.clone());
        let file = file.unwrap_or_default();

        // The store may also come from the file, but only when neither the
        // environment nor a flag set it.
        if env.get(env_vars::STORE).is_none() && overrides.store.is_none() {
            if let Some(raw) = &file.store {
                paths.store = expand_home(raw)?;
            }
        }
        if let Some(path) = &overrides.store {
            paths.store = path.clone();
        }

        let truthy = |value: &String| matches!(value.as_str(), "1" | "true" | "yes" | "on");

        let offline = overrides
            .offline
            .or_else(|| env.get(env_vars::OFFLINE).map(truthy))
            .or(file.offline)
            .unwrap_or(false);

        let allow_http = overrides.allow_http.or(file.allow_http).unwrap_or(false);

        let scope = match overrides.scope {
            Some(scope) => scope,
            None => match file.default_scope.as_deref() {
                Some(raw) => raw.parse()?,
                None => Scope::User,
            },
        };

        let no_interactive = env
            .get(env_vars::NO_INTERACTIVE)
            .map(truthy)
            .unwrap_or(false);

        Ok(Self {
            limits: file.limits.apply(Limits::default()),
            default_agents: file.default_agents.clone(),
            configured_agents: file.configured_agents.clone(),
            paths,
            scope,
            project_dir: overrides.project_dir.clone(),
            allow_http,
            offline,
            yes: overrides.yes,
            dry_run: overrides.dry_run,
            json: overrides.json,
            verbose: overrides.verbose,
            // `--json` implies a machine is reading, so never prompt there.
            interactive: interactive_tty && !no_interactive && !overrides.json,
            config_file_loaded,
            agent_home: env.get(env_vars::AGENT_HOME).map(PathBuf::from),
        })
    }

    /// Build a configuration for tests, rooted entirely inside `root`.
    pub fn for_test(root: impl AsRef<Path>) -> Self {
        Self {
            paths: Paths::rooted_at(root),
            limits: Limits::default(),
            default_agents: Vec::new(),
            configured_agents: Vec::new(),
            scope: Scope::User,
            project_dir: None,
            allow_http: false,
            offline: false,
            yes: false,
            dry_run: false,
            json: false,
            verbose: false,
            interactive: false,
            config_file_loaded: None,
            agent_home: None,
        }
    }

    /// Apply late flag overrides, after `resolve`.
    pub fn apply_overrides(&mut self, overrides: &Overrides) {
        self.yes = overrides.yes;
        self.dry_run = overrides.dry_run;
        self.json = overrides.json;
        self.verbose = overrides.verbose;
    }

    /// The canonical directory for one package in the store.
    pub fn canonical_path(&self, install_name: &str) -> PathBuf {
        self.paths.store.join(install_name)
    }

    /// Fail when an operation needs the network and offline mode is set.
    pub fn require_network(&self, what: &str) -> Result<()> {
        if self.offline {
            return Err(Error::OfflineRequired {
                what: what.to_string(),
                hint: "drop --offline, or unset SKILL_OFFLINE, to allow this fetch".into(),
            });
        }
        Ok(())
    }
}

/// Expand a leading `~` using the platform home directory.
///
/// Only a leading `~/` is expanded. A `~user` form is not, because resolving
/// another account's home is not something a skill manager should be doing.
pub fn expand_home(raw: &str) -> Result<PathBuf> {
    if raw == "~" || raw.starts_with("~/") || raw.starts_with("~\\") {
        let base = directories::BaseDirs::new()
            .ok_or_else(|| Error::Internal("could not determine the home directory".into()))?;
        let rest = raw.trim_start_matches('~').trim_start_matches(['/', '\\']);
        return Ok(if rest.is_empty() {
            base.home_dir().to_path_buf()
        } else {
            base.home_dir().join(rest)
        });
    }
    Ok(PathBuf::from(raw))
}

/// Read and parse the config file, returning `None` when it does not exist.
fn load_file(path: &Path) -> Result<Option<ConfigFile>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let parsed: ConfigFile = toml::from_str(&text).map_err(|err| {
                // A broken config file is a usage problem, not an internal one,
                // and the message needs to say which file and which key.
                Error::Usage(format!(
                    "could not parse the config file {}: {err}\nhint: see examples/config.toml \
                     for the supported keys",
                    path.display()
                ))
            })?;
            Ok(Some(parsed))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Error::io("reading the config file", path, err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn defaults_put_the_store_in_the_home_directory() {
        let cfg = Config::resolve(&Overrides::default(), &BTreeMap::new(), false).unwrap();
        assert!(cfg.paths.store.ends_with("skills"));
        assert_eq!(cfg.scope, Scope::User);
        assert!(!cfg.allow_http, "plain HTTP must be off by default");
        assert!(!cfg.offline);
    }

    #[test]
    fn state_never_lives_inside_the_store() {
        // A database inside the store would be hashed as package content and
        // then reported as drift on the next sync.
        let cfg = Config::resolve(&Overrides::default(), &BTreeMap::new(), false).unwrap();
        assert!(
            !cfg.paths.database().starts_with(&cfg.paths.store),
            "state must be kept out of skill directories"
        );
        assert!(!cfg.paths.journal_dir().starts_with(&cfg.paths.store));
        assert!(!cfg.paths.backup_dir().starts_with(&cfg.paths.store));
    }

    #[test]
    fn flags_beat_environment_which_beats_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            "store = \"/from/file\"\ndefault_scope = \"project\"\n",
        )
        .unwrap();

        // File only.
        let overrides = Overrides {
            config: Some(config_path.clone()),
            ..Overrides::default()
        };
        let cfg = Config::resolve(&overrides, &BTreeMap::new(), false).unwrap();
        assert_eq!(cfg.paths.store, PathBuf::from("/from/file"));
        assert_eq!(cfg.scope, Scope::Project);

        // Environment beats the file.
        let cfg =
            Config::resolve(&overrides, &env(&[(env_vars::STORE, "/from/env")]), false).unwrap();
        assert_eq!(cfg.paths.store, PathBuf::from("/from/env"));

        // Flag beats both.
        let with_flag = Overrides {
            store: Some(PathBuf::from("/from/flag")),
            ..overrides.clone()
        };
        let cfg =
            Config::resolve(&with_flag, &env(&[(env_vars::STORE, "/from/env")]), false).unwrap();
        assert_eq!(cfg.paths.store, PathBuf::from("/from/flag"));
    }

    #[test]
    fn offline_can_be_set_from_any_layer() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("c.toml");
        std::fs::write(&path, "offline = true\n").unwrap();
        let overrides = Overrides {
            config: Some(path),
            ..Overrides::default()
        };
        assert!(
            Config::resolve(&overrides, &BTreeMap::new(), false)
                .unwrap()
                .offline
        );

        assert!(
            Config::resolve(
                &Overrides::default(),
                &env(&[(env_vars::OFFLINE, "1")]),
                false
            )
            .unwrap()
            .offline
        );
    }

    #[test]
    fn require_network_refuses_when_offline() {
        let mut cfg = Config::for_test("/tmp/x");
        assert!(cfg.require_network("fetching").is_ok());
        cfg.offline = true;
        let err = cfg.require_network("checking upstream").unwrap_err();
        assert_eq!(err.exit_code(), crate::ExitCode::OfflineRequired);
        assert!(err.to_string().contains("--offline"));
    }

    #[test]
    fn json_output_disables_prompting() {
        // A machine is reading, so a prompt would hang the caller.
        let overrides = Overrides {
            json: true,
            ..Overrides::default()
        };
        let cfg = Config::resolve(&overrides, &BTreeMap::new(), true).unwrap();
        assert!(!cfg.interactive);
    }

    #[test]
    fn no_interactive_env_var_disables_prompting() {
        let cfg = Config::resolve(
            &Overrides::default(),
            &env(&[(env_vars::NO_INTERACTIVE, "1")]),
            true,
        )
        .unwrap();
        assert!(!cfg.interactive);
    }

    #[test]
    fn an_unknown_config_key_is_a_usage_error_naming_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("c.toml");
        std::fs::write(&path, "stroe = \"typo\"\n").unwrap();
        let overrides = Overrides {
            config: Some(path.clone()),
            ..Overrides::default()
        };
        let err = Config::resolve(&overrides, &BTreeMap::new(), false).unwrap_err();
        assert_eq!(err.exit_code(), crate::ExitCode::Usage);
        assert!(err.to_string().contains("config.toml") || err.to_string().contains("c.toml"));
    }

    #[test]
    fn a_missing_config_file_is_not_an_error() {
        let overrides = Overrides {
            config: Some(PathBuf::from("/nonexistent/skill/config.toml")),
            ..Overrides::default()
        };
        let cfg = Config::resolve(&overrides, &BTreeMap::new(), false).unwrap();
        assert!(cfg.config_file_loaded.is_none());
    }

    #[test]
    fn limits_are_overridable_from_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("c.toml");
        std::fs::write(&path, "[limits]\nmax_files = 7\n").unwrap();
        let overrides = Overrides {
            config: Some(path),
            ..Overrides::default()
        };
        let cfg = Config::resolve(&overrides, &BTreeMap::new(), false).unwrap();
        assert_eq!(cfg.limits.max_files, 7);
        // Unset keys keep their defaults.
        assert_eq!(cfg.limits.max_depth, Limits::default().max_depth);
    }

    #[test]
    fn ensure_creates_private_state_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(tmp.path());
        paths.ensure().unwrap();

        assert!(paths.store.is_dir());
        assert!(paths.state_dir.is_dir());
        assert!(paths.journal_dir().is_dir());
        assert!(paths.backup_dir().is_dir());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&paths.state_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "state must not be group or world readable");
        }
    }

    #[test]
    fn home_expansion_only_handles_the_leading_tilde() {
        let expanded = expand_home("~/skills").unwrap();
        assert!(expanded.is_absolute());
        assert!(expanded.ends_with("skills"));
        // A `~user` form is deliberately left alone.
        assert_eq!(expand_home("~other/x").unwrap(), PathBuf::from("~other/x"));
        assert_eq!(expand_home("/abs/x").unwrap(), PathBuf::from("/abs/x"));
    }
}
