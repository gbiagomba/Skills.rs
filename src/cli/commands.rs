//! Command implementations.
//!
//! Each handler builds a plan, discloses what it is about to do, and only then
//! hands the plan to [`crate::txn`]. The split matters: planning is read-only and
//! can be printed under `--dry-run`, and applying is the only thing that writes.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::render::{table, Output};
use super::{Command, GlobalArgs, ScopeArgs, SelectionArgs};
use crate::agent::{self, registry, Host, Provenance, Scope};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::pkg::frontmatter;
use crate::pkg::identity::{InstallName, PackageId, SourceType};
use crate::pkg::{scan, tree};
use crate::recon::{self, Action, Drift};
use crate::source::{self, locator};
use crate::state::models::*;
use crate::state::Store;
use crate::txn::{self, Transaction};

/// Everything a handler needs.
pub struct Context {
    pub config: Config,
    pub host: Host,
    pub output: Output,
}

impl Context {
    /// Open the state store, creating the directories it needs.
    pub fn store(&self) -> Result<Store> {
        self.config.paths.ensure()?;
        Store::open(&self.config.paths.database())
    }

    /// Take the exclusive lock. Only mutating commands do this.
    pub fn lock(&self) -> Result<txn::Lock> {
        self.config.paths.ensure()?;
        txn::Lock::acquire(&self.config.paths.lock_file())
    }
}

/// Dispatch one parsed command.
pub fn run(ctx: &Context, command: &Command, global: &GlobalArgs) -> Result<()> {
    match command {
        Command::Agents => agents(ctx),
        Command::List { agent } => list(ctx, agent.as_deref()),
        Command::Copy {
            source,
            agents,
            install_as,
            r#ref,
            allow_http,
            selection,
            scope,
        } => install(
            ctx,
            InstallRequest {
                source,
                agents,
                install_as: install_as.as_deref(),
                requested_ref: r#ref.as_deref(),
                allow_http: *allow_http,
                selection,
                scope,
                mode: DeployMode::Copy,
            },
        ),
        Command::Link {
            source,
            agents,
            install_as,
            r#ref,
            allow_http,
            selection,
            scope,
        } => install(
            ctx,
            InstallRequest {
                source,
                agents,
                install_as: install_as.as_deref(),
                requested_ref: r#ref.as_deref(),
                allow_http: *allow_http,
                selection,
                scope,
                mode: DeployMode::Link,
            },
        ),
        Command::Status { skill } => status(ctx, skill.as_deref()),
        Command::Diff { skill } => diff(ctx, skill),
        Command::Sync {
            skills,
            adopt_from,
            all_skills,
        } => sync(ctx, skills, adopt_from.as_deref(), *all_skills),
        Command::Rollback { transaction_id } => rollback(ctx, transaction_id),
        Command::Doctor => doctor(ctx),

        // Specified, designed, and documented, but not implemented in this
        // build. Reported as its own exit code so it can never read as success.
        Command::Migrate { .. } => Err(not_implemented(
            "migrate",
            "the journalled migration pipeline is built (see src/txn) but the command is not \
             wired up yet. Until it is, move a skill by running `skill copy <store-path> <to>` \
             and removing the old deployment yourself",
        )),
        Command::Update { .. } => Err(not_implemented(
            "update",
            "upstream refresh needs the git and http backends, which are not in this build. \
             `skill sync` already reconciles the store with its deployments offline",
        )),
        Command::Export { .. } => Err(not_implemented(
            "export",
            "bundle writing is not in this build; copy the store directory to back it up",
        )),
        Command::Import { .. } => Err(not_implemented(
            "import",
            "bundle restore is not in this build; `skill copy <path>` installs from a directory",
        )),
    }
    .map(|()| {
        let _ = global;
    })
}

fn not_implemented(command: &str, hint: &str) -> Error {
    Error::NotImplemented {
        command: command.to_string(),
        hint: format!("{hint}. See docs/checklist.md for what this build does and does not do"),
    }
}

// ---- agents ------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
struct AgentReport {
    id: &'static str,
    display_name: &'static str,
    aliases: &'static [&'static str],
    installed: bool,
    evidence: Vec<String>,
    user_write_path: Option<PathBuf>,
    shared_with: Vec<String>,
    isolated_user_root: bool,
    symlink_support: agent::SymlinkSupport,
    caveats: Vec<agent::Caveat>,
}

fn agents(ctx: &Context) -> Result<()> {
    let mut reports = Vec::new();
    let mut rows = Vec::new();

    for (adapter, detection) in registry::detect_all(&ctx.host) {
        let capabilities = adapter.capabilities();
        let write_root = adapter.write_root(&ctx.host, Scope::User).ok();

        rows.push(vec![
            adapter.id().to_string(),
            if detection.installed {
                "yes".into()
            } else {
                "no".into()
            },
            write_root
                .as_ref()
                .map(|r| r.path.display().to_string())
                .unwrap_or_else(|| "(none)".into()),
            write_root
                .as_ref()
                .filter(|r| r.is_shared())
                .map(|r| format!("shared with {}", r.shared_with.join(", ")))
                .unwrap_or_else(|| "isolated".into()),
        ]);

        reports.push(AgentReport {
            id: adapter.id(),
            display_name: adapter.display_name(),
            aliases: adapter.aliases(),
            installed: detection.installed,
            evidence: detection.evidence.clone(),
            user_write_path: write_root.as_ref().map(|r| r.path.clone()),
            shared_with: write_root
                .as_ref()
                .map(|r| r.shared_with.iter().map(|s| (*s).to_string()).collect())
                .unwrap_or_default(),
            isolated_user_root: capabilities.isolated_user_root,
            symlink_support: capabilities.symlink,
            caveats: capabilities.caveats,
        });
    }

    ctx.output.line(table(
        &["AGENT", "DETECTED", "USER SKILLS PATH", "VISIBILITY"],
        &rows,
    ));

    for report in &reports {
        if ctx.output.verbose {
            ctx.output
                .line(format!("{} ({})", report.display_name, report.id));
            for line in &report.evidence {
                ctx.output.detail(line);
            }
            for caveat in &report.caveats {
                ctx.output.detail(format!("caveat: {}", caveat.message));
            }
        } else if !report.isolated_user_root {
            // This one is important enough to surface without --verbose.
            ctx.output.warn(format!(
                "{} has no isolated user skills directory, so anything installed there is \
                 visible to {}",
                report.display_name,
                report.shared_with.join(", ")
            ));
        }
    }

    ctx.output.emit("agents", &reports, &[])
}

// ---- list --------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
struct ListEntry {
    name: String,
    source_type: SourceType,
    locator: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    selector: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved_revision: Option<String>,
    pin_policy: PinPolicy,
    deployments: Vec<DeploymentSummary>,
}

#[derive(Debug, serde::Serialize)]
struct DeploymentSummary {
    agent: String,
    scope: Scope,
    path: PathBuf,
    mode: DeployMode,
    present: bool,
}

fn list(ctx: &Context, agent_filter: Option<&str>) -> Result<()> {
    let store = ctx.store()?;
    // Validate the filter up front, so a typo is a clear error rather than an
    // empty result that looks like "nothing installed".
    let filter = match agent_filter {
        Some(name) => Some(registry::resolve(name)?.id()),
        None => None,
    };

    let mut entries = Vec::new();
    let mut rows = Vec::new();

    for package in store.packages()? {
        let deployments: Vec<DeploymentSummary> = store
            .deployments(&package.id)?
            .into_iter()
            .filter(|d| filter.is_none_or(|f| d.agent == f))
            .map(|d| DeploymentSummary {
                present: d.path.exists() || std::fs::symlink_metadata(&d.path).is_ok(),
                agent: d.agent,
                scope: d.scope,
                path: d.path,
                mode: d.mode,
            })
            .collect();

        if filter.is_some() && deployments.is_empty() {
            continue;
        }

        rows.push(vec![
            package.install_name.as_str().to_string(),
            package.source_type.to_string(),
            if deployments.is_empty() {
                "(none)".to_string()
            } else {
                deployments
                    .iter()
                    .map(|d| format!("{}{}", d.agent, if d.present { "" } else { " (missing)" }))
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            package.locator.clone(),
        ]);

        entries.push(ListEntry {
            name: package.install_name.as_str().to_string(),
            source_type: package.source_type,
            locator: package.locator,
            selector: Some(package.selector).filter(|s| !s.is_empty()),
            requested_ref: package.requested_ref,
            resolved_revision: package.resolved_revision,
            pin_policy: package.pin_policy,
            deployments,
        });
    }

    if entries.is_empty() {
        ctx.output.line("No managed skills yet.");
        ctx.output
            .line("Install one with: skill copy <source> <agent>");
    } else {
        ctx.output
            .line(table(&["SKILL", "SOURCE", "DEPLOYED TO", "ORIGIN"], &rows));
    }

    ctx.output.emit("list", &entries, &[])
}

// ---- copy and link ----------------------------------------------------

struct InstallRequest<'a> {
    source: &'a str,
    agents: &'a [String],
    install_as: Option<&'a str>,
    requested_ref: Option<&'a str>,
    allow_http: bool,
    selection: &'a SelectionArgs,
    scope: &'a ScopeArgs,
    mode: DeployMode,
}

#[derive(Debug, serde::Serialize)]
struct InstallReport {
    mode: DeployMode,
    installed: Vec<InstalledPackage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    disclosures: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
struct InstalledPackage {
    name: String,
    canonical: PathBuf,
    digest: String,
    destinations: Vec<txn::Outcome>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    findings: Vec<frontmatter::Finding>,
}

fn install(ctx: &Context, request: InstallRequest<'_>) -> Result<()> {
    let config = &ctx.config;
    let _lock = ctx.lock()?;
    let mut store = ctx.store()?;

    // 1. Parse the source and refuse anything we cannot handle, by name.
    let mut parsed = locator::parse(request.source)?;
    if let Some(explicit) = request.requested_ref {
        parsed.requested_ref = Some(explicit.to_string());
    }

    if let locator::Target::Http(url) = &parsed.target {
        if url.scheme() == "http" && !(request.allow_http || config.allow_http) {
            return Err(Error::InsecureTransport {
                url: url.to_string(),
            });
        }
    }
    if parsed.needs_network() {
        config.require_network(&format!("acquiring {}", parsed.describe()))?;
    }

    let mut disclosures = Vec::new();

    // 2. Acquire. For a filesystem source this reads in place and never writes.
    let backend = source::backend_for(&parsed)?;
    let acquired = backend.acquire(&parsed, &config.limits)?;
    disclosures.extend(acquired.notes.clone());

    // 3. Survey the source for packages.
    let survey = scan::scan(&acquired.root, &config.limits, scan::DEFAULT_SEARCH_DEPTH)?;
    disclosures.extend(survey.notes.clone());

    let chosen: Vec<&scan::Candidate> = if request.selection.all_skills {
        survey.candidates.iter().collect()
    } else if !request.selection.skills.is_empty() {
        survey.select(&request.selection.skills)?
    } else if survey.candidates.len() == 1 {
        survey.candidates.iter().collect()
    } else {
        let mut names: Vec<&str> = survey.candidates.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        return Err(Error::NeedsSelection {
            what: format!("a package selection ({} skills found)", names.len()),
            hint: format!(
                "pass --skill <name> (repeatable) or --all-skills. Available: {}",
                names.join(", ")
            ),
        });
    };

    if request.install_as.is_some() && chosen.len() > 1 {
        return Err(Error::Usage(
            "--as renames a single package, but several were selected\nhint: select one with \
             --skill <name>, or drop --as"
                .to_string(),
        ));
    }

    // 4. Resolve destinations.
    let scope = resolve_scope(config, request.scope)?;
    let destination_agents = resolve_destination_agents(ctx, request.agents, request.selection)?;

    // 5. Validate every package before writing any of them, so a bad package in
    //    a collection does not leave a half-installed set.
    for candidate in &chosen {
        let blocking = frontmatter::blocking_errors(&candidate.findings);
        if !blocking.is_empty() {
            return Err(Error::InvalidPackage {
                name: Some(candidate.name.as_str().to_string()),
                reason: blocking
                    .iter()
                    .map(|f| f.message.clone())
                    .collect::<Vec<_>>()
                    .join("; "),
            });
        }
    }

    let mut installed = Vec::new();

    for candidate in chosen {
        let install_name = match request.install_as {
            Some(alias) => InstallName::parse(alias)?,
            None => candidate.name.clone(),
        };

        let package_id =
            PackageId::derive(parsed.source_type, &parsed.sanitized, &candidate.selector);

        // Refuse to let a second origin take an occupied name.
        if let Some(existing) = store.package_by_install_name(install_name.as_str())? {
            if existing.id != package_id {
                return Err(Error::NameCollision {
                    name: install_name.as_str().to_string(),
                    existing: format!("{} {}", existing.source_type, existing.locator),
                });
            }
        }

        let canonical = config.canonical_path(install_name.as_str());
        let source_tree = tree::build(&candidate.root, &config.limits)?;

        // Report compatibility findings for the agents actually selected.
        for finding in &candidate.findings {
            if let Some(agent_id) = &finding.agent {
                if destination_agents.iter().any(|a| a.id() == agent_id) {
                    ctx.output.warn(format!(
                        "{}: {} ({})",
                        agent_id,
                        finding.message,
                        if finding.confirmed {
                            "documented"
                        } else {
                            "heuristic"
                        }
                    ));
                }
            } else if finding.severity != frontmatter::Severity::Note {
                ctx.output.warn(&finding.message);
            }
        }

        let plan =
            registry::plan_destinations(&destination_agents, &ctx.host, scope, &install_name)?;
        disclosures.extend(plan.disclosures.clone());

        // Warn where an agent cannot actually load what we are about to write.
        for destination in &plan.destinations {
            let adapter = registry::resolve(&destination.agent)?;
            let capabilities = adapter.capabilities();
            if scope == Scope::Project {
                for caveat in &capabilities.caveats {
                    if caveat.code == "gemini.untrusted_workspace" {
                        disclosures.push(caveat.message.clone());
                    }
                }
            }
            if request.mode == DeployMode::Link
                && capabilities.symlink == agent::SymlinkSupport::Unsupported
            {
                return Err(Error::Unsupported {
                    agent: destination.agent.clone(),
                    what: "use a symlinked skill directory".into(),
                    reason: "this agent documents symlinks as unsupported".into(),
                    hint: "use `skill copy` instead of `skill link`".into(),
                });
            }
        }

        // Guard every existing destination before writing any of them.
        //
        // There are two distinct refusals here and conflating them would be a
        // bug. A destination we have never managed must not be overwritten at
        // all. A destination we *do* manage must still not be overwritten when it
        // has been edited since we wrote it, because re-running `copy` is not an
        // instruction to discard local work.
        for destination in &plan.destinations {
            if std::fs::symlink_metadata(&destination.path).is_err() {
                continue;
            }

            let Some(known) = store.deployment_at(&destination.path)? else {
                let provenance =
                    registry::classify_for(&destination.agent, &ctx.host, &destination.path)?;
                return Err(Error::Refused {
                    path: destination.path.clone(),
                    kind: provenance.to_string(),
                    hint: format!("{}. Nothing was written", provenance.refusal_hint()),
                });
            };

            // It is ours. Has it drifted since we last wrote it?
            let current = txn::current_digest(&destination.path, &config.limits)?;
            let baseline = match known.baseline_snapshot_id {
                Some(id) => store.snapshot(id)?.map(|s| s.digest),
                None => None,
            };

            let drifted = match (&baseline, &current) {
                // A recorded copy whose content no longer matches what we wrote.
                (Some(baseline), Some(current)) => baseline != current,
                // A link: drift means it no longer points at canonical content.
                (None, Some(_)) if known.mode == DeployMode::Link => {
                    std::fs::read_link(&destination.path)
                        .map(|target| target != canonical)
                        .unwrap_or(true)
                }
                _ => false,
            };

            if drifted {
                return Err(Error::Conflict {
                    count: 1,
                    hint: format!(
                        "{} has changed since skill last wrote it, so re-installing would                          discard those changes. Nothing was written.\n                         Inspect them with `skill diff {}`, keep them with                          `skill sync {} --adopt-from {}`, or delete the deployment yourself to                          discard them",
                        destination.path.display(),
                        install_name,
                        install_name,
                        destination.agent
                    ),
                });
            }
        }

        // 6. Stage canonical content, then deploy.
        let mut transaction = Transaction::begin(
            config,
            &store,
            if request.mode == DeployMode::Copy {
                "copy"
            } else {
                "link"
            },
        )?;

        let mut targets = vec![txn::Target {
            path: canonical.clone(),
            mode: DeployMode::Copy,
            source: candidate.root.clone(),
            containment_root: config.paths.store.clone(),
            expected_fingerprint: txn::current_digest(&canonical, &config.limits)?,
        }];

        for destination in &plan.destinations {
            targets.push(txn::Target {
                path: destination.path.clone(),
                mode: request.mode,
                source: canonical.clone(),
                containment_root: destination.root.clone(),
                expected_fingerprint: txn::current_digest(&destination.path, &config.limits)?,
            });
        }

        match transaction.apply(&store, &targets) {
            Ok(()) => {}
            Err(err) => {
                transaction.fail(&store)?;
                return Err(err);
            }
        }

        let destinations: Vec<txn::Outcome> = transaction.outcomes()[1..].to_vec();

        if !config.dry_run {
            let record = PackageRecord {
                id: package_id.clone(),
                install_name: install_name.clone(),
                display_name: candidate.declared_name.clone(),
                source_type: parsed.source_type,
                locator: parsed.sanitized.clone(),
                selector: candidate.selector.clone(),
                requested_ref: parsed.requested_ref.clone(),
                resolved_revision: acquired.resolved_revision.clone(),
                // A resolved immutable revision is a pin; a branch or tag tracks.
                pin_policy: if parsed.requested_ref.is_none()
                    && acquired.resolved_revision.is_some()
                {
                    PinPolicy::Pinned
                } else {
                    PinPolicy::Tracking
                },
                // A locally acquired source is trusted; a bundle is not.
                trusted_origin: parsed.source_type != SourceType::Bundle,
                acquired_at: crate::state::now(),
                last_checked_at: None,
            };
            store.upsert_package(&record)?;

            let canonical_tree = tree::build(&canonical, &config.limits)?;
            store.record_snapshot(&package_id, SnapshotKind::Canonical, &canonical_tree)?;
            // The pristine upstream baseline is what `update` compares against.
            store.record_snapshot(&package_id, SnapshotKind::UpstreamPristine, &source_tree)?;

            for (destination, outcome) in plan.destinations.iter().zip(&destinations) {
                let baseline = match request.mode {
                    DeployMode::Copy => {
                        let deployed = tree::build(&destination.path, &config.limits)?;
                        Some(store.record_snapshot(
                            &package_id,
                            SnapshotKind::DeploymentBaseline,
                            &deployed,
                        )?)
                    }
                    // A link has no independent content, so it has no baseline of
                    // its own: edits through it are canonical edits.
                    DeployMode::Link => None,
                };
                store.upsert_deployment(
                    &package_id,
                    &destination.agent,
                    scope,
                    &destination.path,
                    request.mode,
                    baseline,
                )?;
                let _ = outcome;
            }
        }

        transaction.commit(&store)?;

        ctx.output.line(format!(
            "{} {} -> {}",
            if config.dry_run {
                "would install"
            } else {
                "installed"
            },
            install_name,
            plan.destinations
                .iter()
                .map(|d| format!("{} ({})", d.agent, d.path.display()))
                .collect::<Vec<_>>()
                .join(", ")
        ));

        installed.push(InstalledPackage {
            name: install_name.as_str().to_string(),
            canonical,
            digest: source_tree.digest.clone(),
            destinations,
            findings: candidate.findings.clone(),
        });
    }

    disclosures.sort();
    disclosures.dedup();
    for disclosure in &disclosures {
        ctx.output.warn(disclosure);
    }

    ctx.output.emit(
        if request.mode == DeployMode::Copy {
            "copy"
        } else {
            "link"
        },
        InstallReport {
            mode: request.mode,
            installed,
            disclosures: disclosures.clone(),
        },
        &disclosures,
    )
}

/// Resolve the scope, validating that a project scope has a directory.
fn resolve_scope(config: &Config, args: &ScopeArgs) -> Result<Scope> {
    let scope: Scope = match &args.scope {
        Some(raw) => raw.parse()?,
        None => config.scope,
    };
    if scope == Scope::Project && args.project_dir.is_none() && config.project_dir.is_none() {
        return Err(Error::NeedsSelection {
            what: "a project directory".to_string(),
            hint: "pass --project-dir <DIR> alongside --scope project".to_string(),
        });
    }
    Ok(scope)
}

/// Work out which agents to deploy to.
///
/// With no agents named and no terminal, this fails with instructions rather than
/// guessing, unless a configured default exists.
fn resolve_destination_agents(
    ctx: &Context,
    named: &[String],
    selection: &SelectionArgs,
) -> Result<Vec<&'static dyn agent::Agent>> {
    if selection.all_detected {
        let detected = registry::detected(&ctx.host);
        if detected.is_empty() {
            return Err(Error::AgentNotDetected {
                agent: "any".to_string(),
                evidence: "no supported agent executable was found on PATH".to_string(),
                hint: "name a destination explicitly, for example `skill copy <source> claude`"
                    .to_string(),
            });
        }
        return Ok(detected);
    }

    if !named.is_empty() {
        return registry::resolve_many(named);
    }

    if !ctx.config.default_agents.is_empty() {
        return registry::resolve_many(&ctx.config.default_agents);
    }

    if ctx.config.interactive {
        return prompt_for_agents(ctx);
    }

    let detected = registry::detected(&ctx.host);
    let available = if detected.is_empty() {
        registry::known_ids()
    } else {
        detected
            .iter()
            .map(|a| a.id())
            .collect::<Vec<_>>()
            .join(", ")
    };
    Err(Error::NeedsSelection {
        what: "at least one destination agent".to_string(),
        hint: format!(
            "name one or more agents, or pass --all-detected, or set default_agents in the \
             config file. Detected here: {available}"
        ),
    })
}

/// Offer the detected agents, showing the exact path each would write.
fn prompt_for_agents(ctx: &Context) -> Result<Vec<&'static dyn agent::Agent>> {
    let detected = registry::detected(&ctx.host);
    if detected.is_empty() {
        return Err(Error::AgentNotDetected {
            agent: "any".to_string(),
            evidence: "no supported agent executable was found on PATH".to_string(),
            hint: "name a destination explicitly".to_string(),
        });
    }

    // The exact path is part of the label: an operator choosing a destination
    // needs to see that a Codex install lands in the shared directory.
    let labels: Vec<String> = detected
        .iter()
        .map(|adapter| {
            let root = adapter.write_root(&ctx.host, ctx.config.scope).ok();
            match root {
                Some(root) if root.is_shared() => format!(
                    "{} -> {} (also read by {})",
                    adapter.id(),
                    root.path.display(),
                    root.shared_with.join(", ")
                ),
                Some(root) => format!("{} -> {}", adapter.id(), root.path.display()),
                None => format!("{} (no writable location)", adapter.id()),
            }
        })
        .collect();

    let picked = inquire::MultiSelect::new("Install to which agents?", labels.clone())
        .prompt()
        .map_err(|err| Error::Usage(format!("no destination was chosen: {err}")))?;

    let chosen: Vec<&'static dyn agent::Agent> = detected
        .into_iter()
        .enumerate()
        .filter(|(index, _)| picked.contains(&labels[*index]))
        .map(|(_, adapter)| adapter)
        .collect();

    if chosen.is_empty() {
        return Err(Error::NeedsSelection {
            what: "at least one destination agent".to_string(),
            hint: "select one or more agents, or pass them on the command line".to_string(),
        });
    }
    Ok(chosen)
}

// ---- status ------------------------------------------------------------

fn status(ctx: &Context, only: Option<&str>) -> Result<()> {
    let store = ctx.store()?;
    let plan = build_sync_plan(
        ctx,
        &store,
        only.map(str::to_string).into_iter().collect(),
        false,
    )?;

    let rows: Vec<Vec<String>> = plan
        .items
        .iter()
        .map(|item| {
            vec![
                item.package.clone(),
                item.agent.clone().unwrap_or_else(|| "(store)".into()),
                item.drift.describe().to_string(),
                item.provenance.to_string(),
                item.path.display().to_string(),
            ]
        })
        .collect();

    if rows.is_empty() {
        ctx.output.line("Nothing is managed yet.");
    } else {
        ctx.output
            .line(table(&["SKILL", "AGENT", "STATE", "OWNER", "PATH"], &rows));
    }

    for item in &plan.items {
        if let Some(detail) = &item.detail {
            ctx.output.detail(format!("{}: {detail}", item.package));
        }
    }

    // Never imply upstream was checked: status is local only.
    let mut notes = plan.disclosures.clone();
    notes.push(
        "status compares the store with its deployments only. It does not contact upstream, so \
         it cannot say whether a newer version exists."
            .to_string(),
    );

    ctx.output.emit("status", &plan, &notes)
}

// ---- diff --------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
struct DiffReport {
    skill: String,
    canonical: PathBuf,
    deployments: Vec<DiffEntry>,
}

#[derive(Debug, serde::Serialize)]
struct DiffEntry {
    agent: String,
    path: PathBuf,
    drift: Drift,
    changed: Vec<String>,
    added: Vec<String>,
    deleted: Vec<String>,
}

fn diff(ctx: &Context, skill: &str) -> Result<()> {
    let store = ctx.store()?;
    let package = store.require_package(skill)?;
    let canonical = ctx.config.canonical_path(package.install_name.as_str());

    let canonical_tree = match txn::deployed_tree(&canonical, &ctx.config.limits)? {
        Some(tree) => tree,
        None => {
            return Err(Error::Usage(format!(
                "the canonical package {} is missing from the store\nhint: run `skill doctor`",
                canonical.display()
            )))
        }
    };

    let mut entries = Vec::new();

    for deployment in store.deployments(&package.id)? {
        let deployed = txn::deployed_tree(&deployment.path, &ctx.config.limits)?;
        let baseline = match deployment.baseline_snapshot_id {
            Some(id) => store.snapshot(id)?.map(|s| s.digest),
            None => None,
        };
        let current_digest = txn::current_digest(&deployment.path, &ctx.config.limits)?;
        let drift = recon::compare(
            baseline.as_deref(),
            &canonical_tree.digest,
            current_digest.as_deref(),
        );

        let delta = match &deployed {
            Some(deployed) => recon::delta(&canonical_tree, deployed),
            None => recon::FileDelta::default(),
        };

        ctx.output.line(format!(
            "{} at {} ({})",
            deployment.agent,
            deployment.path.display(),
            drift.describe()
        ));

        if ctx.output.verbose {
            for path in &delta.changed {
                let left = std::fs::read_to_string(canonical.join(path)).ok();
                let right = std::fs::read_to_string(deployment.path.join(path)).ok();
                match (&left, &right) {
                    (Some(_), Some(_)) | (Some(_), None) | (None, Some(_)) => {
                        ctx.output.line(recon::unified_diff(
                            &format!("store/{path}"),
                            &format!("{}/{path}", deployment.agent),
                            left.as_deref(),
                            right.as_deref(),
                        ));
                    }
                    (None, None) => ctx
                        .output
                        .line(format!("{path}: differs (binary or unreadable)")),
                }
            }
        } else {
            for path in &delta.changed {
                ctx.output.line(format!("  M {path}"));
            }
            for path in &delta.added {
                ctx.output.line(format!("  + {path}"));
            }
            for path in &delta.deleted {
                ctx.output.line(format!("  - {path}"));
            }
        }

        entries.push(DiffEntry {
            agent: deployment.agent,
            path: deployment.path,
            drift,
            changed: delta.changed,
            added: delta.added,
            deleted: delta.deleted,
        });
    }

    if entries.is_empty() {
        ctx.output
            .line(format!("{skill} has no recorded deployments."));
    }

    ctx.output.emit(
        "diff",
        DiffReport {
            skill: package.install_name.as_str().to_string(),
            canonical,
            deployments: entries,
        },
        &[],
    )
}

// ---- sync --------------------------------------------------------------

/// Build the reconciliation plan `status` reports and `sync` carries out.
fn build_sync_plan(
    ctx: &Context,
    store: &Store,
    only: Vec<String>,
    _all: bool,
) -> Result<recon::ReconPlan> {
    let mut plan = recon::ReconPlan::default();

    let packages = if only.is_empty() {
        store.packages()?
    } else {
        only.iter()
            .map(|name| store.require_package(name))
            .collect::<Result<Vec<_>>>()?
    };

    for package in packages {
        let canonical = ctx.config.canonical_path(package.install_name.as_str());
        let canonical_digest = txn::current_digest(&canonical, &ctx.config.limits)?;

        // The store copy going missing is itself a problem worth reporting.
        if canonical_digest.is_none() {
            plan.items.push(recon::Item {
                package: package.install_name.as_str().to_string(),
                agent: None,
                path: canonical.clone(),
                drift: Drift::TargetMissing,
                action: Action::Report,
                provenance: Provenance::ManagedByUs,
                changed_files: vec![],
                added_files: vec![],
                deleted_files: vec![],
                detail: Some(
                    "the canonical package is missing from the store, so its deployments cannot \
                     be reconciled"
                        .to_string(),
                ),
            });
            continue;
        }
        let canonical_digest = canonical_digest.unwrap_or_default();

        for deployment in store.deployments(&package.id)? {
            let current = txn::current_digest(&deployment.path, &ctx.config.limits)?;

            // A link has no independent content: edits through it are canonical
            // edits, so the comparison is link validity, not content drift.
            if deployment.mode == DeployMode::Link {
                let (drift, detail) = match std::fs::read_link(&deployment.path) {
                    Ok(target) if target == canonical => (Drift::Unchanged, None),
                    Ok(target) => (
                        Drift::TargetDrifted,
                        Some(format!(
                            "the link points at {} instead of the canonical package",
                            target.display()
                        )),
                    ),
                    Err(_) => (
                        Drift::TargetMissing,
                        Some("the link is missing or broken".to_string()),
                    ),
                };
                let (action, _) = recon::decide(drift, Provenance::ManagedByUs, current.is_some());
                plan.items.push(recon::Item {
                    package: package.install_name.as_str().to_string(),
                    agent: Some(deployment.agent.clone()),
                    path: deployment.path.clone(),
                    drift,
                    action,
                    provenance: Provenance::ManagedByUs,
                    changed_files: vec![],
                    added_files: vec![],
                    deleted_files: vec![],
                    detail,
                });
                continue;
            }

            let baseline = match deployment.baseline_snapshot_id {
                Some(id) => store.snapshot(id)?.map(|s| s.digest),
                None => None,
            };
            let drift = recon::compare(baseline.as_deref(), &canonical_digest, current.as_deref());
            let (action, detail) = recon::decide(drift, Provenance::ManagedByUs, current.is_some());

            let delta = match (
                txn::deployed_tree(&canonical, &ctx.config.limits)?,
                txn::deployed_tree(&deployment.path, &ctx.config.limits)?,
            ) {
                (Some(left), Some(right)) => recon::delta(&left, &right),
                _ => recon::FileDelta::default(),
            };

            plan.items.push(recon::Item {
                package: package.install_name.as_str().to_string(),
                agent: Some(deployment.agent.clone()),
                path: deployment.path.clone(),
                drift,
                action,
                provenance: Provenance::ManagedByUs,
                changed_files: delta.changed,
                added_files: delta.added,
                deleted_files: delta.deleted,
                detail,
            });
        }
    }

    Ok(plan)
}

fn sync(
    ctx: &Context,
    skills: &[String],
    adopt_from: Option<&str>,
    all_skills: bool,
) -> Result<()> {
    if let Some(agent_id) = adopt_from {
        // Validate early so a typo does not look like "nothing to adopt".
        registry::resolve(agent_id)?;
        return Err(not_implemented(
            "sync --adopt-from",
            "promoting a destination into the store is designed (see the recon truth table) but \
             not wired up in this build",
        ));
    }

    let _lock = ctx.lock()?;
    let store = ctx.store()?;
    let plan = build_sync_plan(ctx, &store, skills.to_vec(), all_skills)?;

    // sync reconciles locally and never fetches, so say so rather than letting a
    // reader assume it checked upstream.
    let mut notes = plan.disclosures.clone();
    notes.push(
        "sync reconciles the store with its deployments. It does not contact upstream; use \
         `skill update` for that."
            .to_string(),
    );

    if plan.is_noop() {
        ctx.output.line("Everything is already in sync.");
        return ctx.output.emit("sync", &plan, &notes);
    }

    for item in plan.blocked() {
        ctx.output.warn(format!(
            "{} at {}: {}{}",
            item.package,
            item.path.display(),
            item.drift.describe(),
            item.detail
                .as_ref()
                .map(|d| format!(" ({d})"))
                .unwrap_or_default()
        ));
    }

    let writable: Vec<&recon::Item> = plan.mutating().collect();
    if writable.is_empty() {
        ctx.output
            .line("Nothing can be changed without an explicit decision.");
        ctx.output.emit("sync", &plan, &notes)?;
        // A conflict is an actionable failure, not a steady state, so it gets the
        // documented conflict code. Plain drift is different: preserving a local
        // edit is sync working as designed, so that stays a success with a
        // warning rather than a non-zero exit a caller would have to special-case.
        return plan.reject_if_conflicted();
    }

    let mut transaction = Transaction::begin(&ctx.config, &store, "sync")?;
    let mut targets = Vec::new();
    let mut deployments = BTreeMap::new();

    for item in &writable {
        let Some(agent_id) = &item.agent else {
            continue;
        };
        let package = store.require_package(&item.package)?;
        let canonical = ctx.config.canonical_path(package.install_name.as_str());
        let deployment = store
            .deployment_at(&item.path)?
            .ok_or_else(|| Error::Internal(format!("{} is not tracked", item.path.display())))?;

        targets.push(txn::Target {
            path: item.path.clone(),
            mode: deployment.mode,
            source: canonical,
            containment_root: item
                .path
                .parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| ctx.config.paths.store.clone()),
            expected_fingerprint: txn::current_digest(&item.path, &ctx.config.limits)?,
        });
        deployments.insert(
            item.path.clone(),
            (package.id.clone(), agent_id.clone(), deployment),
        );
    }

    match transaction.apply(&store, &targets) {
        Ok(()) => {}
        Err(err) => {
            transaction.fail(&store)?;
            return Err(err);
        }
    }

    if !ctx.config.dry_run {
        let mut store = store;
        for outcome in transaction.outcomes() {
            if !outcome.applied {
                continue;
            }
            let Some((package_id, agent_id, deployment)) = deployments.get(&outcome.path) else {
                continue;
            };
            let deployed = tree::build(&outcome.path, &ctx.config.limits)?;
            let baseline =
                store.record_snapshot(package_id, SnapshotKind::DeploymentBaseline, &deployed)?;
            store.upsert_deployment(
                package_id,
                agent_id,
                deployment.scope,
                &outcome.path,
                deployment.mode,
                Some(baseline),
            )?;
        }
        transaction.commit(&store)?;
    } else {
        transaction.commit(&store)?;
    }

    for item in &writable {
        ctx.output.line(format!(
            "{} {} at {}",
            if ctx.config.dry_run {
                "would update"
            } else {
                "updated"
            },
            item.package,
            item.path.display()
        ));
    }

    ctx.output.emit("sync", &plan, &notes)?;
    // Some targets were updated and some conflict. Report the conflict so a
    // caller is never told everything reconciled when it did not.
    plan.reject_if_conflicted()
}

// ---- rollback ----------------------------------------------------------

fn rollback(ctx: &Context, transaction_id: &str) -> Result<()> {
    let _lock = ctx.lock()?;
    let store = ctx.store()?;
    let outcomes = txn::rollback(&ctx.config, &store, transaction_id)?;

    for outcome in &outcomes {
        ctx.output.line(format!(
            "{} {}",
            if outcome.applied {
                "restored"
            } else {
                "would restore"
            },
            outcome.path.display()
        ));
    }

    ctx.output.emit("rollback", &outcomes, &[])
}

// ---- doctor ------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
struct DoctorReport {
    store: PathBuf,
    store_exists: bool,
    database: PathBuf,
    schema_version: u32,
    config_file: Option<PathBuf>,
    managed_packages: usize,
    managed_deployments: usize,
    unfinished_transactions: Vec<TxnRecord>,
    problems: Vec<String>,
    agents_detected: Vec<String>,
}

fn doctor(ctx: &Context) -> Result<()> {
    let store = ctx.store()?;
    let mut problems = Vec::new();

    let packages = store.packages()?;
    let deployments = store.all_deployments()?;

    for deployment in &deployments {
        match std::fs::symlink_metadata(&deployment.path) {
            Err(_) => problems.push(format!(
                "deployment for {} is missing from {}",
                deployment.agent,
                deployment.path.display()
            )),
            Ok(meta) if meta.file_type().is_symlink() => {
                // A link whose target is gone loads nothing, so it is a problem
                // even though the link itself exists.
                if std::fs::metadata(&deployment.path).is_err() {
                    problems.push(format!("{} is a broken link", deployment.path.display()));
                }
            }
            Ok(_) => {}
        }
    }

    for package in &packages {
        let canonical = ctx.config.canonical_path(package.install_name.as_str());
        if !canonical.exists() {
            problems.push(format!(
                "canonical package {} is missing from the store",
                canonical.display()
            ));
        }
    }

    let unfinished = store.unfinished_txns()?;
    for record in &unfinished {
        problems.push(format!(
            "transaction {} ({}) did not finish; undo it with `skill rollback {}`",
            record.id, record.command, record.id
        ));
    }

    // Journals with no matching transaction row mean the database and the
    // filesystem disagree, which is worth surfacing explicitly.
    for journal_path in txn::journal::list(&ctx.config.paths.journal_dir())? {
        if let Some(recovery) = txn::journal::analyse(&txn::journal::read(&journal_path)?) {
            if recovery.needs_recovery() && store.txn(&recovery.txn_id)?.is_none() {
                problems.push(format!(
                    "journal {} records an unfinished run with no database record",
                    journal_path.display()
                ));
            }
        }
    }

    let agents_detected: Vec<String> = registry::detected(&ctx.host)
        .iter()
        .map(|a| a.id().to_string())
        .collect();

    let report = DoctorReport {
        store: ctx.config.paths.store.clone(),
        store_exists: ctx.config.paths.store.is_dir(),
        database: ctx.config.paths.database(),
        schema_version: crate::state::schema::SUPPORTED_VERSION,
        config_file: ctx.config.config_file_loaded.clone(),
        managed_packages: packages.len(),
        managed_deployments: deployments.len(),
        unfinished_transactions: unfinished,
        problems: problems.clone(),
        agents_detected: agents_detected.clone(),
    };

    ctx.output
        .line(format!("store            {}", report.store.display()));
    ctx.output
        .line(format!("database         {}", report.database.display()));
    ctx.output
        .line(format!("schema version   {}", report.schema_version));
    ctx.output.line(format!(
        "config file      {}",
        report
            .config_file
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none loaded)".into())
    ));
    ctx.output
        .line(format!("managed skills   {}", report.managed_packages));
    ctx.output
        .line(format!("deployments      {}", report.managed_deployments));
    ctx.output.line(format!(
        "agents detected  {}",
        if agents_detected.is_empty() {
            "(none)".to_string()
        } else {
            agents_detected.join(", ")
        }
    ));

    if problems.is_empty() {
        ctx.output.line("\nNo problems found.");
    } else {
        ctx.output.line(format!("\n{} problem(s):", problems.len()));
        for problem in &problems {
            ctx.output.line(format!("  - {problem}"));
        }
    }

    ctx.output.emit("doctor", report, &[])
}
