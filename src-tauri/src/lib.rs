mod clipboard;
mod commands;
mod error;
mod files;
mod handlers;
mod identity;
#[cfg(target_os = "windows")]
mod installer_helper;
mod models;
mod protocol;
mod settings;
mod state;
mod transport;

use std::{fs, path::Path};

use state::{AppState, AppStateConfig, ClipboardCommand};
use tauri::Manager;
use uuid::Uuid;

#[cfg(target_os = "windows")]
pub fn run_installer_helper_from_args() -> Option<i32> {
    installer_helper::run_from_args()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let application = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            let app_data = app.path().app_data_dir()?;
            fs::create_dir_all(&app_data)?;
            let device_id = load_or_create_device_id(&app_data.join("device-id"))?;
            let device_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "此电脑".into());
            let settings_path = app_data.join("settings.json");
            let settings = settings::load(&settings_path);
            let identity = identity::load_or_create_identity(&app_data.join("identity.bin"))?;
            let trust_path = app_data.join("trusted-devices.json");
            let trusted_devices = identity::load_trusted_devices(&trust_path);
            let receive_directory = dirs::download_dir()
                .unwrap_or_else(|| app_data.clone())
                .join("NearWeave Received");
            #[cfg(target_os = "windows")]
            let autostart_enabled = {
                use tauri_plugin_autostart::ManagerExt;
                app.autolaunch().is_enabled().unwrap_or(false)
            };
            #[cfg(not(target_os = "windows"))]
            let autostart_enabled = false;
            #[cfg(target_os = "windows")]
            let lan_enabled =
                settings.lan_enabled && installer_helper::lan_firewall_is_configured();
            #[cfg(not(target_os = "windows"))]
            let lan_enabled = false;
            let state = AppState::new(AppStateConfig {
                device_id,
                device_name,
                receive_directory,
                settings_path,
                trust_path,
                identity,
                trusted_devices,
                clipboard_enabled: settings.clipboard_enabled,
                autostart_enabled,
                lan_enabled,
                lan_setup_decided: settings.lan_setup_decided,
            });
            app.manage(state.clone());
            if let Err(error) = tauri::async_runtime::block_on(transport::set_receiver_enabled(
                app.handle().clone(),
                state.clone(),
                true,
            )) {
                state.emit_notice(
                    app.handle(),
                    models::NoticeLevel::Info,
                    format!("恢复 NearWeave 连接状态失败：{error}"),
                );
            }
            clipboard::start_clipboard_worker(app.handle().clone(), state.clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::refresh_devices,
            commands::set_connection_service_enabled,
            commands::connect_peer,
            commands::connect_by_ip,
            commands::confirm_pairing,
            commands::reject_pairing,
            commands::list_trusted_devices,
            commands::remove_trusted_device,
            commands::disconnect_device,
            commands::send_files,
            commands::add_shared_directory,
            commands::remove_shared_directory,
            commands::refresh_remote_shares,
            commands::list_remote_share_roots,
            commands::list_remote_directory,
            commands::download_shared_file,
            commands::cancel_transfer,
            commands::set_clipboard_enabled,
            commands::set_autostart_enabled,
            commands::enable_lan,
            commands::dismiss_lan_setup,
            commands::remove_transfer,
            commands::clear_transfer_history,
            commands::open_receive_directory,
        ])
        .build(tauri::generate_context!())
        .expect("无法启动 NearWeave 应用");

    application.run(|app_handle, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            let state = app_handle.state::<AppState>();
            let _ = tauri::async_runtime::block_on(transport::set_receiver_enabled(
                app_handle.clone(),
                state.inner().clone(),
                false,
            ));
            state.send_clipboard_command(ClipboardCommand::Stop);
        }
    });
}

fn load_or_create_device_id(path: &Path) -> Result<Uuid, Box<dyn std::error::Error>> {
    if let Ok(value) = fs::read_to_string(path)
        && let Ok(device_id) = Uuid::parse_str(value.trim())
    {
        return Ok(device_id);
    }
    let device_id = Uuid::new_v4();
    fs::write(path, device_id.to_string())?;
    Ok(device_id)
}
