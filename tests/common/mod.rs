//! Shared harness for the acceptance tests.
//!
//! Every test builds a complete fake machine inside a temporary directory: its
//! own store, state directory, cache, config file, and agent home. The
//! developer's real `HOME` is never rewritten and no real agent installation is
//! read or written. Isolation is injected through `SKILL_*` variables on a
//! tightly scoped child process, never by mutating this process's environment.
//!
//! `dead_code` is allowed because this harness is shared by two test binaries and
//! each one uses a different subset of it; the alternative is duplicating the
//! whole harness per target.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;

/// An isolated machine for one test.
pub struct Sandbox {
    pub root: tempfile::TempDir,
}

impl Sandbox {
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary directory");
        for dir in ["store", "state", "cache", "home", "sources", "project"] {
            std::fs::create_dir_all(root.path().join(dir)).expect("sandbox directory");
        }
        Self { root }
    }

    pub fn path(&self) -> &Path {
        self.root.path()
    }

    /// The fake agent home. Agent skill roots resolve against this.
    pub fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    pub fn store(&self) -> PathBuf {
        self.root.path().join("store")
    }

    pub fn project(&self) -> PathBuf {
        self.root.path().join("project")
    }

    /// Where a given agent's user-scope skills live inside the sandbox.
    pub fn agent_skills(&self, agent: &str) -> PathBuf {
        match agent {
            "claude" => self.home().join(".claude/skills"),
            "codex" => self.home().join(".agents/skills"),
            "gemini" => self.home().join(".gemini/skills"),
            other => panic!("unknown agent {other}"),
        }
    }

    /// Pretend an agent is installed, by placing an executable on a fake PATH.
    pub fn install_agent(&self, agent: &str) -> &Self {
        let bin = self.root.path().join("bin");
        std::fs::create_dir_all(&bin).expect("bin directory");
        let name = if cfg!(windows) {
            format!("{agent}.exe")
        } else {
            agent.to_string()
        };
        let path = bin.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("fake executable");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("executable bit");
        }
        std::fs::create_dir_all(self.agent_skills(agent)).expect("agent skills directory");
        self
    }

    /// Write a minimal valid skill package and return its path.
    pub fn write_skill(&self, name: &str) -> PathBuf {
        let dir = self.root.path().join("sources").join(name);
        self.write_skill_in(&dir, name)
    }

    /// Write a skill package at an explicit location.
    pub fn write_skill_in(&self, dir: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir.join("scripts")).expect("scripts directory");
        std::fs::create_dir_all(dir.join("references")).expect("references directory");
        std::fs::write(
            dir.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: Does {name} things when asked.\n---\n\nBody.\n"
            ),
        )
        .expect("SKILL.md");
        std::fs::write(dir.join("scripts/run.sh"), "#!/bin/sh\necho hi\n").expect("script");
        std::fs::write(dir.join("references/REFERENCE.md"), "reference\n").expect("reference");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.join("scripts/run.sh"),
                std::fs::Permissions::from_mode(0o755),
            )
            .expect("executable bit");
        }
        dir.to_path_buf()
    }

    /// Build a `skill` invocation scoped entirely to this sandbox.
    ///
    /// `env_clear` plus an explicit allowlist means the test cannot be affected
    /// by the developer's own environment, and cannot affect it.
    pub fn cmd(&self) -> Command {
        let mut cmd = Command::cargo_bin("skill").expect("the skill binary");
        cmd.env_clear();
        for key in [
            "PATH",
            "SYSTEMROOT",
            "TMPDIR",
            "TEMP",
            "TMP",
            "USERPROFILE",
            "HOME",
        ] {
            if let Ok(value) = std::env::var(key) {
                cmd.env(key, value);
            }
        }
        cmd.env("SKILL_STORE", self.store())
            .env("SKILL_STATE_DIR", self.root.path().join("state"))
            .env("SKILL_CACHE_DIR", self.root.path().join("cache"))
            .env("SKILL_CONFIG", self.root.path().join("config.toml"))
            .env("SKILL_AGENT_HOME", self.home())
            .env("SKILL_NO_INTERACTIVE", "1");
        cmd
    }

    /// Build an invocation whose PATH contains only the fake agent executables,
    /// so detection sees exactly the agents the test installed.
    pub fn cmd_isolated_path(&self) -> Command {
        let mut cmd = self.cmd();
        cmd.env("PATH", self.root.path().join("bin"));
        cmd
    }
}

/// Run a command and return (exit code, stdout, stderr).
pub fn run(cmd: &mut Command) -> (i32, String, String) {
    let out = cmd.output().expect("the command should run");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Parse the JSON envelope from a `--json` run, asserting its schema.
pub fn envelope(stdout: &str) -> serde_json::Value {
    let value: serde_json::Value = serde_json::from_str(stdout)
        .unwrap_or_else(|err| panic!("stdout is not valid JSON: {err}\n---\n{stdout}\n---"));
    assert_eq!(
        value["schema"], "skill.v1",
        "every payload must name its schema"
    );
    value
}

/// Digest of a deployment, for byte-for-byte comparisons.
pub fn digest(path: &Path) -> Option<String> {
    skill::txn::current_digest(path, &skill::safepath::Limits::default())
        .expect("digest should be computable")
}
