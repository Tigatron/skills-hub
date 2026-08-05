//! Read-only global, Workspace, and reconciliation scanning.

mod global;
mod watcher;
mod workspace;

pub(crate) use global::{
    CancellationFlag, CoverageState, GlobalScanRequest, GlobalScanResult, ManagedLinkExpectation,
    ScanDiagnostic, ScanObservation, ScanProgress, scan_global_root,
};
#[allow(unused_imports)]
pub(crate) use watcher::{
    Invalidation, InvalidationKind, NotifyBackend, ReconcileReason, ReconcileRequest, WatchBackend,
    WatchCoordinator, WatchEvent,
};
#[allow(unused_imports)]
pub(crate) use workspace::{
    ManualProject, ProjectBatch, ProjectKind, WorkspaceAdapter, WorkspaceScanRequest,
    WorkspaceScanResult, scan_workspace,
};
