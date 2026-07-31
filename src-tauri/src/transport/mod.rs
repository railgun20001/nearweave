use std::time::Duration;

use tauri::AppHandle;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{
    error::{AppError, AppResult},
    models::{ConnectionServiceState, NearbyDevice, NoticeLevel},
    protocol::{CAPABILITY_TRANSFER_CANCEL, Frame, Message},
    state::{AppState, ReconnectTarget},
};

mod network;
#[cfg(not(target_os = "windows"))]
mod unsupported;
#[cfg(target_os = "windows")]
mod windows;

pub use network::*;
#[cfg(not(target_os = "windows"))]
pub use unsupported::*;
#[cfg(target_os = "windows")]
pub use windows::*;

#[derive(Debug)]
pub enum TransportCommand {
    Send(Frame),
    Close { reason: Option<String> },
}

#[derive(Clone)]
pub struct TransportSender {
    normal: mpsc::Sender<TransportCommand>,
    control: mpsc::Sender<TransportCommand>,
}

pub struct TransportReceiver {
    normal: mpsc::Receiver<TransportCommand>,
    control: mpsc::Receiver<TransportCommand>,
}

#[derive(Debug)]
pub struct TransportQueueError;

impl TransportSender {
    pub async fn send(
        &self,
        command: TransportCommand,
    ) -> Result<(), mpsc::error::SendError<TransportCommand>> {
        if command.is_high_priority() {
            self.control.send(command).await
        } else {
            self.normal.send(command).await
        }
    }

    pub fn try_send(&self, command: TransportCommand) -> Result<(), TransportQueueError> {
        if command.is_high_priority() {
            self.control
                .try_send(command)
                .map_err(|_| TransportQueueError)
        } else {
            self.normal
                .try_send(command)
                .map_err(|_| TransportQueueError)
        }
    }
}

impl TransportReceiver {
    pub async fn recv(&mut self) -> Option<TransportCommand> {
        tokio::select! {
            biased;
            command = self.control.recv() => {
                match command {
                    Some(command) => Some(command),
                    None => self.normal.recv().await,
                }
            }
            command = self.normal.recv() => command,
        }
    }

    #[cfg(test)]
    pub fn try_recv(&mut self) -> Result<TransportCommand, mpsc::error::TryRecvError> {
        self.control.try_recv().or_else(|error| {
            if matches!(error, mpsc::error::TryRecvError::Empty) {
                self.normal.try_recv()
            } else {
                Err(error)
            }
        })
    }
}

impl TransportCommand {
    fn is_high_priority(&self) -> bool {
        match self {
            Self::Close { .. } => true,
            Self::Send(frame) => matches!(
                frame.message,
                Message::Ping { .. }
                    | Message::Pong { .. }
                    | Message::Disconnect { .. }
                    | Message::TransferCancel { .. }
                    | Message::TransferCancelAck { .. }
            ),
        }
    }
}

pub fn transport_channel(capacity: usize) -> (TransportSender, TransportReceiver) {
    let (normal, normal_receiver) = mpsc::channel(capacity);
    let (control, control_receiver) = mpsc::channel(16);
    (
        TransportSender { normal, control },
        TransportReceiver {
            normal: normal_receiver,
            control: control_receiver,
        },
    )
}

const RECONNECT_DELAYS_SECONDS: [u64; 5] = [1, 2, 5, 10, 30];

pub async fn refresh_nearby_devices(app: &AppHandle, state: &AppState) -> AppResult<()> {
    match scan_bluetooth_devices().await {
        Ok(devices) => state.replace_bluetooth_devices(devices),
        Err(AppError::Unsupported(_)) => {}
        Err(error) => {
            state.emit_notice(
                app,
                NoticeLevel::Info,
                format!("蓝牙设备刷新失败，局域网发现仍可使用：{error}"),
            );
        }
    }
    state.emit_snapshot(app);
    Ok(())
}

pub async fn set_receiver_enabled(app: AppHandle, state: AppState, enabled: bool) -> AppResult<()> {
    let _transition = state.inner.service_transition.lock().await;
    if enabled {
        if matches!(
            state.connection_service_state(),
            ConnectionServiceState::Running | ConnectionServiceState::Starting
        ) {
            return Ok(());
        }
        state.set_connection_service_state(ConnectionServiceState::Starting);
        state.emit_snapshot(&app);
        if state.network_runtime().is_none()
            && let Err(error) = start_network_transport(app.clone(), state.clone()).await
        {
            state.set_network_start_error(error.to_string());
            state.emit_notice(
                &app,
                NoticeLevel::Info,
                format!("局域网服务未开启，蓝牙仍可使用：{error}"),
            );
        }
        match start_bluetooth_listener(app.clone(), state.clone()).await {
            Ok(()) => state.set_bluetooth_error(None),
            Err(error) => {
                state.set_bluetooth_error(Some(error.to_string()));
                state.emit_notice(
                    &app,
                    NoticeLevel::Info,
                    format!("蓝牙接收服务未开启，纯局域网仍可使用：{error}"),
                );
            }
        }
        state.set_connection_service_state(ConnectionServiceState::Running);
        spawn_service_retry(app.clone(), state.clone());
        if state.network_runtime().is_none()
            && state
                .inner
                .listener
                .lock()
                .expect("监听实例锁损坏")
                .is_none()
        {
            state.emit_notice(
                &app,
                NoticeLevel::Error,
                "蓝牙和局域网接收服务均不可用，NearWeave 将继续后台重试",
            );
        }
    } else {
        if matches!(
            state.connection_service_state(),
            ConnectionServiceState::Stopped | ConnectionServiceState::Stopping
        ) {
            return Ok(());
        }
        state.set_connection_service_state(ConnectionServiceState::Stopping);
        state.emit_snapshot(&app);
        state.cancel_service_retry();
        let active_transfers = state.active_transfer_keys();
        state.cancel_all_transfers();
        state.cancel_reconnect();
        state.cancel_all_pairings();
        state.cancel_all_directory_requests().await;
        state.cancel_all_transfer_acks();
        for device_id in state.connected_peer_ids() {
            if state.supports_capability(device_id, CAPABILITY_TRANSFER_CANCEL) {
                for (_, transfer_id) in active_transfers
                    .iter()
                    .filter(|(peer_device_id, _)| *peer_device_id == device_id)
                {
                    let _ = state.try_send_frame_to(
                        device_id,
                        Frame::new(Message::TransferCancel {
                            transfer_id: *transfer_id,
                            reason: "本机正在停止 NearWeave 连接".into(),
                        }),
                    );
                }
            }
            let _ = state.try_send_frame_to(
                device_id,
                Frame::new(Message::Disconnect {
                    reason: "对方已停止 NearWeave 连接".into(),
                }),
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        state.disconnect();
        state
            .cancel_all_incoming("连接服务已停止，未完成的临时文件已删除")
            .await;
        state.mark_all_transfers_canceled("连接服务已停止，任务已取消");
        if let Err(error) = stop_bluetooth_listener(&state).await {
            state.set_bluetooth_error(Some(error.to_string()));
        } else {
            state.set_bluetooth_error(None);
        }
        stop_network_transport(&state).await;
        state.set_connection_service_state(ConnectionServiceState::Stopped);
    }
    state.emit_snapshot(&app);
    Ok(())
}

fn spawn_service_retry(app: AppHandle, state: AppState) {
    let token = state.begin_service_retry();
    tauri::async_runtime::spawn(async move {
        let mut attempt = 0_usize;
        loop {
            tokio::time::sleep(reconnect_delay(attempt)).await;
            if !state.service_retry_active(token) {
                return;
            }
            let _transition = state.inner.service_transition.lock().await;
            if !state.service_retry_active(token) {
                return;
            }

            let bluetooth_missing = state
                .inner
                .listener
                .lock()
                .expect("监听实例锁损坏")
                .is_none();
            if bluetooth_missing {
                match start_bluetooth_listener(app.clone(), state.clone()).await {
                    Ok(()) => state.set_bluetooth_error(None),
                    Err(error) => state.set_bluetooth_error(Some(error.to_string())),
                }
            }

            let discovery_unavailable = state
                .inner
                .discovery_error
                .lock()
                .expect("局域网发现状态锁损坏")
                .is_some();
            if state.network_runtime().is_none() {
                match start_network_transport(app.clone(), state.clone()).await {
                    Ok(()) => {}
                    Err(error) => state.set_network_start_error(error.to_string()),
                }
            } else if discovery_unavailable {
                let _ = retry_network_discovery(app.clone(), state.clone()).await;
            }
            state.emit_snapshot(&app);

            let has_component_error = state
                .inner
                .bluetooth_error
                .lock()
                .expect("蓝牙服务状态锁损坏")
                .is_some()
                || state
                    .inner
                    .discovery_error
                    .lock()
                    .expect("局域网发现状态锁损坏")
                    .is_some()
                || state
                    .inner
                    .tcp_error
                    .lock()
                    .expect("局域网接收状态锁损坏")
                    .is_some();
            attempt = if has_component_error {
                attempt.saturating_add(1)
            } else {
                RECONNECT_DELAYS_SECONDS.len() - 1
            };
        }
    });
}

pub async fn connect_peer(app: AppHandle, state: AppState, device: NearbyDevice) -> AppResult<()> {
    let target = ReconnectTarget {
        device_id: device.device_id,
        display_name: device.name,
        lan_endpoint: device
            .lan_endpoint
            .as_deref()
            .and_then(|value| value.parse().ok()),
        bluetooth_endpoint: device.bluetooth_endpoint,
    };
    connect_new_session(app, state, target).await
}

pub async fn connect_by_ip(app: AppHandle, state: AppState, endpoint: String) -> AppResult<()> {
    let target = resolve_manual_target(&state, &endpoint).await?;
    connect_new_session(app, state, target).await
}

async fn connect_new_session(
    app: AppHandle,
    state: AppState,
    target: ReconnectTarget,
) -> AppResult<()> {
    if !state.receiver_enabled() {
        return Err(AppError::InvalidInput(
            "NearWeave 连接已停止，请先恢复连接".into(),
        ));
    }
    if target
        .device_id
        .is_some_and(|device_id| state.connection_generation_for_peer(device_id).is_some())
    {
        return Err(AppError::InvalidInput("该设备已经连接".into()));
    }
    let session = state.begin_reconnect_session(target.clone());
    match connect_target(app.clone(), state.clone(), target, session).await {
        Ok(()) => Ok(()),
        Err(error) => {
            state.cancel_reconnect_session(session);
            state.emit_snapshot(&app);
            Err(error)
        }
    }
}

async fn connect_target(
    app: AppHandle,
    state: AppState,
    mut target: ReconnectTarget,
    session: Uuid,
) -> AppResult<()> {
    if !state.is_reconnect_session_active(session) {
        return Err(AppError::NotConnected);
    }
    if let Some(device_id) = target.device_id
        && let Some(device) = state.find_device(&device_id.to_string())
    {
        target.display_name = device.name;
        target.lan_endpoint = device
            .lan_endpoint
            .as_deref()
            .and_then(|value| value.parse().ok())
            .or(target.lan_endpoint);
        target.bluetooth_endpoint = device.bluetooth_endpoint.or(target.bluetooth_endpoint);
    }

    let mut lan_error = None;
    if target.lan_endpoint.is_some() {
        match connect_lan_target(app.clone(), state.clone(), target.clone(), session).await {
            Ok(()) => return Ok(()),
            Err(error) => lan_error = Some(error),
        }
    }
    if target.bluetooth_endpoint.is_some() {
        return connect_bluetooth_target(app, state, target, session).await;
    }
    Err(lan_error
        .unwrap_or_else(|| AppError::InvalidInput("设备当前没有可用的局域网或蓝牙连接地址".into())))
}

pub(crate) async fn finish_primary_connection(
    app: &AppHandle,
    state: &AppState,
    generation: Uuid,
    failure: Option<String>,
) {
    if let Some(closed) = state.clear_connection(generation) {
        if let Some(peer_device_id) = closed.peer_device_id {
            state
                .cleanup_incoming_for_peer(peer_device_id, "连接中断，未完成的临时文件已删除")
                .await;
        }
        state.emit_snapshot(app);
        if let Some(failure) = failure {
            state.emit_notice(app, NoticeLevel::Info, format!("连接已断开：{failure}"));
        } else {
            state.emit_notice(app, NoticeLevel::Info, "连接已断开");
        }
        if let Some(session) = closed
            .reconnect_session
            .filter(|session| state.is_reconnect_session_active(*session))
        {
            spawn_reconnect(app.clone(), state.clone(), session);
        }
    }
}

fn spawn_reconnect(app: AppHandle, state: AppState, session: Uuid) {
    if !state.start_reconnect_task(session) {
        return;
    }
    state.emit_snapshot(&app);

    tauri::async_runtime::spawn(async move {
        let mut attempt = 0_usize;
        loop {
            let Some(target) = state.reconnect_target(session) else {
                return;
            };
            let delay = reconnect_delay(attempt);
            state.emit_notice(
                &app,
                NoticeLevel::Info,
                format!(
                    "{} 秒后尝试重新连接 {}",
                    delay.as_secs(),
                    target.display_name
                ),
            );
            tokio::time::sleep(delay).await;
            if !state.is_reconnect_session_active(session) {
                return;
            }
            match connect_target(app.clone(), state.clone(), target.clone(), session).await {
                Ok(()) => {
                    state.emit_notice(
                        &app,
                        NoticeLevel::Success,
                        format!("已恢复与 {} 的连接", target.display_name),
                    );
                    return;
                }
                Err(error) => {
                    if !state.is_reconnect_session_active(session) {
                        return;
                    }
                    state.emit_notice(&app, NoticeLevel::Info, format!("自动重连失败：{error}"));
                    attempt = attempt.saturating_add(1);
                }
            }
        }
    });
}

fn reconnect_delay(attempt: usize) -> Duration {
    Duration::from_secs(RECONNECT_DELAYS_SECONDS[attempt.min(RECONNECT_DELAYS_SECONDS.len() - 1)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_delay_increases_and_caps_at_thirty_seconds() {
        let seconds = (0..7)
            .map(|attempt| reconnect_delay(attempt).as_secs())
            .collect::<Vec<_>>();
        assert_eq!(seconds, vec![1, 2, 5, 10, 30, 30, 30]);
    }

    #[test]
    fn control_messages_jump_ahead_of_bulk_frames() {
        let (sender, mut receiver) = transport_channel(1);
        sender
            .try_send(TransportCommand::Send(Frame::with_payload(
                Message::FileChunk {
                    transfer_id: Uuid::nil(),
                    offset: 0,
                },
                vec![1],
            )))
            .expect("应加入普通队列");
        sender
            .try_send(TransportCommand::Send(Frame::new(
                Message::TransferCancel {
                    transfer_id: Uuid::nil(),
                    reason: "测试".into(),
                },
            )))
            .expect("应加入控制队列");

        assert!(matches!(
            receiver.try_recv(),
            Ok(TransportCommand::Send(Frame {
                message: Message::TransferCancel { .. },
                ..
            }))
        ));
    }
}
