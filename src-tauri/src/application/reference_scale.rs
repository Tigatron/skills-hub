//! Committed reference-scale fixture generator and performance measurement helpers (M0-016).
//!
//! Generates a disposable Vault at PRD reference scale:
//! - 1,000 Vault Skills
//! - 5,000 observations (mix of external + managed-linked locations)
//! - 200 projects across several Workspace Roots
//! - 20 targets suitable for a multi-target plan
//!
//! Fixtures are index-oriented: working Bundle content is minimal `SKILL.md` files so
//! Library search, plan generation (with pre-hashed digests), and Workspace inventory
//! stay measurable without mutating product semantics.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use serde::Serialize;
use tempfile::TempDir;

use crate::{
    domain::{
        AdapterId, BundleDigest, BundleRelativePath, DeploymentHealth, DeploymentId,
        DeploymentMode, DeploymentName, ObservationId, ProjectId, SkillId, SkillLifecycle,
        TargetId, UtcTimestamp, WorkspaceRootId, normalized_path_identity,
    },
    persistence::{
        DeploymentRecord, ObservationRecord, OpenVault, ProjectRecord, SkillRecord, TargetRecord,
        WorkspaceRootRecord,
    },
};

/// PRD §17.1 reference-scale dimensions.
pub const REFERENCE_VAULT_SKILLS: usize = 1_000;
pub const REFERENCE_OBSERVATIONS: usize = 5_000;
pub const REFERENCE_PROJECTS: usize = 200;
pub const REFERENCE_WORKSPACE_ROOTS: usize = 4;
pub const REFERENCE_TARGETS: usize = 20;

/// Default sample count for percentile evidence (a single best run is not evidence).
pub const DEFAULT_PERF_SAMPLES: usize = 11;

#[derive(Debug, Clone, Copy)]
pub struct ReferenceScaleSpec {
    pub vault_skills: usize,
    pub observations: usize,
    pub projects: usize,
    pub workspace_roots: usize,
    pub targets: usize,
}

impl Default for ReferenceScaleSpec {
    fn default() -> Self {
        Self {
            vault_skills: REFERENCE_VAULT_SKILLS,
            observations: REFERENCE_OBSERVATIONS,
            projects: REFERENCE_PROJECTS,
            workspace_roots: REFERENCE_WORKSPACE_ROOTS,
            targets: REFERENCE_TARGETS,
        }
    }
}

/// Owned disposable layout plus open Vault used by the performance harness.
#[allow(dead_code)] // Fields are part of the committed fixture surface for harness consumers.
pub struct ReferenceScaleFixture {
    _temporary: TempDir,
    pub root: PathBuf,
    pub home: PathBuf,
    pub vault: Arc<OpenVault>,
    pub skill_ids: Vec<SkillId>,
    pub target_ids: Vec<TargetId>,
    pub workspace_root_ids: Vec<WorkspaceRootId>,
    pub external_root: PathBuf,
    pub global_skills_root: PathBuf,
    pub workspace_roots: Vec<PathBuf>,
    pub target_roots: Vec<PathBuf>,
    pub generation_elapsed: Duration,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PercentileReport {
    pub name: String,
    pub samples_ms: Vec<f64>,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
    pub gate_ms: f64,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceEvidence {
    pub hardware: String,
    pub os: String,
    pub build: String,
    pub dataset: String,
    pub sample_count: usize,
    pub measurements: Vec<PercentileReport>,
}

impl ReferenceScaleFixture {
    /// Builds a full reference-scale Vault under a temporary directory.
    ///
    /// # Errors
    ///
    /// Returns filesystem or repository errors when the disposable layout cannot be created.
    #[allow(clippy::too_many_lines)]
    pub fn generate(spec: ReferenceScaleSpec) -> Result<Self, String> {
        let started = Instant::now();
        let temporary = tempfile::tempdir().map_err(err)?;
        let root = temporary.path().to_path_buf();
        let home = root.join("home");
        let vault_root = root.join("vault");
        let support = root.join("support");
        let external_root = root.join("external");
        let global_skills_root = home.join(".agents").join("skills");
        fs::create_dir_all(&home).map_err(err)?;
        fs::create_dir_all(&global_skills_root).map_err(err)?;
        fs::create_dir_all(&external_root).map_err(err)?;

        let vault = Arc::new(
            OpenVault::open(&vault_root, &support, std::slice::from_ref(&external_root))
                .map_err(|error| error.to_string())?,
        );
        let now = UtcTimestamp::now();
        let adapter = AdapterId::new("universal-agent-skills", 1).map_err(err)?;

        let mut skill_ids = Vec::with_capacity(spec.vault_skills);
        for index in 0..spec.vault_skills {
            let skill_id = SkillId::generate();
            let name = format!("vault-skill-{index:04}");
            let deployment_name = DeploymentName::parse(&name).map_err(err)?;
            let digest = synthetic_digest(index as u64);
            let relative = format!("skills/{skill_id}/{name}");
            let working = vault.paths.root().join(&relative);
            fs::create_dir_all(&working).map_err(err)?;
            fs::write(
                working.join("SKILL.md"),
                format!("# {name}\n\nReference-scale fixture body {index}.\n"),
            )
            .map_err(err)?;
            vault
                .repositories
                .upsert_skill(SkillRecord {
                    id: skill_id,
                    display_name: name,
                    deployment_name,
                    working_path: BundleRelativePath::parse(&relative).map_err(err)?,
                    working_digest: digest,
                    baseline_digest: digest,
                    lifecycle: SkillLifecycle::Active,
                    created_at: now,
                    updated_at: now,
                })
                .map_err(err)?;
            skill_ids.push(skill_id);
        }

        let mut workspace_roots = Vec::with_capacity(spec.workspace_roots);
        let mut workspace_root_ids = Vec::with_capacity(spec.workspace_roots);
        for index in 0..spec.workspace_roots {
            let path = root.join(format!("workspace-root-{index}"));
            fs::create_dir_all(&path).map_err(err)?;
            let id = WorkspaceRootId::generate();
            vault
                .repositories
                .upsert_workspace_root(WorkspaceRootRecord {
                    id,
                    selected_path: path.clone(),
                    canonical_path: path.canonicalize().unwrap_or_else(|_| path.clone()),
                    paused: false,
                    maximum_depth: 8,
                    ignore_rules: serde_json::json!(["node_modules", "target", ".git"]),
                    scan_status: "idle".into(),
                    created_at: now,
                    updated_at: now,
                })
                .map_err(err)?;
            workspace_root_ids.push(id);
            workspace_roots.push(path);
        }

        let mut project_ids = Vec::with_capacity(spec.projects);
        for index in 0..spec.projects {
            let root_index = index % spec.workspace_roots;
            let project_path = workspace_roots[root_index].join(format!("project-{index:03}"));
            fs::create_dir_all(project_path.join(".git")).map_err(err)?;
            fs::create_dir_all(project_path.join(".agents").join("skills")).map_err(err)?;
            let project_id = ProjectId::generate();
            vault
                .repositories
                .upsert_project(ProjectRecord {
                    id: project_id,
                    workspace_root_id: Some(workspace_root_ids[root_index]),
                    root_path: project_path.clone(),
                    canonical_path: project_path
                        .canonicalize()
                        .unwrap_or_else(|_| project_path.clone()),
                    discovery_evidence: "fixture-git".into(),
                    git_classification: "nested_git".into(),
                    manual: false,
                    created_at: now,
                    updated_at: now,
                })
                .map_err(err)?;
            project_ids.push(project_id);
        }

        let mut target_ids = Vec::with_capacity(spec.targets);
        let mut target_roots = Vec::with_capacity(spec.targets);
        for index in 0..spec.targets {
            let path = root.join(format!("target-{index:02}"));
            fs::create_dir_all(&path).map_err(err)?;
            let target_id = TargetId::generate();
            let project_id = if index < project_ids.len() {
                Some(project_ids[index])
            } else {
                None
            };
            let scope = if project_id.is_some() {
                "project"
            } else {
                "global"
            };
            vault
                .repositories
                .upsert_target(TargetRecord {
                    id: target_id,
                    adapter_id: adapter.clone(),
                    scope: scope.into(),
                    root_path: path.clone(),
                    canonical_root_path: path.canonicalize().unwrap_or_else(|_| path.clone()),
                    project_id,
                    is_override: false,
                    is_custom: true,
                    created_at: now,
                    updated_at: now,
                })
                .map_err(err)?;
            target_ids.push(target_id);
            target_roots.push(path);
        }

        // Seed deployments so managed Library rows and deployment inventory reflect scale.
        // UNIQUE(target_id, normalized_deployment_name) → at most one Skill name per Target.
        // Fill targets round-robin: skill S on target T for T in 0..targets until 4_000 rows.
        let deployment_count = 4_000.min(spec.vault_skills * spec.targets);
        for index in 0..deployment_count {
            let skill_index = index % skill_ids.len();
            let target_index = (index / skill_ids.len()) % target_ids.len();
            let skill_id = skill_ids[skill_index];
            let target_id = target_ids[target_index];
            let target_root = &target_roots[target_index];
            let name = format!("vault-skill-{skill_index:04}");
            let deployment_name = DeploymentName::parse(&name).map_err(err)?;
            let target_path = target_root.join(&name);
            let digest = synthetic_digest(skill_index as u64);
            vault
                .repositories
                .upsert_deployment(DeploymentRecord {
                    id: DeploymentId::generate(),
                    skill_id,
                    target_id,
                    deployment_name,
                    target_path: target_path.clone(),
                    mode: if index % 2 == 0 {
                        DeploymentMode::Symlink
                    } else {
                        DeploymentMode::ManagedCopy
                    },
                    expected_digest: digest,
                    expected_link_target: if index % 2 == 0 {
                        Some(vault.paths.root().join(format!("skills/{skill_id}/{name}")))
                    } else {
                        None
                    },
                    health: DeploymentHealth::Clean,
                    adapter_version: adapter.clone(),
                    active: true,
                    last_verified_at: Some(now),
                    last_operation_id: None,
                    created_at: now,
                    updated_at: now,
                })
                .map_err(err)?;
        }

        // Exactly REFERENCE_OBSERVATIONS observation rows:
        // - project-local rows (one per project, up to 200)
        // - remaining external/global rows, including filesystem candidates for warm scan.
        let project_observation_count = spec.projects.min(200);
        let external_count = spec.observations.saturating_sub(project_observation_count);
        for index in 0..external_count {
            let name = format!("external-skill-{index:04}");
            let path = if index < 200 {
                let candidate = global_skills_root.join(&name);
                fs::create_dir_all(&candidate).map_err(err)?;
                fs::write(
                    candidate.join("SKILL.md"),
                    format!("# {name}\n\nWarm-scan fixture {index}.\n"),
                )
                .map_err(err)?;
                candidate
            } else {
                let candidate = external_root.join(&name);
                fs::create_dir_all(&candidate).map_err(err)?;
                fs::write(
                    candidate.join("SKILL.md"),
                    format!("# {name}\n\nExternal observation fixture {index}.\n"),
                )
                .map_err(err)?;
                candidate
            };
            let path_text = path.to_string_lossy().into_owned();
            let digest = synthetic_digest(10_000 + index as u64);
            vault
                .repositories
                .upsert_observation(ObservationRecord {
                    id: ObservationId::generate(),
                    skill_id: None,
                    adapter_id: adapter.clone(),
                    scope: "global".into(),
                    project_id: None,
                    source_root_kind: "adapter_global".into(),
                    source_root_id: "universal-agent-skills@1".into(),
                    display_path: path.clone(),
                    normalized_path: normalized_path_identity(&path_text),
                    canonical_path: path.canonicalize().ok(),
                    deployment_name: DeploymentName::parse(&name).map_err(err)?,
                    digest: Some(digest),
                    status: "verified".into(),
                    error_code: None,
                    error_summary: None,
                    last_successful_run_id: None,
                    first_seen_at: now,
                    observed_at: now,
                    stale_at: None,
                })
                .map_err(err)?;
        }

        for (index, project_id) in project_ids
            .iter()
            .enumerate()
            .take(project_observation_count)
        {
            let root_index = index % workspace_roots.len();
            let name = format!("project-skill-{index:03}");
            let path = workspace_roots[root_index]
                .join(format!("project-{index:03}"))
                .join(".agents")
                .join("skills")
                .join(&name);
            fs::create_dir_all(&path).map_err(err)?;
            fs::write(path.join("SKILL.md"), format!("# {name}\n")).map_err(err)?;
            let path_text = path.to_string_lossy().into_owned();
            vault
                .repositories
                .upsert_observation(ObservationRecord {
                    id: ObservationId::generate(),
                    skill_id: None,
                    adapter_id: adapter.clone(),
                    scope: "project".into(),
                    project_id: Some(*project_id),
                    source_root_kind: "workspace_root".into(),
                    source_root_id: workspace_root_ids[root_index].to_string(),
                    display_path: path.clone(),
                    normalized_path: normalized_path_identity(&path_text),
                    canonical_path: path.canonicalize().ok(),
                    deployment_name: DeploymentName::parse(&name).map_err(err)?,
                    digest: Some(synthetic_digest(20_000 + index as u64)),
                    status: "verified".into(),
                    error_code: None,
                    error_summary: None,
                    last_successful_run_id: None,
                    first_seen_at: now,
                    observed_at: now,
                    stale_at: None,
                })
                .map_err(err)?;
        }

        Ok(Self {
            _temporary: temporary,
            root,
            home,
            vault,
            skill_ids,
            target_ids,
            workspace_root_ids,
            external_root,
            global_skills_root,
            workspace_roots,
            target_roots,
            generation_elapsed: started.elapsed(),
        })
    }

    /// Counts rows that define the reference dataset.
    pub fn inventory(&self) -> Result<BTreeMap<&'static str, i64>, String> {
        self.vault
            .database
            .execute(|connection| {
                let skills: i64 = connection
                    .query_row(
                        "SELECT count(*) FROM skills WHERE lifecycle = 'active'",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(crate::persistence::DbExecutorError::Sqlite)?;
                let observations: i64 = connection
                    .query_row(
                        "SELECT count(*) FROM observations WHERE status <> 'stale'",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(crate::persistence::DbExecutorError::Sqlite)?;
                let projects: i64 = connection
                    .query_row("SELECT count(*) FROM projects", [], |row| row.get(0))
                    .map_err(crate::persistence::DbExecutorError::Sqlite)?;
                let roots: i64 = connection
                    .query_row("SELECT count(*) FROM workspace_roots", [], |row| row.get(0))
                    .map_err(crate::persistence::DbExecutorError::Sqlite)?;
                let targets: i64 = connection
                    .query_row("SELECT count(*) FROM targets", [], |row| row.get(0))
                    .map_err(crate::persistence::DbExecutorError::Sqlite)?;
                let deployments: i64 = connection
                    .query_row(
                        "SELECT count(*) FROM deployments WHERE active = 1",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(crate::persistence::DbExecutorError::Sqlite)?;
                Ok(BTreeMap::from([
                    ("active_skills", skills),
                    ("observations", observations),
                    ("projects", projects),
                    ("workspace_roots", roots),
                    ("targets", targets),
                    ("active_deployments", deployments),
                ]))
            })
            .map_err(|error| error.to_string())
    }
}

/// Runs `samples` iterations of `op`, returning sorted durations and percentile stats.
pub fn measure_percentiles<F>(
    name: impl Into<String>,
    gate: Duration,
    samples: usize,
    mut op: F,
) -> PercentileReport
where
    F: FnMut() -> Result<(), String>,
{
    let mut durations = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = Instant::now();
        if let Err(error) = op() {
            return PercentileReport {
                name: name.into(),
                samples_ms: Vec::new(),
                p50_ms: f64::NAN,
                p95_ms: f64::NAN,
                max_ms: f64::NAN,
                gate_ms: duration_ms(gate),
                passed: false,
            }
            .with_error(&error);
        }
        durations.push(started.elapsed());
    }
    durations.sort_unstable();
    let samples_ms = durations
        .iter()
        .copied()
        .map(duration_ms)
        .collect::<Vec<_>>();
    let p50 = percentile(&durations, 0.50);
    let p95 = percentile(&durations, 0.95);
    let max = *durations.last().unwrap_or(&Duration::ZERO);
    PercentileReport {
        name: name.into(),
        samples_ms,
        p50_ms: duration_ms(p50),
        p95_ms: duration_ms(p95),
        max_ms: duration_ms(max),
        gate_ms: duration_ms(gate),
        // Gate on p95 so a single best run is not evidence.
        passed: p95 <= gate,
    }
}

impl PercentileReport {
    fn with_error(mut self, error: &str) -> Self {
        self.name = format!("{} (error: {error})", self.name);
        self.passed = false;
        self
    }
}

fn percentile(sorted: &[Duration], fraction: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let last = sorted.len().saturating_sub(1);
    let rank = f64::from(u32::try_from(last).unwrap_or(u32::MAX)).mul_add(fraction, 0.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let index = rank.ceil() as usize;
    sorted[index.min(last)]
}

fn duration_ms(value: Duration) -> f64 {
    value.as_secs_f64() * 1_000.0
}

fn synthetic_digest(seed: u64) -> BundleDigest {
    let mut bytes = [0_u8; 32];
    bytes[..8].copy_from_slice(&seed.to_be_bytes());
    // Spread entropy so consecutive seeds are not trivial prefixes only.
    for (index, slot) in bytes[8..].iter_mut().enumerate() {
        let index_u64 = u64::try_from(index).unwrap_or(0);
        *slot = u8::try_from(seed.wrapping_mul(31).wrapping_add(index_u64) % 251).unwrap_or(0);
    }
    BundleDigest::from_bytes(bytes)
}

#[allow(clippy::needless_pass_by_value)]
fn err(error: impl ToString) -> String {
    error.to_string()
}

/// Hardware / OS / build string for evidence records.
pub fn environment_label(build: &str) -> (String, String, String) {
    let hardware = std::process::Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map_or_else(
            || std::env::consts::ARCH.to_owned(),
            |value| {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    std::env::consts::ARCH.to_owned()
                } else {
                    trimmed.to_owned()
                }
            },
        );
    let mem = std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map_or_else(String::new, |bytes| {
            let gib = bytes / 1_073_741_824;
            format!(" / {gib} GB RAM")
        });
    let os = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map_or_else(
            || std::env::consts::OS.to_owned(),
            |value| format!("macOS {}", value.trim()),
        );
    (format!("{hardware}{mem}"), os, build.to_owned())
}

/// Writes JSON evidence next to an optional path (or stdout-friendly string).
pub fn write_evidence(path: Option<&Path>, evidence: &PerformanceEvidence) -> Result<(), String> {
    let body = serde_json::to_string_pretty(evidence).map_err(err)?;
    if let Some(path) = path {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(err)?;
        }
        fs::write(path, body).map_err(err)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        application::{
            deployment::{
                DeploymentPlanRequest, DeploymentService, FixtureTargetKindDto,
                RegisterTargetRequest,
            },
            scanning::{
                DomainInvalidated, LibraryFilter, LibraryQuery, ScanEventSink, ScanJobs,
                ScanProgress, ScanRequest, ScanRunState, ScanSource, ScanningService,
            },
            takeover::DeploymentModeDto,
        },
        filesystem::{BundleCaps, hash_bundle},
        operations::OperationCoordinator,
        persistence::SkillManifest,
        runtime::BlockingWorkPool,
    };

    struct NoopEvents;

    impl ScanEventSink for NoopEvents {
        fn progress(&self, _: ScanProgress) {}
        fn invalidated(&self, _: DomainInvalidated) {}
    }

    fn i64_count(value: usize) -> i64 {
        i64::try_from(value).expect("reference-scale counts fit in i64")
    }

    fn profile_gate(debug_ms: u64, release_ms: u64) -> Duration {
        Duration::from_millis(if cfg!(debug_assertions) {
            debug_ms
        } else {
            release_ms
        })
    }

    fn report_from_samples(name: &str, gate: Duration, sorted: &[Duration]) -> PercentileReport {
        let samples_ms = sorted.iter().copied().map(duration_ms).collect();
        let p50 = percentile(sorted, 0.50);
        let p95 = percentile(sorted, 0.95);
        let max = *sorted.last().unwrap_or(&Duration::ZERO);
        PercentileReport {
            name: name.to_owned(),
            samples_ms,
            p50_ms: duration_ms(p50),
            p95_ms: duration_ms(p95),
            max_ms: duration_ms(max),
            gate_ms: duration_ms(gate),
            passed: p95 <= gate,
        }
    }

    async fn wait_scan_done(scanning: &ScanningService, job_id: &str) {
        loop {
            let view = scanning.get(job_id).unwrap();
            match view.state {
                ScanRunState::Completed
                | ScanRunState::CompletedWithErrors
                | ScanRunState::Cancelled
                | ScanRunState::Failed => return,
                ScanRunState::Queued | ScanRunState::Running => {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
        }
    }

    async fn measure_library_search(
        scanning: &ScanningService,
        samples: usize,
    ) -> PercentileReport {
        let _ = scanning
            .library_list(LibraryQuery {
                offset: 0,
                limit: 25,
                search: Some("vault-skill-0100".into()),
                filter: LibraryFilter::All,
            })
            .await
            .unwrap();
        let mut durations = Vec::with_capacity(samples);
        for _ in 0..samples {
            let started = Instant::now();
            let page = scanning
                .library_list(LibraryQuery {
                    offset: 0,
                    limit: 25,
                    search: Some("vault-skill-0421".into()),
                    filter: LibraryFilter::All,
                })
                .await
                .unwrap();
            assert_eq!(page.items.len(), 1);
            durations.push(started.elapsed());
        }
        durations.sort_unstable();
        report_from_samples("library_search", profile_gate(2_000, 100), &durations)
    }

    async fn measure_warm_scan(scanning: &ScanningService, samples: usize) -> PercentileReport {
        let events: Arc<dyn ScanEventSink> = Arc::new(NoopEvents);
        let prime = scanning
            .start(
                ScanRequest {
                    source: ScanSource::UniversalGlobal,
                },
                Arc::clone(&events),
            )
            .await
            .unwrap();
        wait_scan_done(scanning, &prime.job_id).await;
        let mut durations = Vec::with_capacity(samples);
        for _ in 0..samples {
            let started = Instant::now();
            let job = scanning
                .start(
                    ScanRequest {
                        source: ScanSource::UniversalGlobal,
                    },
                    Arc::clone(&events),
                )
                .await
                .unwrap();
            wait_scan_done(scanning, &job.job_id).await;
            durations.push(started.elapsed());
        }
        durations.sort_unstable();
        report_from_samples("warm_global_scan", profile_gate(5_000, 1_000), &durations)
    }

    fn measure_workspace(fixture: &ReferenceScaleFixture, samples: usize) -> PercentileReport {
        measure_percentiles(
            "workspace_first_result",
            Duration::from_secs(2),
            samples,
            || {
                let roots = fixture
                    .vault
                    .repositories
                    .workspace_roots()
                    .map_err(|error| error.to_string())?;
                assert!(!roots.is_empty());
                let _ = fixture
                    .vault
                    .repositories
                    .workspace_projects(fixture.workspace_root_ids[0])
                    .map_err(|error| error.to_string())?;
                Ok(())
            },
        )
    }

    fn measure_plan_generation(
        fixture: &ReferenceScaleFixture,
        samples: usize,
    ) -> PercentileReport {
        let skill_id = fixture.skill_ids[0];
        let skill = fixture.vault.repositories.skill(skill_id).unwrap().unwrap();
        let working = fixture.vault.paths.root().join(skill.working_path.as_str());
        let hashed = hash_bundle(&working, BundleCaps::default()).unwrap();
        let mut skill = skill;
        skill.working_digest = hashed.digest;
        skill.baseline_digest = hashed.digest;
        skill.updated_at = UtcTimestamp::now();
        fixture
            .vault
            .repositories
            .upsert_skill(skill.clone())
            .unwrap();
        let manifest = SkillManifest::new(
            skill_id,
            skill.display_name.clone(),
            skill.deployment_name.clone(),
            hashed.digest,
            hashed.digest,
            skill.created_at,
            Vec::new(),
        )
        .unwrap();
        fixture.vault.manifests.write_skill(&manifest).unwrap();

        let deployment = DeploymentService::with_runtime(
            Arc::clone(&fixture.vault),
            Arc::new(OperationCoordinator::new()),
        );
        let plan_root = fixture.root.join("plan-target-fresh");
        fs::create_dir_all(&plan_root).unwrap();
        let target = deployment
            .register_target(&RegisterTargetRequest {
                kind: FixtureTargetKindDto::Global,
                selected_directory: plan_root.to_string_lossy().into_owned(),
                adapter_id: None,
                is_override: None,
            })
            .unwrap();
        let _ = deployment.plan_deployment(&DeploymentPlanRequest {
            skill_id: skill_id.to_string(),
            target_id: target.target_id.clone(),
            requested_mode: Some(DeploymentModeDto::Symlink),
        });
        measure_percentiles("plan_generation", profile_gate(2_000, 500), samples, || {
            deployment
                .plan_deployment(&DeploymentPlanRequest {
                    skill_id: skill_id.to_string(),
                    target_id: target.target_id.clone(),
                    requested_mode: Some(DeploymentModeDto::Symlink),
                })
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }

    #[test]
    fn reference_scale_fixture_meets_prd_inventory() {
        let fixture = ReferenceScaleFixture::generate(ReferenceScaleSpec::default()).unwrap();
        let inventory = fixture.inventory().unwrap();
        assert_eq!(
            inventory["active_skills"],
            i64_count(REFERENCE_VAULT_SKILLS)
        );
        assert_eq!(
            inventory["observations"],
            i64_count(REFERENCE_OBSERVATIONS),
            "observations={}",
            inventory["observations"]
        );
        assert!(inventory["active_deployments"] >= 1_000);
        assert_eq!(inventory["projects"], i64_count(REFERENCE_PROJECTS));
        assert_eq!(
            inventory["workspace_roots"],
            i64_count(REFERENCE_WORKSPACE_ROOTS)
        );
        assert_eq!(inventory["targets"], i64_count(REFERENCE_TARGETS));
        assert!(fixture.generation_elapsed < Duration::from_secs(120));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn library_search_is_interactive_at_reference_scale() {
        let fixture = ReferenceScaleFixture::generate(ReferenceScaleSpec::default()).unwrap();
        let service = ScanningService::new(
            fixture.home.clone(),
            fixture.vault.repositories.clone(),
            BlockingWorkPool::new(2),
            ScanJobs::default(),
        );
        let report = measure_library_search(&service, DEFAULT_PERF_SAMPLES).await;
        assert!(
            report.passed,
            "library search p95 {}ms exceeded gate {}ms (samples_ms={:?})",
            report.p95_ms, report.gate_ms, report.samples_ms
        );
    }

    #[test]
    fn percentile_helper_rejects_empty_failure() {
        let report = measure_percentiles("always_fails", Duration::from_millis(1), 3, || {
            Err("boom".into())
        });
        assert!(!report.passed);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn reference_scale_ci_gates_and_optional_evidence() {
        let fixture = ReferenceScaleFixture::generate(ReferenceScaleSpec::default()).unwrap();
        let scanning = ScanningService::new(
            fixture.home.clone(),
            fixture.vault.repositories.clone(),
            BlockingWorkPool::new(4),
            ScanJobs::default(),
        );
        let samples = DEFAULT_PERF_SAMPLES;
        let mut measurements = vec![
            measure_library_search(&scanning, samples).await,
            measure_warm_scan(&scanning, samples).await,
            measure_workspace(&fixture, samples),
            measure_plan_generation(&fixture, samples),
        ];
        for report in &measurements {
            assert!(
                report.passed,
                "{} p95={}ms samples={:?}",
                report.name, report.p95_ms, report.samples_ms
            );
        }
        let (hardware, os, build) = environment_label(if cfg!(debug_assertions) {
            "debug (CI gate; release evidence via scripts/perf-harness.sh --release)"
        } else {
            "release"
        });
        let evidence = PerformanceEvidence {
            hardware,
            os,
            build,
            dataset: format!(
                "reference-scale inventory={:?}",
                fixture.inventory().unwrap()
            ),
            sample_count: samples,
            measurements: std::mem::take(&mut measurements),
        };
        if let Some(path) = std::env::var_os("SKILLS_HUB_PERF_OUT") {
            write_evidence(Some(Path::new(&path)), &evidence).unwrap();
        }
        assert!(evidence.measurements.iter().all(|item| item.passed));
    }
}
