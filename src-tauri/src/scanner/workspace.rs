//! Bounded, read-only Workspace project discovery.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use ignore::{
    WalkBuilder,
    gitignore::{Gitignore, GitignoreBuilder},
};

use super::{
    CancellationFlag, CoverageState, GlobalScanRequest, ScanDiagnostic, ScanObservation,
    scan_global_root,
};
use crate::{domain::AdapterId, filesystem::BundleCaps};

const DEFAULT_PRUNES: &[&str] = &[
    ".git",
    "node_modules",
    "vendor",
    "target",
    "dist",
    "build",
    ".cache",
    ".next",
    "DerivedData",
    "coverage",
    "out",
];

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceAdapter {
    pub adapter_id: AdapterId,
    pub target_suffix: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectKind {
    Git,
    Implicit,
    ManualGit,
    ManualNonGit,
}

#[derive(Debug, Clone)]
pub(crate) struct ManualProject {
    pub root: PathBuf,
    pub is_git: bool,
    pub device_id: u64,
    pub file_id: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceScanRequest {
    pub source_root_id: String,
    pub selected_root: PathBuf,
    pub canonical_root: PathBuf,
    pub device_id: u64,
    pub file_id: u64,
    pub max_depth: u8,
    pub user_ignores: Vec<String>,
    pub adapters: Vec<WorkspaceAdapter>,
    pub manual_projects: Vec<ManualProject>,
    pub caps: BundleCaps,
    pub cancellation: CancellationFlag,
}

impl WorkspaceScanRequest {
    pub const DEFAULT_MAX_DEPTH: u8 = 8;
    pub fn validate(&self) -> Result<(), ScanDiagnostic> {
        if !(1..=32).contains(&self.max_depth) {
            return Err(diag(
                &self.selected_root,
                "invalid_depth",
                "Workspace depth must be between 1 and 32.",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectBatch {
    pub project_root: PathBuf,
    pub kind: ProjectKind,
    pub observations: Vec<ScanObservation>,
    pub diagnostics: Vec<ScanDiagnostic>,
    /// False means consumers must not infer absence from this batch.
    pub batch_complete: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceScanResult {
    pub source_root_id: String,
    pub coverage: CoverageState,
    pub batches: Vec<ProjectBatch>,
    pub diagnostics: Vec<ScanDiagnostic>,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn scan_workspace(
    request: &WorkspaceScanRequest,
    mut emit: impl FnMut(&ProjectBatch),
) -> WorkspaceScanResult {
    let mut diagnostics = Vec::new();
    if let Err(error) = request.validate() {
        return terminal(request, CoverageState::InvalidRoot, vec![error]);
    }
    if request.cancellation.is_cancelled() {
        return terminal(request, CoverageState::Cancelled, vec![]);
    }
    let meta = match fs::symlink_metadata(&request.selected_root) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return terminal(
                request,
                CoverageState::Missing,
                vec![diag(
                    &request.selected_root,
                    "missing_root",
                    "Workspace root does not exist.",
                )],
            );
        }
        Err(_) => {
            return terminal(
                request,
                CoverageState::Inaccessible,
                vec![diag(
                    &request.selected_root,
                    "inaccessible_root",
                    "Workspace root cannot be read.",
                )],
            );
        }
    };
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return terminal(
            request,
            CoverageState::InvalidRoot,
            vec![diag(
                &request.selected_root,
                "invalid_root",
                "Workspace root must be a real directory.",
            )],
        );
    }
    let identity = crate::filesystem::PathIdentity::from_metadata(&meta);
    if identity.device_id != request.device_id || identity.file_id != request.file_id {
        return terminal(
            request,
            CoverageState::InvalidRoot,
            vec![diag(
                &request.selected_root,
                "root_replaced",
                "Workspace root filesystem identity no longer matches.",
            )],
        );
    }
    let Ok(actual) = request.selected_root.canonicalize() else {
        return terminal(
            request,
            CoverageState::Inaccessible,
            vec![diag(
                &request.selected_root,
                "inaccessible_root",
                "Workspace root cannot be resolved.",
            )],
        );
    };
    if actual != request.canonical_root {
        return terminal(
            request,
            CoverageState::InvalidRoot,
            vec![diag(
                &request.selected_root,
                "root_replaced",
                "Workspace root identity no longer matches.",
            )],
        );
    }

    let ignores = compile_ignores(&actual, &request.user_ignores, &mut diagnostics);
    let protected = request
        .adapters
        .iter()
        .flat_map(|adapter| target_prefixes(&adapter.target_suffix))
        .collect::<Vec<_>>();
    for rule in &request.user_ignores {
        if protected.iter().any(|prefix| {
            rule.trim_matches('/') == prefix.to_string_lossy()
                || prefix
                    .components()
                    .any(|part| rule.trim_matches('/') == part.as_os_str().to_string_lossy())
        }) {
            diagnostics.push(diag(
                &actual,
                "reduced_coverage",
                "An ignore rule overlaps an adapter target; coverage is reduced.",
            ));
        }
    }
    let mut projects = BTreeMap::<PathBuf, ProjectKind>::new();
    let mut incomplete = !diagnostics.is_empty();
    let mut walker = WalkBuilder::new(&actual);
    walker
        .hidden(false)
        .follow_links(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false);
    let max_depth = usize::from(request.max_depth);
    let suffix_depth = request
        .adapters
        .iter()
        .map(|adapter| adapter.target_suffix.components().count())
        .max()
        .unwrap_or(0);
    walker.max_depth(Some(max_depth + suffix_depth + 1));
    let filter_root = actual.clone();
    let filter_ignores = ignores.clone();
    let filter_protected = protected.clone();
    walker.filter_entry(move |entry| {
        let relative = entry
            .path()
            .strip_prefix(&filter_root)
            .unwrap_or(entry.path());
        relative.as_os_str().is_empty()
            || !ignored(
                relative,
                entry.file_type().is_some_and(|kind| kind.is_dir()),
                &filter_ignores,
                &filter_protected,
            )
    });
    let direct_manual = request
        .manual_projects
        .iter()
        .any(|manual| manual.root.canonicalize().is_ok_and(|root| root == actual));
    if !direct_manual {
        for entry in walker.build() {
            if request.cancellation.is_cancelled() {
                return finish(request, CoverageState::Cancelled, vec![], diagnostics);
            }
            let entry = match entry {
                Ok(e) => e,
                Err(error) => {
                    incomplete = true;
                    diagnostics.push(diag(&actual, "walk_error", &error.to_string()));
                    continue;
                }
            };
            let path = entry.path();
            let relative = path.strip_prefix(&actual).unwrap_or(path);
            let depth = relative.components().count();
            if depth == max_depth && entry.file_type().is_some_and(|t| t.is_dir()) {
                incomplete = true;
            }
            if depth <= max_depth && entry.file_type().is_some_and(|kind| kind.is_dir()) {
                let git = path.join(".git");
                if fs::symlink_metadata(git)
                    .is_ok_and(|metadata| metadata.is_dir() || metadata.is_file())
                {
                    projects.insert(path.to_path_buf(), ProjectKind::Git);
                }
            }
            for adapter in &request.adapters {
                if relative.ends_with(&adapter.target_suffix)
                    && let Some(root) = strip_suffix(path, &adapter.target_suffix)
                    && root
                        .strip_prefix(&actual)
                        .is_ok_and(|path| path.components().count() <= max_depth)
                {
                    projects.entry(root).or_insert(ProjectKind::Implicit);
                }
            }
        }
    }
    for manual in &request.manual_projects {
        let metadata = match fs::symlink_metadata(&manual.root) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => metadata,
            Ok(_) => {
                diagnostics.push(diag(
                    &manual.root,
                    "manual_project_invalid",
                    "Manual project is not a real directory.",
                ));
                incomplete = true;
                continue;
            }
            Err(error) => {
                diagnostics.push(diag(
                    &manual.root,
                    "manual_project_unavailable",
                    &error.to_string(),
                ));
                incomplete = true;
                continue;
            }
        };
        let identity = crate::filesystem::PathIdentity::from_metadata(&metadata);
        if identity.device_id != manual.device_id || identity.file_id != manual.file_id {
            diagnostics.push(diag(
                &manual.root,
                "manual_project_replaced",
                "Manual project filesystem identity no longer matches.",
            ));
            incomplete = true;
            continue;
        }
        projects.insert(
            manual.root.clone(),
            if manual.is_git {
                ProjectKind::ManualGit
            } else {
                ProjectKind::ManualNonGit
            },
        );
    }
    let mut batches = Vec::new();
    for (root, kind) in projects {
        if request.cancellation.is_cancelled() {
            return finish(request, CoverageState::Cancelled, batches, diagnostics);
        }
        let mut observations = Vec::new();
        let mut project_diagnostics = Vec::new();
        let mut complete = true;
        for adapter in &request.adapters {
            let result = scan_global_root(
                &GlobalScanRequest {
                    adapter_id: adapter.adapter_id.clone(),
                    source_root_id: request.source_root_id.clone(),
                    root: root.join(&adapter.target_suffix),
                    caps: request.caps,
                    managed_links: BTreeMap::new(),
                },
                &request.cancellation,
                |_| {},
            );
            if !matches!(
                result.coverage,
                CoverageState::Complete | CoverageState::Missing
            ) {
                complete = false;
            }
            observations.extend(result.observations);
            if result.coverage != CoverageState::Missing {
                project_diagnostics.extend(result.diagnostics);
            }
        }
        observations.sort_by(|a, b| a.normalized_path.cmp(&b.normalized_path));
        let batch = ProjectBatch {
            project_root: root,
            kind,
            observations,
            diagnostics: project_diagnostics,
            batch_complete: complete,
        };
        emit(&batch);
        batches.push(batch);
    }
    let coverage = if request.cancellation.is_cancelled() {
        CoverageState::Cancelled
    } else if incomplete || batches.iter().any(|b| !b.batch_complete) {
        CoverageState::Partial
    } else {
        CoverageState::Complete
    };
    finish(request, coverage, batches, diagnostics)
}

fn compile_ignores(
    root: &Path,
    rules: &[String],
    diagnostics: &mut Vec<ScanDiagnostic>,
) -> Gitignore {
    let mut builder = GitignoreBuilder::new(root);
    for rule in rules {
        if let Err(error) = builder.add_line(None, rule) {
            diagnostics.push(diag(root, "invalid_ignore", &error.to_string()));
        }
    }
    builder.build().unwrap_or_else(|error| {
        diagnostics.push(diag(root, "invalid_ignore", &error.to_string()));
        Gitignore::empty()
    })
}
fn ignored(relative: &Path, is_dir: bool, user: &Gitignore, protected: &[PathBuf]) -> bool {
    let default_pruned = relative.components().any(|component| {
        DEFAULT_PRUNES.contains(&component.as_os_str().to_string_lossy().as_ref())
    });
    if default_pruned {
        return true;
    }
    let protected_path = protected.iter().any(|prefix| relative.ends_with(prefix));
    !protected_path && user.matched(relative, is_dir).is_ignore()
}
fn target_prefixes(suffix: &Path) -> Vec<PathBuf> {
    let mut current = PathBuf::new();
    suffix
        .components()
        .map(|component| {
            current.push(component.as_os_str());
            current.clone()
        })
        .collect()
}
fn strip_suffix(path: &Path, suffix: &Path) -> Option<PathBuf> {
    let count = suffix.components().count();
    let mut root = path.to_path_buf();
    for _ in 0..count {
        if !root.pop() {
            return None;
        }
    }
    Some(root)
}
fn diag(path: &Path, code: &'static str, summary: &str) -> ScanDiagnostic {
    ScanDiagnostic {
        path: path.to_path_buf(),
        code,
        summary: summary.to_owned(),
    }
}
fn terminal(
    r: &WorkspaceScanRequest,
    c: CoverageState,
    d: Vec<ScanDiagnostic>,
) -> WorkspaceScanResult {
    finish(r, c, vec![], d)
}
fn finish(
    r: &WorkspaceScanRequest,
    coverage: CoverageState,
    batches: Vec<ProjectBatch>,
    diagnostics: Vec<ScanDiagnostic>,
) -> WorkspaceScanResult {
    WorkspaceScanResult {
        source_root_id: r.source_root_id.clone(),
        coverage,
        batches,
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use tempfile::TempDir;
    fn adapter() -> WorkspaceAdapter {
        WorkspaceAdapter {
            adapter_id: AdapterId::new("test", 1).unwrap(),
            target_suffix: ".agents/skills".into(),
        }
    }
    fn skill(root: &Path, name: &str) {
        let p = root.join(".agents/skills").join(name);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("SKILL.md"), "---\nname: x\ndescription: y\n---\n").unwrap();
    }
    fn req(t: &TempDir) -> WorkspaceScanRequest {
        let metadata = fs::symlink_metadata(t.path()).unwrap();
        let identity = crate::filesystem::PathIdentity::from_metadata(&metadata);
        WorkspaceScanRequest {
            source_root_id: "r".into(),
            selected_root: t.path().into(),
            canonical_root: t.path().canonicalize().unwrap(),
            device_id: identity.device_id,
            file_id: identity.file_id,
            max_depth: 8,
            user_ignores: vec![],
            adapters: vec![adapter()],
            manual_projects: vec![],
            caps: BundleCaps::default(),
            cancellation: CancellationFlag::default(),
        }
    }
    #[test]
    fn nested_git_and_hidden_implicit_are_distinct_and_deterministic() {
        let t = TempDir::new().unwrap();
        for p in ["z", "z/nested", "a"] {
            fs::create_dir_all(t.path().join(p).join(".git")).unwrap();
            skill(&t.path().join(p), p);
        }
        let one = scan_workspace(&req(&t), |_| {});
        let two = scan_workspace(&req(&t), |_| {});
        assert_eq!(one.batches.len(), 3);
        assert_eq!(
            one.batches
                .iter()
                .map(|b| &b.project_root)
                .collect::<Vec<_>>(),
            two.batches
                .iter()
                .map(|b| &b.project_root)
                .collect::<Vec<_>>()
        );
    }
    #[test]
    fn prune_symlink_depth_cancel_and_manual() {
        let t = TempDir::new().unwrap();
        skill(&t.path().join("node_modules/pkg"), "bad");
        fs::create_dir_all(t.path().join("ordinary/deeper")).unwrap();
        let outside = TempDir::new().unwrap();
        skill(outside.path(), "escape");
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), t.path().join("link")).unwrap();
        let mut r = req(&t);
        r.max_depth = 1;
        r.manual_projects.push(ManualProject {
            root: outside.path().into(),
            is_git: false,
            device_id: crate::filesystem::PathIdentity::from_metadata(
                &fs::symlink_metadata(outside.path()).unwrap(),
            )
            .device_id,
            file_id: crate::filesystem::PathIdentity::from_metadata(
                &fs::symlink_metadata(outside.path()).unwrap(),
            )
            .file_id,
        });
        let result = scan_workspace(&r, |_| {});
        assert_eq!(result.batches.len(), 1);
        assert_eq!(result.coverage, CoverageState::Partial);
        r.cancellation.cancel();
        assert_eq!(
            scan_workspace(&r, |_| {}).coverage,
            CoverageState::Cancelled
        );
    }
    #[test]
    #[allow(clippy::many_single_char_names)]
    fn manual_git_and_non_git_bypass_depth() {
        let t = TempDir::new().unwrap();
        let a = t.path().join("deep/a");
        let b = t.path().join("deep/b");
        skill(&a, "a");
        skill(&b, "b");
        let mut r = req(&t);
        r.max_depth = 1;
        r.manual_projects = vec![
            ManualProject {
                root: a.clone(),
                is_git: true,
                device_id: crate::filesystem::PathIdentity::from_metadata(
                    &fs::symlink_metadata(&a).unwrap(),
                )
                .device_id,
                file_id: crate::filesystem::PathIdentity::from_metadata(
                    &fs::symlink_metadata(&a).unwrap(),
                )
                .file_id,
            },
            ManualProject {
                root: b.clone(),
                is_git: false,
                device_id: crate::filesystem::PathIdentity::from_metadata(
                    &fs::symlink_metadata(&b).unwrap(),
                )
                .device_id,
                file_id: crate::filesystem::PathIdentity::from_metadata(
                    &fs::symlink_metadata(&b).unwrap(),
                )
                .file_id,
            },
        ];
        let x = scan_workspace(&r, |_| {});
        assert!(x.batches.iter().any(|b| b.kind == ProjectKind::ManualGit));
        assert!(
            x.batches
                .iter()
                .any(|b| b.kind == ProjectKind::ManualNonGit)
        );
    }

    #[test]
    fn unavailable_manual_project_never_establishes_complete_absence() {
        let workspace = TempDir::new().unwrap();
        let manual = TempDir::new().unwrap();
        let manual_path = manual.path().to_path_buf();
        let identity = crate::filesystem::PathIdentity::from_metadata(
            &fs::symlink_metadata(&manual_path).unwrap(),
        );
        let mut request = req(&workspace);
        request.manual_projects.push(ManualProject {
            root: manual_path.clone(),
            is_git: false,
            device_id: identity.device_id,
            file_id: identity.file_id,
        });
        drop(manual);

        let result = scan_workspace(&request, |_| {});

        assert_eq!(result.coverage, CoverageState::Partial);
        assert!(result.batches.is_empty());
        assert!(
            result
                .diagnostics
                .iter()
                .any(|item| item.code == "manual_project_unavailable")
        );
    }

    proptest! {
        #[test]
        fn ignore_and_depth_combinations_never_claim_pruned_content_as_discovered(
            depth in 1_u8..=8,
            ignore_ordinary in any::<bool>(),
        ) {
            let directory = TempDir::new().unwrap();
            let mut nested = directory.path().join("ordinary");
            for _ in 0..=depth {
                nested.push("child");
            }
            fs::create_dir_all(nested).unwrap();
            let mut request = req(&directory);
            request.max_depth = depth;
            if ignore_ordinary {
                request.user_ignores.push("ordinary/".to_owned());
            }

            let result = scan_workspace(&request, |_| {});

            prop_assert!(result.batches.is_empty());
            prop_assert_eq!(
                result.coverage,
                if ignore_ordinary {
                    CoverageState::Complete
                } else {
                    CoverageState::Partial
                }
            );
        }
    }
}
