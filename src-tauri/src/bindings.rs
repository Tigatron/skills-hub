//! Rust-authored Tauri command and TypeScript binding registry.

use std::path::Path;

use specta_typescript::Typescript;
use tauri_specta::{Builder, collect_commands, collect_events};

pub(crate) fn builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new()
        .commands(collect_commands![
            crate::commands::bootstrap::bootstrap_get_state,
            crate::commands::bootstrap::vault_initialize,
            crate::commands::bootstrap::vault_status,
            crate::commands::bootstrap::startup_recovery_run,
            crate::commands::bootstrap::startup_recovery_status,
            crate::commands::scanning::scan_start,
            crate::commands::scanning::scan_all_global,
            crate::commands::scanning::adapters_list,
            crate::commands::scanning::scan_get,
            crate::commands::scanning::scan_cancel,
            crate::commands::scanning::library_list,
            crate::commands::deployment::target_register_fixture,
            crate::commands::deployment::adapters_configured_list,
            crate::commands::deployment::adapter_configure,
            crate::commands::deployment::custom_target_register,
            crate::commands::deployment::adapter_project_target_register,
            crate::commands::deployment::targets_list,
            crate::commands::deployment::deployment_plan,
            crate::commands::deployment::batch_deployment_plan,
            crate::commands::deployment::deployment_undo_plan,
            crate::commands::deployment::operation_undo_plan,
            crate::commands::deployment::undeploy_plan,
            crate::commands::deployment::deployment_verify,
            crate::commands::deployment::deployments_list,
            crate::commands::takeover::takeover_keep_external,
            crate::commands::takeover::takeover_plan,
            crate::commands::takeover::operation_execute,
            crate::commands::takeover::operation_cancel,
            crate::commands::takeover::operation_get,
            crate::commands::takeover::operation_plan_export,
            crate::commands::takeover::skill_get,
            crate::commands::takeover::skill_preview_file,
            crate::commands::trash::trash_move_plan,
            crate::commands::trash::trash_restore_plan,
            crate::commands::trash::trash_permanent_delete_plan,
            crate::commands::trash::trash_move_execute,
            crate::commands::trash::trash_restore_execute,
            crate::commands::trash::trash_permanent_delete_execute,
            crate::commands::trash::trash_entries_list,
            crate::commands::trash::trash_entry_get,
            crate::commands::trash::trash_retention_summary,
            crate::commands::activity::activity_list,
            crate::commands::activity::activity_detail,
            crate::commands::vault_lifecycle::vault_reconcile_working,
            crate::commands::vault_lifecycle::vault_verify,
            crate::commands::vault_lifecycle::vault_repair_plan,
            crate::commands::vault_lifecycle::vault_repair_execute,
            crate::commands::vault_lifecycle::vault_index_rebuild_plan,
            crate::commands::vault_lifecycle::vault_index_rebuild_execute,
            crate::commands::vault_lifecycle::vault_object_gc_plan,
            crate::commands::vault_lifecycle::vault_object_gc_execute,
            crate::commands::vault_lifecycle::vault_object_gc_settings,
            crate::commands::vault_lifecycle::vault_reveal_working,
            crate::commands::vault_lifecycle::vault_relocate_plan,
            crate::commands::vault_lifecycle::vault_relocate_execute,
            crate::commands::vault_lifecycle::vault_old_cleanup_plan,
            crate::commands::vault_lifecycle::vault_old_cleanup_execute,
            crate::commands::workspaces::workspace_root_add,
            crate::commands::workspaces::workspace_root_update,
            crate::commands::workspaces::workspace_root_pause,
            crate::commands::workspaces::workspace_root_remove,
            crate::commands::workspaces::workspace_root_rescan,
            crate::commands::workspaces::workspace_roots_list,
            crate::commands::workspaces::manual_project_add,
            crate::commands::workspaces::manual_projects_list,
            crate::commands::workspaces::manual_project_rescan
        ])
        .events(collect_events![
            crate::application::scanning::ScanProgress,
            crate::application::scanning::DomainInvalidated,
            crate::application::workspaces::WorkspaceProjectBatchEvent
        ])
}

/// Exports the committed TypeScript command client used by the renderer.
///
/// # Errors
///
/// Returns a readable exporter error when a Rust DTO cannot be represented.
pub fn export_typescript_bindings(path: impl AsRef<Path>) -> Result<(), String> {
    builder()
        .export(Typescript::default().header("/* eslint-disable */"), path)
        .map_err(|error| error.to_string())
}
