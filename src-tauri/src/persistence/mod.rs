//! `SQLite` repositories, migrations, readable manifests, and Vault lifecycle.

mod executor;
mod manifests;
mod migrations;
mod repositories;
mod vault;

pub use executor::{DbExecutor, DbExecutorError};
pub use manifests::{
    Appearance, DeploymentManifest, DeviceSettings, LocalSourceKind, ManifestError, ManifestStore,
    SkillManifest, SkillManifestProvenance, SkillManifestSource, SourceConfidence,
    TrashEntryManifest, TrashPolicy, VaultManifest, read_trash_entry, write_trash_entry,
};
pub use migrations::{DatabaseSettings, MigrationError, replace_database_file};
pub use repositories::{
    ActivityDetailRecord, ActivityListRecord, ActivityQuery, ActivityRecord,
    AdapterConfigurationRecord, AuthorizationIdentityRecord, BatchDeploymentProjection,
    DeploymentProjection, DeploymentRecord, ExternalObservationRecord, ManagedLinkRecord,
    ObjectRecord, ObservationRecord, OperationRecord, OperationStepRecord, ProjectRecord,
    Repositories, RepositoryError, ScanErrorRecord, ScanReconciliation, ScanRunRecord, SkillRecord,
    SkillRevisionRecord, SkillSourceRecord, SnapshotItemRecord, SnapshotRecord, TakeoverProjection,
    TargetRecord, TargetRegistrationMetadataRecord, WorkspaceCoverageRecord, WorkspaceRootRecord,
};
pub use vault::{
    OpenVault, VaultError, VaultPaths, default_application_support, default_vault_path,
    existing_device_settings,
};
pub(crate) use vault::{update_active_vault_path, update_debug_logging};
