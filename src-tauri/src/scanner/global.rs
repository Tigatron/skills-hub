use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::{Deserialize, Serialize};

use crate::{
    domain::{AdapterId, BundleDigest, DeploymentName, SkillId, normalized_path_identity},
    filesystem::{BundleCaps, BundleHashError, hash_bundle},
};

#[derive(Debug, Clone)]
pub(crate) struct GlobalScanRequest {
    pub adapter_id: AdapterId,
    pub source_root_id: String,
    pub root: PathBuf,
    pub caps: BundleCaps,
    pub managed_links: BTreeMap<String, ManagedLinkExpectation>,
}

#[derive(Debug, Clone)]
pub(crate) struct ManagedLinkExpectation {
    pub skill_id: SkillId,
    pub raw_target: PathBuf,
    pub resolved_target: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CancellationFlag(Arc<AtomicBool>);

impl CancellationFlag {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CoverageState {
    Complete,
    Missing,
    Inaccessible,
    InvalidRoot,
    Partial,
    Cancelled,
}

impl CoverageState {
    #[must_use]
    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ScanObservationStatus {
    Verified,
    HashError,
    PermissionDenied,
    UnsupportedBundle,
    UnstableInput,
    ManagedLinkVerified,
    ManagedLinkError,
    ManagedLinkMismatch,
    BrokenOrCyclicLink,
    SymlinkOutsideAuthorizedRoot,
    UnknownSymlink,
}

impl ScanObservationStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::HashError => "hash_error",
            Self::PermissionDenied => "permission_denied",
            Self::UnsupportedBundle => "unsupported_bundle",
            Self::UnstableInput => "unstable_input",
            Self::ManagedLinkVerified => "managed_link_verified",
            Self::ManagedLinkError => "managed_link_error",
            Self::ManagedLinkMismatch => "managed_link_mismatch",
            Self::BrokenOrCyclicLink => "broken_or_cyclic_link",
            Self::SymlinkOutsideAuthorizedRoot => "symlink_outside_authorized_root",
            Self::UnknownSymlink => "unknown_symlink",
        }
    }

    #[cfg(test)]
    #[must_use]
    pub const fn is_verified(self) -> bool {
        matches!(self, Self::Verified | Self::ManagedLinkVerified)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ScanObservation {
    pub skill_id: Option<SkillId>,
    pub adapter_id: AdapterId,
    pub source_root_id: String,
    pub display_path: PathBuf,
    pub normalized_path: String,
    pub canonical_path: Option<PathBuf>,
    pub deployment_name: DeploymentName,
    pub digest: Option<BundleDigest>,
    pub status: ScanObservationStatus,
    pub error: Option<ScanDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScanDiagnostic {
    pub path: PathBuf,
    pub code: &'static str,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScanProgress {
    pub completed_entries: usize,
    pub estimated_entries: usize,
    pub current_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct GlobalScanResult {
    pub adapter_id: AdapterId,
    pub source_root_id: String,
    pub coverage: CoverageState,
    pub observations: Vec<ScanObservation>,
    pub diagnostics: Vec<ScanDiagnostic>,
    pub completed_entries: usize,
    pub estimated_entries: usize,
}

impl GlobalScanResult {
    fn terminal(
        request: &GlobalScanRequest,
        coverage: CoverageState,
        diagnostics: Vec<ScanDiagnostic>,
    ) -> Self {
        Self {
            adapter_id: request.adapter_id.clone(),
            source_root_id: request.source_root_id.clone(),
            coverage,
            observations: Vec::new(),
            diagnostics,
            completed_entries: 0,
            estimated_entries: 0,
        }
    }
}

/// Scans one global adapter root without following unknown directory links or mutating the root.
#[allow(clippy::too_many_lines)]
pub(crate) fn scan_global_root(
    request: &GlobalScanRequest,
    cancellation: &CancellationFlag,
    mut progress: impl FnMut(ScanProgress),
) -> GlobalScanResult {
    if cancellation.is_cancelled() {
        return GlobalScanResult::terminal(request, CoverageState::Cancelled, Vec::new());
    }

    let root_metadata = match fs::symlink_metadata(&request.root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return GlobalScanResult::terminal(
                request,
                CoverageState::Missing,
                vec![io_diagnostic(&request.root, "root_missing", &error)],
            );
        }
        Err(error) => {
            return GlobalScanResult::terminal(
                request,
                CoverageState::Inaccessible,
                vec![io_diagnostic(&request.root, io_code(&error), &error)],
            );
        }
    };
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return GlobalScanResult::terminal(
            request,
            CoverageState::InvalidRoot,
            vec![ScanDiagnostic {
                path: request.root.clone(),
                code: "invalid_root",
                summary: "The configured scan root is not a real directory.".to_owned(),
            }],
        );
    }
    let canonical_root = match request.root.canonicalize() {
        Ok(path) => path,
        Err(error) => {
            return GlobalScanResult::terminal(
                request,
                CoverageState::Inaccessible,
                vec![io_diagnostic(&request.root, io_code(&error), &error)],
            );
        }
    };

    let (entries, mut diagnostics, enumeration_complete) = match enumerate_root(&request.root) {
        Ok(enumeration) => enumeration,
        Err(diagnostic) => {
            return GlobalScanResult::terminal(
                request,
                CoverageState::Inaccessible,
                vec![diagnostic],
            );
        }
    };

    let estimated_entries = entries.len();
    let mut observations = Vec::new();
    let mut completed_entries = 0;
    progress(ScanProgress {
        completed_entries,
        estimated_entries,
        current_path: None,
    });

    for entry in entries {
        if cancellation.is_cancelled() {
            return GlobalScanResult {
                adapter_id: request.adapter_id.clone(),
                source_root_id: request.source_root_id.clone(),
                coverage: CoverageState::Cancelled,
                observations,
                diagnostics,
                completed_entries,
                estimated_entries,
            };
        }

        let path = entry.path();
        match scan_child(request, &canonical_root, &path) {
            ChildResult::Ignored => {}
            ChildResult::Diagnostic(diagnostic) => diagnostics.push(diagnostic),
            ChildResult::Observation(observation) => {
                if let Some(error) = &observation.error {
                    diagnostics.push(error.clone());
                }
                observations.push(*observation);
            }
        }
        completed_entries += 1;
        progress(ScanProgress {
            completed_entries,
            estimated_entries,
            current_path: Some(path),
        });
    }

    GlobalScanResult {
        adapter_id: request.adapter_id.clone(),
        source_root_id: request.source_root_id.clone(),
        coverage: if enumeration_complete {
            CoverageState::Complete
        } else {
            CoverageState::Partial
        },
        observations,
        diagnostics,
        completed_entries,
        estimated_entries,
    }
}

fn enumerate_root(
    root: &Path,
) -> Result<(Vec<fs::DirEntry>, Vec<ScanDiagnostic>, bool), ScanDiagnostic> {
    let children =
        fs::read_dir(root).map_err(|error| io_diagnostic(root, io_code(&error), &error))?;
    let mut entries = Vec::new();
    let mut diagnostics = Vec::new();
    let mut complete = true;
    for child in children {
        match child {
            Ok(child) => entries.push(child),
            Err(error) => {
                complete = false;
                diagnostics.push(io_diagnostic(root, io_code(&error), &error));
            }
        }
    }
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok((entries, diagnostics, complete))
}

enum ChildResult {
    Ignored,
    Diagnostic(ScanDiagnostic),
    Observation(Box<ScanObservation>),
}

impl ChildResult {
    fn observation(observation: ScanObservation) -> Self {
        Self::Observation(Box::new(observation))
    }
}

fn scan_child(request: &GlobalScanRequest, canonical_root: &Path, path: &Path) -> ChildResult {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            let Some((deployment_name, normalized_path)) = candidate_identity(canonical_root, path)
            else {
                return ChildResult::Diagnostic(io_diagnostic(path, io_code(&error), &error));
            };
            return ChildResult::observation(unverified_directory_observation(
                request,
                path,
                deployment_name,
                normalized_path,
                io_error_status(&error),
                io_diagnostic(path, io_code(&error), &error),
            ));
        }
    };
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        return scan_directory(request, canonical_root, path);
    }
    if file_type.is_symlink() {
        return scan_symlink(request, canonical_root, path);
    }
    ChildResult::Ignored
}

#[allow(clippy::too_many_lines)]
fn scan_directory(request: &GlobalScanRequest, canonical_root: &Path, path: &Path) -> ChildResult {
    let manifest = path.join("SKILL.md");
    let manifest_metadata = match fs::symlink_metadata(&manifest) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return ChildResult::Ignored,
        Err(error) => {
            let Some((deployment_name, normalized_path)) = candidate_identity(canonical_root, path)
            else {
                return ChildResult::Diagnostic(ScanDiagnostic {
                    path: path.to_path_buf(),
                    code: "unsupported_name",
                    summary: "The Skill directory name or path cannot be represented safely."
                        .to_owned(),
                });
            };
            return ChildResult::observation(unverified_directory_observation(
                request,
                path,
                deployment_name,
                normalized_path,
                io_error_status(&error),
                io_diagnostic(&manifest, io_code(&error), &error),
            ));
        }
    };
    let Some((deployment_name, normalized_path)) = candidate_identity(canonical_root, path) else {
        return ChildResult::Diagnostic(ScanDiagnostic {
            path: path.to_path_buf(),
            code: "unsupported_name",
            summary: "The Skill directory name or path cannot be represented safely.".to_owned(),
        });
    };
    if manifest_metadata.file_type().is_symlink() || !manifest_metadata.is_file() {
        return ChildResult::observation(unverified_directory_observation(
            request,
            path,
            deployment_name,
            normalized_path,
            ScanObservationStatus::UnsupportedBundle,
            ScanDiagnostic {
                path: manifest,
                code: "invalid_skill_manifest_type",
                summary: "SKILL.md must be a direct regular file.".to_owned(),
            },
        ));
    }

    let canonical_path = match path.canonicalize() {
        Ok(canonical) if canonical.starts_with(canonical_root) => Some(canonical),
        Ok(_) => {
            return ChildResult::observation(unverified_directory_observation(
                request,
                path,
                deployment_name,
                normalized_path,
                ScanObservationStatus::UnsupportedBundle,
                ScanDiagnostic {
                    path: path.to_path_buf(),
                    code: "path_outside_authorized_root",
                    summary: "The Skill directory resolves outside the configured scan root."
                        .to_owned(),
                },
            ));
        }
        Err(error) => {
            return ChildResult::observation(unverified_directory_observation(
                request,
                path,
                deployment_name,
                normalized_path,
                io_error_status(&error),
                io_diagnostic(path, io_code(&error), &error),
            ));
        }
    };

    let (digest, status, error) = match hash_bundle(path, request.caps) {
        Ok(hashed) => (Some(hashed.digest), ScanObservationStatus::Verified, None),
        Err(error) => {
            let code = bundle_error_code(&error);
            let status = bundle_error_status(&error);
            (
                None,
                status,
                Some(ScanDiagnostic {
                    path: path.to_path_buf(),
                    code,
                    summary: error.to_string(),
                }),
            )
        }
    };
    ChildResult::observation(ScanObservation {
        skill_id: None,
        adapter_id: request.adapter_id.clone(),
        source_root_id: request.source_root_id.clone(),
        display_path: path.to_path_buf(),
        normalized_path,
        canonical_path,
        deployment_name,
        digest,
        status,
        error,
    })
}

fn unverified_directory_observation(
    request: &GlobalScanRequest,
    path: &Path,
    deployment_name: DeploymentName,
    normalized_path: String,
    status: ScanObservationStatus,
    error: ScanDiagnostic,
) -> ScanObservation {
    ScanObservation {
        skill_id: None,
        adapter_id: request.adapter_id.clone(),
        source_root_id: request.source_root_id.clone(),
        display_path: path.to_path_buf(),
        normalized_path,
        canonical_path: path.canonicalize().ok(),
        deployment_name,
        digest: None,
        status,
        error: Some(error),
    }
}

fn scan_symlink(request: &GlobalScanRequest, canonical_root: &Path, path: &Path) -> ChildResult {
    let Some((deployment_name, normalized_path)) = candidate_identity(canonical_root, path) else {
        return ChildResult::Diagnostic(ScanDiagnostic {
            path: path.to_path_buf(),
            code: "unsupported_name",
            summary: "The linked Skill name or path cannot be represented safely.".to_owned(),
        });
    };
    let raw_target = match fs::read_link(path) {
        Ok(target) => target,
        Err(error) => {
            return ChildResult::Diagnostic(io_diagnostic(path, io_code(&error), &error));
        }
    };

    if let Some(expected) = request.managed_links.get(&normalized_path) {
        return scan_managed_link(
            request,
            path,
            deployment_name,
            normalized_path,
            &raw_target,
            expected,
        );
    }

    match path.canonicalize() {
        Ok(resolved) if resolved.starts_with(canonical_root) => {
            ChildResult::observation(link_observation(
                request,
                path,
                deployment_name,
                normalized_path,
                Some(resolved),
                ScanObservationStatus::UnknownSymlink,
                "unknown_symlink",
                "An unmanaged Skill-directory link was not followed.",
            ))
        }
        Ok(resolved) => ChildResult::observation(link_observation(
            request,
            path,
            deployment_name,
            normalized_path,
            Some(resolved),
            ScanObservationStatus::SymlinkOutsideAuthorizedRoot,
            "symlink_outside_authorized_root",
            "An unmanaged link resolves outside the configured scan root and was not followed.",
        )),
        Err(error) => ChildResult::observation(link_observation(
            request,
            path,
            deployment_name,
            normalized_path,
            None,
            ScanObservationStatus::BrokenOrCyclicLink,
            "broken_or_cyclic_link",
            &format!("The unmanaged link is broken or cyclic: {error}"),
        )),
    }
}

fn scan_managed_link(
    request: &GlobalScanRequest,
    path: &Path,
    deployment_name: DeploymentName,
    normalized_path: String,
    raw_target: &Path,
    expected: &ManagedLinkExpectation,
) -> ChildResult {
    let resolved = path.canonicalize();
    let expected_resolved = expected.resolved_target.canonicalize();
    if raw_target != expected.raw_target.as_path()
        || resolved.as_ref().ok() != expected_resolved.as_ref().ok()
        || resolved.is_err()
    {
        return ChildResult::observation(link_observation_with_skill(
            request,
            path,
            deployment_name,
            normalized_path,
            resolved.ok(),
            Some(expected.skill_id),
            ScanObservationStatus::ManagedLinkMismatch,
            "managed_link_mismatch",
            "The managed link no longer matches its recorded Vault working target.",
        ));
    }

    let resolved = resolved.expect("checked successful managed-link resolution");
    match hash_bundle(&resolved, request.caps) {
        Ok(hashed) => ChildResult::observation(ScanObservation {
            skill_id: Some(expected.skill_id),
            adapter_id: request.adapter_id.clone(),
            source_root_id: request.source_root_id.clone(),
            display_path: path.to_path_buf(),
            normalized_path,
            canonical_path: Some(resolved),
            deployment_name,
            digest: Some(hashed.digest),
            status: ScanObservationStatus::ManagedLinkVerified,
            error: None,
        }),
        Err(error) => ChildResult::observation(link_observation_with_skill(
            request,
            path,
            deployment_name,
            normalized_path,
            Some(resolved),
            Some(expected.skill_id),
            ScanObservationStatus::ManagedLinkError,
            bundle_error_code(&error),
            &error.to_string(),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn link_observation(
    request: &GlobalScanRequest,
    path: &Path,
    deployment_name: DeploymentName,
    normalized_path: String,
    canonical_path: Option<PathBuf>,
    status: ScanObservationStatus,
    code: &'static str,
    summary: &str,
) -> ScanObservation {
    link_observation_with_skill(
        request,
        path,
        deployment_name,
        normalized_path,
        canonical_path,
        None,
        status,
        code,
        summary,
    )
}

#[allow(clippy::too_many_arguments)]
fn link_observation_with_skill(
    request: &GlobalScanRequest,
    path: &Path,
    deployment_name: DeploymentName,
    normalized_path: String,
    canonical_path: Option<PathBuf>,
    skill_id: Option<SkillId>,
    status: ScanObservationStatus,
    code: &'static str,
    summary: &str,
) -> ScanObservation {
    ScanObservation {
        skill_id,
        adapter_id: request.adapter_id.clone(),
        source_root_id: request.source_root_id.clone(),
        display_path: path.to_path_buf(),
        normalized_path,
        canonical_path,
        deployment_name,
        digest: None,
        status,
        error: Some(ScanDiagnostic {
            path: path.to_path_buf(),
            code,
            summary: summary.to_owned(),
        }),
    }
}

fn candidate_identity(canonical_root: &Path, path: &Path) -> Option<(DeploymentName, String)> {
    let name = path.file_name()?.to_str()?;
    let deployment_name = DeploymentName::parse(name).ok()?;
    let location = canonical_root.join(name);
    let normalized_path = normalized_path_identity(location.to_str()?);
    Some((deployment_name, normalized_path))
}

fn io_diagnostic(path: &Path, code: &'static str, error: &io::Error) -> ScanDiagnostic {
    ScanDiagnostic {
        path: path.to_path_buf(),
        code,
        summary: error.to_string(),
    }
}

fn io_code(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::NotFound => "not_found",
        io::ErrorKind::PermissionDenied => "permission_denied",
        _ => "io_failure",
    }
}

fn io_error_status(error: &io::Error) -> ScanObservationStatus {
    if error.kind() == io::ErrorKind::PermissionDenied {
        ScanObservationStatus::PermissionDenied
    } else {
        ScanObservationStatus::HashError
    }
}

fn bundle_error_code(error: &BundleHashError) -> &'static str {
    match error {
        BundleHashError::ReadRoot { source }
        | BundleHashError::ReadDirectory { source, .. }
        | BundleHashError::ReadEntry { source, .. }
        | BundleHashError::ReadManifest { source }
            if source.kind() == io::ErrorKind::PermissionDenied =>
        {
            "permission_denied"
        }
        BundleHashError::UnstableInput { .. } => "unstable_input",
        BundleHashError::ReadRoot { .. }
        | BundleHashError::ReadDirectory { .. }
        | BundleHashError::ReadEntry { .. }
        | BundleHashError::ReadManifest { .. } => "hash_io_failure",
        _ => "unsupported_bundle",
    }
}

fn bundle_error_status(error: &BundleHashError) -> ScanObservationStatus {
    match bundle_error_code(error) {
        "permission_denied" => ScanObservationStatus::PermissionDenied,
        "unstable_input" => ScanObservationStatus::UnstableInput,
        "unsupported_bundle" => ScanObservationStatus::UnsupportedBundle,
        _ => ScanObservationStatus::HashError,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn request(root: &Path) -> GlobalScanRequest {
        GlobalScanRequest {
            adapter_id: "universal-agent-skills@1".parse().unwrap(),
            source_root_id: "universal-global".to_owned(),
            root: root.to_path_buf(),
            caps: BundleCaps::default(),
            managed_links: BTreeMap::new(),
        }
    }

    fn write_skill(path: &Path, body: &str) {
        fs::create_dir(path).unwrap();
        fs::write(path.join("SKILL.md"), body).unwrap();
    }

    fn tree_evidence(root: &Path) -> BTreeSet<(PathBuf, Vec<u8>, bool)> {
        fn visit(root: &Path, current: &Path, output: &mut BTreeSet<(PathBuf, Vec<u8>, bool)>) {
            let mut entries = fs::read_dir(current)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            entries.sort_by_key(fs::DirEntry::file_name);
            for entry in entries {
                let path = entry.path();
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                let metadata = fs::symlink_metadata(&path).unwrap();
                if metadata.file_type().is_symlink() {
                    output.insert((
                        relative,
                        fs::read_link(path)
                            .unwrap()
                            .to_string_lossy()
                            .as_bytes()
                            .to_vec(),
                        true,
                    ));
                } else if metadata.is_dir() {
                    output.insert((relative, Vec::new(), false));
                    visit(root, &path, output);
                } else {
                    output.insert((relative, fs::read(path).unwrap(), false));
                }
            }
        }

        let mut output = BTreeSet::new();
        visit(root, root, &mut output);
        output
    }

    #[test]
    fn scans_only_immediate_real_directories_with_a_direct_regular_manifest_without_writes() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("skills");
        fs::create_dir(&root).unwrap();
        write_skill(&root.join("valid"), "---\nname: valid\n---\n");
        fs::create_dir(root.join("empty")).unwrap();
        let nested = root.join("nested/too-deep");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("SKILL.md"), "nested").unwrap();
        fs::write(root.join("file"), "not a skill").unwrap();
        let before = tree_evidence(&root);

        let result = scan_global_root(&request(&root), &CancellationFlag::default(), |_| {});

        assert_eq!(result.coverage, CoverageState::Complete);
        assert_eq!(result.observations.len(), 1);
        assert_eq!(result.observations[0].deployment_name.as_str(), "valid");
        assert!(result.observations[0].digest.is_some());
        assert_eq!(tree_evidence(&root), before);
    }

    #[test]
    fn missing_and_invalid_roots_remain_explicit_coverage_states() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing");
        assert_eq!(
            scan_global_root(&request(&missing), &CancellationFlag::default(), |_| {}).coverage,
            CoverageState::Missing
        );

        let file = directory.path().join("file");
        fs::write(&file, "not a root").unwrap();
        assert_eq!(
            scan_global_root(&request(&file), &CancellationFlag::default(), |_| {}).coverage,
            CoverageState::InvalidRoot
        );
    }

    #[cfg(unix)]
    #[test]
    fn inaccessible_root_is_not_reported_as_empty_or_complete() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("skills");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o000)).unwrap();

        let result = scan_global_root(&request(&root), &CancellationFlag::default(), |_| {});

        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(result.coverage, CoverageState::Inaccessible);
        assert!(result.observations.is_empty());
        assert_eq!(result.diagnostics.len(), 1);
    }

    #[test]
    fn hash_errors_stay_visible_without_hiding_valid_siblings() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("skills");
        fs::create_dir(&root).unwrap();
        write_skill(&root.join("valid"), "valid");
        write_skill(&root.join("invalid"), "invalid");
        fs::write(root.join("invalid/bad.bin"), vec![0; 8]).unwrap();
        let mut request = request(&root);
        request.caps.maximum_single_file_bytes = 6;

        let result = scan_global_root(&request, &CancellationFlag::default(), |_| {});

        assert_eq!(result.coverage, CoverageState::Complete);
        assert_eq!(result.observations.len(), 2);
        assert!(
            result
                .observations
                .iter()
                .any(|item| item.status.is_verified())
        );
        assert!(
            result
                .observations
                .iter()
                .any(|item| item.status == ScanObservationStatus::UnsupportedBundle)
        );
        assert_eq!(result.diagnostics.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_candidate_remains_a_visible_observation() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("skills");
        fs::create_dir(&root).unwrap();
        let unreadable = root.join("unreadable");
        write_skill(&unreadable, "unreadable");
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();

        let result = scan_global_root(&request(&root), &CancellationFlag::default(), |_| {});

        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(result.coverage, CoverageState::Complete);
        assert_eq!(result.observations.len(), 1);
        assert_eq!(
            result.observations[0].status,
            ScanObservationStatus::PermissionDenied
        );
        assert_eq!(
            result.observations[0].error.as_ref().unwrap().code,
            "permission_denied"
        );
        assert_eq!(result.diagnostics.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn invalid_manifest_type_replaces_verified_evidence_with_a_visible_error() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("skills");
        fs::create_dir(&root).unwrap();
        let skill = root.join("changed");
        write_skill(&skill, "valid");
        assert_eq!(
            scan_global_root(&request(&root), &CancellationFlag::default(), |_| {}).observations[0]
                .status,
            ScanObservationStatus::Verified
        );
        fs::remove_file(skill.join("SKILL.md")).unwrap();
        fs::write(skill.join("real.md"), "invalid replacement").unwrap();
        symlink("real.md", skill.join("SKILL.md")).unwrap();

        let result = scan_global_root(&request(&root), &CancellationFlag::default(), |_| {});

        assert_eq!(result.coverage, CoverageState::Complete);
        assert_eq!(result.observations.len(), 1);
        assert_eq!(
            result.observations[0].status,
            ScanObservationStatus::UnsupportedBundle
        );
        assert_eq!(
            result.observations[0].error.as_ref().unwrap().code,
            "invalid_skill_manifest_type"
        );
    }

    #[test]
    fn path_identity_preserves_case_for_distinct_case_sensitive_candidates() {
        let root = Path::new("/skills");
        let (_, upper) = candidate_identity(root, Path::new("/skills/Alpha")).unwrap();
        let (_, lower) = candidate_identity(root, Path::new("/skills/alpha")).unwrap();

        assert_ne!(upper, lower);
    }

    #[cfg(unix)]
    #[test]
    fn unknown_links_are_never_traversed_and_managed_links_require_exact_evidence() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("skills");
        let outside = directory.path().join("outside");
        let vault = directory.path().join("vault-working");
        fs::create_dir(&root).unwrap();
        write_skill(&outside, "outside");
        write_skill(&vault, "vault");
        write_skill(&root.join("inside"), "inside");
        symlink(&outside, root.join("outside-link")).unwrap();
        symlink("inside", root.join("inside-link")).unwrap();
        symlink("missing", root.join("broken-link")).unwrap();
        symlink(&vault, root.join("managed-link")).unwrap();

        let mut request = request(&root);
        let managed_path = normalized_path_identity(
            root.canonicalize()
                .unwrap()
                .join("managed-link")
                .to_str()
                .unwrap(),
        );
        request.managed_links.insert(
            managed_path,
            ManagedLinkExpectation {
                skill_id: SkillId::generate(),
                raw_target: vault.clone(),
                resolved_target: vault,
            },
        );

        let result = scan_global_root(&request, &CancellationFlag::default(), |_| {});
        let statuses = result
            .observations
            .iter()
            .map(|item| item.status)
            .collect::<BTreeSet<_>>();

        assert!(statuses.contains(&ScanObservationStatus::SymlinkOutsideAuthorizedRoot));
        assert!(statuses.contains(&ScanObservationStatus::UnknownSymlink));
        assert!(statuses.contains(&ScanObservationStatus::BrokenOrCyclicLink));
        assert!(statuses.contains(&ScanObservationStatus::ManagedLinkVerified));
        let expected_digest = hash_bundle(
            directory.path().join("vault-working").as_path(),
            BundleCaps::default(),
        )
        .unwrap()
        .digest;
        assert_eq!(
            result
                .observations
                .iter()
                .find(|item| item.status == ScanObservationStatus::ManagedLinkVerified)
                .unwrap()
                .digest,
            Some(expected_digest)
        );
    }

    #[test]
    fn cancellation_stops_between_child_boundaries() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("skills");
        fs::create_dir(&root).unwrap();
        for name in ["a", "b", "c"] {
            write_skill(&root.join(name), name);
        }
        let cancellation = CancellationFlag::default();
        let callback_flag = cancellation.clone();

        let result = scan_global_root(&request(&root), &cancellation, |progress| {
            if progress.completed_entries == 1 {
                callback_flag.cancel();
            }
        });

        assert_eq!(result.coverage, CoverageState::Cancelled);
        assert_eq!(result.completed_entries, 1);
        assert_eq!(result.observations.len(), 1);
    }
}
