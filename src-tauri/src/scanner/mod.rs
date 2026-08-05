//! Read-only global, Workspace, and reconciliation scanning.

mod global;

pub(crate) use global::{
    CancellationFlag, CoverageState, GlobalScanRequest, GlobalScanResult, ManagedLinkExpectation,
    ScanDiagnostic, ScanProgress, scan_global_root,
};
