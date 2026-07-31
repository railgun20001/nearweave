use tauri::AppHandle;

use crate::{
    error::{AppError, AppResult},
    models::NearbyDevice,
    state::{AppState, ReconnectTarget},
};
use uuid::Uuid;

pub struct ListenerHandle;

pub async fn scan_bluetooth_devices() -> AppResult<Vec<NearbyDevice>> {
    Err(AppError::Unsupported(
        "当前版本仅实现 Windows RFCOMM 传输适配器".into(),
    ))
}

pub async fn start_bluetooth_listener(_app: AppHandle, _state: AppState) -> AppResult<()> {
    Err(AppError::Unsupported(
        "当前版本仅实现 Windows RFCOMM 传输适配器".into(),
    ))
}

pub async fn stop_bluetooth_listener(_state: &AppState) -> AppResult<()> {
    Ok(())
}

pub(crate) async fn connect_bluetooth_target(
    _app: AppHandle,
    _state: AppState,
    _target: ReconnectTarget,
    _reconnect_session: Uuid,
) -> AppResult<()> {
    Err(AppError::Unsupported(
        "当前版本仅实现 Windows RFCOMM 传输适配器".into(),
    ))
}
