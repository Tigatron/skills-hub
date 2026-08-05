//! Skills Hub desktop backend.

mod adapters;
mod application;
mod bindings;
mod commands;
pub mod domain;
mod error;
pub mod filesystem;
pub mod operations;
pub mod persistence;
mod platform;
mod runtime;
mod scanner;

use runtime::AppRuntime;
use tauri::Manager;

pub use bindings::export_typescript_bindings;

/// Starts the Tauri application with the accepted M0 ownership boundaries.
///
/// # Panics
///
/// Panics when Tauri cannot initialize or run the desktop application.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let specta = bindings::builder();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(
            |app, _arguments, _cwd| {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.unminimize();
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            },
        ))
        .manage(AppRuntime::foundation())
        .invoke_handler(specta.invoke_handler())
        .setup(move |app| {
            specta.mount_events(app);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to run Skills Hub");
}
