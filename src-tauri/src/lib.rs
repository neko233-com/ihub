mod app;
mod background_process;
mod builtin_tools;
mod clipboard_access;
mod clipboard_history;
mod cloud_credentials;
mod cloud_drive;
mod detached_plugin_window;
mod host_log;
mod hosts_manager;
mod indexer;
mod lan_share;
mod launcher_hotkey;
mod launcher_shortcuts;
mod models;
mod native_color_picker;
mod native_icons;
mod native_screenshot;
mod network_diagnostics;
mod ntfs_usn;
mod ocr;
mod plugin_artwork;
mod plugin_asset_server;
mod plugin_crypto_storage;
mod plugin_settings;
mod plugin_shortcuts;
mod plugins;
mod project_template;
mod super_panel;
pub mod system_open;
mod utools_browser_window;
mod utools_db;
mod utools_drag;
mod utools_foreground;
mod utools_screen;
mod utools_ubrowser;
mod wifi_profiles;
mod window_management;

pub fn run() {
    if let Some(code) = hosts_manager::elevated_helper_exit_code_from_args() {
        std::process::exit(code);
    }
    if let Some(code) = wifi_profiles::elevated_helper_exit_code_from_args() {
        std::process::exit(code);
    }
    app::run();
}
