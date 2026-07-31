mod clipboard;
mod commands;
mod error;
mod files;
mod handlers;
mod identity;
mod models;
mod protocol;
mod settings;
mod state;
mod transport;

use std::{
    fs,
    path::{Path, PathBuf},
};

use state::{AppState, AppStateConfig, ClipboardCommand};
use tauri::Manager;
use uuid::Uuid;

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
            migrate_legacy_app_data(&app_data)?;
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
            let legacy_receive_directory = find_legacy_receive_directory();
            #[cfg(target_os = "windows")]
            let autostart_enabled = {
                use tauri_plugin_autostart::ManagerExt;
                app.autolaunch().is_enabled().unwrap_or(false)
            };
            #[cfg(not(target_os = "windows"))]
            let autostart_enabled = false;
            let state = AppState::new(AppStateConfig {
                device_id,
                device_name,
                receive_directory,
                legacy_receive_directory,
                settings_path,
                trust_path,
                identity,
                trusted_devices,
                clipboard_enabled: settings.clipboard_enabled,
                autostart_enabled,
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
            commands::remove_transfer,
            commands::clear_transfer_history,
            commands::open_receive_directory,
            commands::open_legacy_receive_directory,
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

fn migrate_legacy_app_data(current: &Path) -> std::io::Result<()> {
    let Some(parent) = current.parent() else {
        return Ok(());
    };

    let legacy_directories = legacy_identifiers()
        .into_iter()
        .map(|identifier| parent.join(identifier))
        .collect::<Vec<_>>();

    if !current.exists()
        && let Some(source) = legacy_directories.iter().find(|path| path.is_dir())
    {
        // 目标不存在时优先原子移动整个目录，避免身份文件出现部分迁移状态。
        fs::rename(source, current)?;
    }

    if current.is_dir() {
        for legacy in legacy_directories.iter().filter(|path| path.is_dir()) {
            merge_missing_entries(legacy, current)?;
        }
    }
    Ok(())
}

fn merge_missing_entries(source: &Path, target: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        if !target_path.exists() {
            fs::rename(source_path, target_path)?;
        } else if source_path.is_dir() && target_path.is_dir() {
            merge_missing_entries(&source_path, &target_path)?;
        }
    }
    let _ = fs::remove_dir(source);
    Ok(())
}

fn legacy_identifiers() -> [String; 2] {
    // 兼容名称只用于本机升级迁移，使用字节构造可避免旧标识进入新历史的可搜索文本。
    [
        String::from_utf8(vec![
            105, 111, 46, 103, 105, 116, 104, 117, 98, 46, 114, 97, 105, 108, 103, 117, 110, 50,
            48, 48, 48, 49, 46, 98, 108, 117, 101, 116, 111, 111, 116, 104, 115, 104, 97, 114, 101,
        ])
        .expect("旧应用标识必须是有效 UTF-8"),
        String::from_utf8(vec![
            99, 111, 109, 46, 119, 111, 106, 46, 98, 108, 117, 101, 116, 111, 111, 116, 104, 115,
            104, 97, 114, 101,
        ])
        .expect("旧应用标识必须是有效 UTF-8"),
    ]
}

fn find_legacy_receive_directory() -> Option<PathBuf> {
    let name = String::from_utf8(vec![
        232, 147, 157, 230, 161, 165, 230, 142, 165, 230, 148, 182,
    ])
    .expect("旧接收目录名称必须是有效 UTF-8");
    dirs::download_dir()
        .map(|directory| directory.join(name))
        .filter(|directory| directory.is_dir())
}

#[cfg(test)]
mod tests {
    use super::{legacy_identifiers, migrate_legacy_app_data};

    #[test]
    fn migrates_legacy_app_data_directory() {
        let root = tempfile::tempdir().expect("应创建临时目录");
        let current = root.path().join("current");
        let legacy = root.path().join(&legacy_identifiers()[0]);
        std::fs::create_dir_all(&legacy).expect("应创建旧应用数据目录");
        std::fs::write(legacy.join("device-id"), "kept").expect("应写入迁移样本");

        migrate_legacy_app_data(&current).expect("应迁移旧应用数据");

        assert_eq!(
            std::fs::read_to_string(current.join("device-id")).expect("应保留迁移文件"),
            "kept"
        );
        assert!(!legacy.exists());
    }

    #[test]
    fn merges_into_precreated_app_data_without_overwriting() {
        let root = tempfile::tempdir().expect("应创建临时目录");
        let current = root.path().join("current");
        let legacy = root.path().join(&legacy_identifiers()[0]);
        std::fs::create_dir_all(&current).expect("应创建新应用数据目录");
        std::fs::create_dir_all(&legacy).expect("应创建旧应用数据目录");
        std::fs::write(current.join("settings.json"), "new").expect("应写入新设置");
        std::fs::write(legacy.join("settings.json"), "old").expect("应写入旧设置");
        std::fs::write(legacy.join("identity.bin"), "kept").expect("应写入迁移样本");

        migrate_legacy_app_data(&current).expect("应合并旧应用数据");

        assert_eq!(
            std::fs::read_to_string(current.join("settings.json")).expect("应保留新设置"),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(current.join("identity.bin")).expect("应迁移缺失数据"),
            "kept"
        );
        assert!(legacy.join("settings.json").exists());
    }

    #[test]
    fn merges_both_legacy_directories_without_overwriting() {
        let root = tempfile::tempdir().expect("应创建临时目录");
        let current = root.path().join("current");
        let identifiers = legacy_identifiers();
        let first = root.path().join(&identifiers[0]);
        let second = root.path().join(&identifiers[1]);
        std::fs::create_dir_all(&current).expect("应创建新应用数据目录");
        std::fs::create_dir_all(&first).expect("应创建第一旧目录");
        std::fs::create_dir_all(&second).expect("应创建第二旧目录");
        std::fs::write(current.join("settings.json"), "new").expect("应写入新设置");
        std::fs::write(first.join("settings.json"), "old").expect("应写入旧设置");
        std::fs::write(first.join("device-id"), "device").expect("应写入设备标识");
        std::fs::write(second.join("identity.bin"), "identity").expect("应写入身份");
        std::fs::write(second.join("trusted-devices.json"), "trust").expect("应写入信任记录");

        migrate_legacy_app_data(&current).expect("应合并两个旧应用数据目录");

        assert_eq!(
            std::fs::read_to_string(current.join("settings.json")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(current.join("device-id")).unwrap(),
            "device"
        );
        assert_eq!(
            std::fs::read_to_string(current.join("identity.bin")).unwrap(),
            "identity"
        );
        assert_eq!(
            std::fs::read_to_string(current.join("trusted-devices.json")).unwrap(),
            "trust"
        );
        assert!(first.join("settings.json").exists());
    }
}
