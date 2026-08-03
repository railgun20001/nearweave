use std::path::PathBuf;

use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;
use uuid::Uuid;

use crate::{
    files,
    models::{
        AppSnapshot, DirectoryPageView, LocalShare, NoticeLevel, SharedRoot, TransferCancelResult,
    },
    protocol::{CAPABILITY_LAZY_DIRECTORY, Frame, Message},
    state::{AppState, ClipboardCommand},
    transport,
};

type CommandResult<T> = Result<T, String>;

#[tauri::command]
pub fn get_snapshot(state: State<'_, AppState>) -> AppSnapshot {
    state.snapshot()
}

#[tauri::command]
pub async fn refresh_devices(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<Vec<crate::models::NearbyDevice>> {
    transport::refresh_nearby_devices(&app, state.inner())
        .await
        .map_err(|error| error.to_string())?;
    Ok(state.inner.devices.lock().expect("设备列表锁损坏").clone())
}

#[tauri::command]
pub async fn set_connection_service_enabled(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> CommandResult<()> {
    transport::set_receiver_enabled(app, state.inner().clone(), enabled)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn enable_lan(app: AppHandle, state: State<'_, AppState>) -> CommandResult<()> {
    #[cfg(target_os = "windows")]
    {
        tauri::async_runtime::spawn_blocking(crate::installer_helper::ensure_lan_firewall_access)
            .await
            .map_err(|error| format!("等待 Windows 管理员授权失败：{error}"))??;
        transport::enable_lan_transport(app, state.inner().clone())
            .await
            .map_err(|error| error.to_string())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, state);
        Err("当前平台暂不支持自动配置局域网防火墙".into())
    }
}

#[tauri::command]
pub fn dismiss_lan_setup(app: AppHandle, state: State<'_, AppState>) -> CommandResult<()> {
    state
        .dismiss_lan_setup()
        .map_err(|error| format!("保存局域网设置失败：{error}"))?;
    state.emit_snapshot(&app);
    Ok(())
}

#[tauri::command]
pub async fn connect_peer(
    app: AppHandle,
    state: State<'_, AppState>,
    device_id: String,
) -> CommandResult<()> {
    let device = state
        .inner
        .devices
        .lock()
        .expect("设备列表锁损坏")
        .iter()
        .find(|value| value.id == device_id)
        .cloned()
        .ok_or_else(|| "目标设备不在本次扫描结果中，请重新扫描".to_string())?;
    let lan_available = state.lan_enabled() && device.lan_endpoint.is_some();
    if !lan_available && !device.nearweave_enabled {
        return Err(
            "对方当前没有可用的 NearWeave 蓝牙连接；如需局域网直连，请先在设置中启用局域网传输"
                .into(),
        );
    }
    transport::connect_peer(app, state.inner().clone(), device)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn connect_by_ip(
    app: AppHandle,
    state: State<'_, AppState>,
    endpoint: String,
) -> CommandResult<()> {
    if !state.lan_enabled() {
        return Err("请先在设置中启用局域网传输".into());
    }
    transport::connect_by_ip(app, state.inner().clone(), endpoint)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn confirm_pairing(
    app: AppHandle,
    state: State<'_, AppState>,
    request_id: Uuid,
) -> CommandResult<()> {
    state
        .resolve_pairing(request_id, true)
        .map_err(|error| error.to_string())?;
    state.emit_snapshot(&app);
    Ok(())
}

#[tauri::command]
pub fn reject_pairing(
    app: AppHandle,
    state: State<'_, AppState>,
    request_id: Uuid,
) -> CommandResult<()> {
    state
        .resolve_pairing(request_id, false)
        .map_err(|error| error.to_string())?;
    state.emit_snapshot(&app);
    Ok(())
}

#[tauri::command]
pub fn remove_trusted_device(
    app: AppHandle,
    state: State<'_, AppState>,
    device_id: Uuid,
) -> CommandResult<()> {
    let removed = state
        .remove_trusted_device(device_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "未找到该信任设备".to_string())?;
    state.disconnect_peer(device_id);
    state.emit_snapshot(&app);
    state.emit_notice(
        &app,
        NoticeLevel::Success,
        format!("已移除对 {removed} 的信任"),
    );
    Ok(())
}

#[tauri::command]
pub fn list_trusted_devices(state: State<'_, AppState>) -> Vec<crate::models::TrustedDeviceView> {
    state.snapshot().trusted_devices
}

#[tauri::command]
pub async fn disconnect_device(
    app: AppHandle,
    state: State<'_, AppState>,
    device_id: Uuid,
) -> CommandResult<()> {
    state.cancel_reconnect_for_peer(device_id);
    if state.connected_peer_ids().contains(&device_id) {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            state.send_frame_to(
                device_id,
                Frame::new(Message::Disconnect {
                    reason: "用户主动断开".into(),
                }),
            ),
        )
        .await;
    }
    state.disconnect_peer(device_id);
    state
        .cleanup_incoming_for_peer(device_id, "设备已断开，未完成的临时文件已删除")
        .await;
    state.emit_snapshot(&app);
    Ok(())
}

#[tauri::command]
pub fn send_files(
    app: AppHandle,
    state: State<'_, AppState>,
    device_id: Uuid,
    paths: Vec<String>,
) -> CommandResult<()> {
    if !state.receiver_enabled() {
        return Err("连接服务已停止".into());
    }
    if paths.is_empty() {
        return Err("至少选择一个文件".into());
    }
    if !state.connected_peer_ids().contains(&device_id) {
        return Err("目标设备尚未连接".into());
    }
    let paths = paths.into_iter().map(PathBuf::from).collect();
    let send_state = state.inner().clone();
    tauri::async_runtime::spawn(files::send_direct_files(app, send_state, device_id, paths));
    Ok(())
}

#[tauri::command]
pub fn add_shared_directory(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> CommandResult<()> {
    if !state.receiver_enabled() {
        return Err("连接服务已停止".into());
    }
    let canonical = PathBuf::from(path)
        .canonicalize()
        .map_err(|error| format!("无法打开共享目录：{error}"))?;
    if !canonical.is_dir() {
        return Err("所选路径不是目录".into());
    }

    {
        let mut shares = state.inner.local_shares.lock().expect("共享目录锁损坏");
        if shares.len() >= 64 {
            return Err("最多同时共享 64 个目录".into());
        }
        if shares.iter().any(|share| share.path == canonical) {
            return Err("该目录已经共享".into());
        }
        let name = canonical
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| canonical.to_string_lossy().into_owned());
        shares.push(LocalShare {
            id: Uuid::new_v4(),
            name,
            path: canonical,
        });
    }
    state.emit_snapshot(&app);
    publish_share_roots_if_connected(state.inner().clone());
    Ok(())
}

#[tauri::command]
pub fn remove_shared_directory(
    app: AppHandle,
    state: State<'_, AppState>,
    share_id: Uuid,
) -> CommandResult<()> {
    let mut shares = state.inner.local_shares.lock().expect("共享目录锁损坏");
    let original_len = shares.len();
    shares.retain(|share| share.id != share_id);
    if shares.len() == original_len {
        return Err("共享目录不存在".into());
    }
    drop(shares);
    state.emit_snapshot(&app);
    publish_share_roots_if_connected(state.inner().clone());
    Ok(())
}

#[tauri::command]
pub async fn refresh_remote_shares(
    state: State<'_, AppState>,
    device_id: Uuid,
) -> CommandResult<()> {
    if !state.receiver_enabled() {
        return Err("连接服务已停止".into());
    }
    if !state.supports_capability(device_id, CAPABILITY_LAZY_DIRECTORY) {
        return Err("对方版本不支持按需目录浏览，请升级 NearWeave".into());
    }
    state.clear_remote_directory_cache(device_id);
    state
        .send_frame_to(
            device_id,
            Frame::new(Message::ShareRootsRequest {
                request_id: Uuid::new_v4(),
            }),
        )
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn list_remote_share_roots(state: State<'_, AppState>, device_id: Uuid) -> Vec<SharedRoot> {
    state.remote_roots(device_id)
}

#[tauri::command]
pub async fn list_remote_directory(
    state: State<'_, AppState>,
    device_id: Uuid,
    share_id: Uuid,
    relative_path: String,
    offset: u32,
) -> CommandResult<DirectoryPageView> {
    if !state.receiver_enabled() {
        return Err("连接服务已停止".into());
    }
    files::request_directory_page(state.inner(), device_id, share_id, relative_path, offset)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn download_shared_file(
    state: State<'_, AppState>,
    device_id: Uuid,
    share_id: Uuid,
    relative_path: String,
) -> CommandResult<()> {
    if !state.receiver_enabled() {
        return Err("连接服务已停止".into());
    }
    let allowed = state
        .remote_roots(device_id)
        .iter()
        .any(|root| root.id == share_id);
    if !allowed {
        return Err("该共享目录已经撤销或尚未加载".into());
    }
    state
        .send_frame_to(
            device_id,
            Frame::new(Message::ShareFileRequest {
                request_id: Uuid::new_v4(),
                share_id,
                relative_path,
            }),
        )
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn cancel_transfer(
    app: AppHandle,
    state: State<'_, AppState>,
    device_id: Uuid,
    transfer_id: Uuid,
) -> CommandResult<TransferCancelResult> {
    files::cancel_transfer(
        &app,
        state.inner(),
        device_id,
        transfer_id,
        "用户主动取消".into(),
    )
    .await
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn set_clipboard_enabled(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> CommandResult<()> {
    state
        .set_clipboard_enabled(enabled)
        .map_err(|error| format!("保存剪贴板设置失败：{error}"))?;
    if enabled {
        for peer_device_id in state.connected_peer_ids() {
            state.send_clipboard_command(ClipboardCommand::SyncPeer(peer_device_id));
        }
    }
    state.emit_snapshot(&app);
    state.emit_notice(
        &app,
        NoticeLevel::Info,
        if enabled {
            "文本剪贴板同步已开启"
        } else {
            "文本剪贴板同步已暂停"
        },
    );
    Ok(())
}

#[tauri::command]
pub fn set_autostart_enabled(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> CommandResult<()> {
    #[cfg(target_os = "windows")]
    {
        use tauri_plugin_autostart::ManagerExt;

        let manager = app.autolaunch();
        if enabled {
            manager.enable()
        } else {
            manager.disable()
        }
        .map_err(|error| format!("更新开机自启失败：{error}"))?;

        *state
            .inner
            .autostart_enabled
            .lock()
            .expect("开机自启状态锁损坏") = manager
            .is_enabled()
            .map_err(|error| format!("读取开机自启状态失败：{error}"))?;
        state.emit_snapshot(&app);
        state.emit_notice(
            &app,
            NoticeLevel::Success,
            if enabled {
                "已开启开机自启动"
            } else {
                "已关闭开机自启动"
            },
        );
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, state, enabled);
        Err("当前平台暂不支持开机自启动设置".into())
    }
}

#[tauri::command]
pub fn remove_transfer(
    app: AppHandle,
    state: State<'_, AppState>,
    transfer_id: Uuid,
) -> CommandResult<()> {
    let name = state
        .remove_transfer(transfer_id)
        .map_err(|error| error.to_string())?;
    state.emit_snapshot(&app);
    state.emit_notice(
        &app,
        NoticeLevel::Success,
        format!("已删除传输记录：{name}"),
    );
    Ok(())
}

#[tauri::command]
pub fn clear_transfer_history(app: AppHandle, state: State<'_, AppState>) -> CommandResult<usize> {
    let removed = state.clear_transfer_history();
    state.emit_snapshot(&app);
    state.emit_notice(
        &app,
        if removed > 0 {
            NoticeLevel::Success
        } else {
            NoticeLevel::Info
        },
        if removed > 0 {
            format!("已清理 {removed} 条传输记录")
        } else {
            "没有可清理的已完成或失败记录".into()
        },
    );
    Ok(removed)
}

#[tauri::command]
pub fn open_receive_directory(app: AppHandle, state: State<'_, AppState>) -> CommandResult<()> {
    std::fs::create_dir_all(&state.inner.receive_directory)
        .map_err(|error| format!("无法创建接收目录：{error}"))?;
    app.opener()
        .open_path(
            state.inner.receive_directory.to_string_lossy(),
            None::<String>,
        )
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn open_legacy_receive_directory(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    let directory = state
        .inner
        .legacy_receive_directory
        .as_ref()
        .filter(|path| path.is_dir())
        .ok_or_else(|| "旧版接收目录已不存在".to_string())?;
    app.opener()
        .open_path(directory.to_string_lossy(), None::<String>)
        .map_err(|error| error.to_string())
}

fn publish_share_roots_if_connected(state: AppState) {
    for device_id in state.connected_peer_ids() {
        if state.supports_capability(device_id, CAPABILITY_LAZY_DIRECTORY) {
            let send_state = state.clone();
            tauri::async_runtime::spawn(async move {
                let _ = files::send_share_roots(&send_state, device_id, None).await;
            });
        }
    }
}
