use serde::{Deserialize, Serialize};
use std::time::Instant;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSnapshot {
    pub platform: String,
    pub device_id: Uuid,
    pub device_name: String,
    pub listening: bool,
    pub connection_service_state: ConnectionServiceState,
    pub listener_status: ListenerStatus,
    pub connected: bool,
    pub network_connected: bool,
    pub active_link: ActiveLink,
    pub reconnecting: bool,
    pub peer_name: Option<String>,
    pub connection_kind: Option<ConnectionKind>,
    pub pairing_request: Option<PairingRequestView>,
    pub pairing_requests: Vec<PairingRequestView>,
    pub connections: Vec<PeerConnectionView>,
    pub trusted_devices: Vec<TrustedDeviceView>,
    pub clipboard_enabled: bool,
    pub autostart_enabled: bool,
    pub lan_enabled: bool,
    pub lan_setup_required: bool,
    pub receive_directory: String,
    pub legacy_receive_directory: Option<String>,
    pub devices: Vec<NearbyDevice>,
    pub local_shares: Vec<LocalShareView>,
    pub remote_workspaces: Vec<RemoteWorkspaceView>,
    pub transfers: Vec<TransferView>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionServiceState {
    Starting,
    Running,
    Stopping,
    Stopped,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerConnectionView {
    pub device_id: Uuid,
    pub name: String,
    pub connection_kind: ConnectionKind,
    pub bluetooth_connected: bool,
    pub network_connected: bool,
    pub reconnecting: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveLink {
    None,
    Bluetooth,
    Network,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionKind {
    Bluetooth,
    Lan,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListenerStatus {
    pub bluetooth: ServiceStatus,
    pub discovery: ServiceStatus,
    pub tcp: ServiceStatus,
    pub bluetooth_error: Option<String>,
    pub discovery_error: Option<String>,
    pub tcp_error: Option<String>,
    pub tcp_port: Option<u16>,
    pub local_addresses: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStatus {
    Off,
    Ready,
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NearbyDevice {
    pub id: String,
    pub device_id: Option<Uuid>,
    pub name: String,
    pub bluetooth_endpoint: Option<String>,
    pub lan_endpoint: Option<String>,
    pub bluetooth_paired: bool,
    pub nearweave_enabled: bool,
    pub trusted: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingRequestView {
    pub request_id: Uuid,
    pub device_id: Uuid,
    pub device_name: String,
    pub verification_code: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustedDeviceView {
    pub device_id: Uuid,
    pub name: String,
    pub fingerprint: String,
    pub last_seen_at: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalShareView {
    pub id: Uuid,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct LocalShare {
    pub id: Uuid,
    pub name: String,
    pub path: std::path::PathBuf,
}

impl From<&LocalShare> for LocalShareView {
    fn from(value: &LocalShare) -> Self {
        Self {
            id: value.id,
            name: value.name.clone(),
            path: value.path.to_string_lossy().into_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedRoot {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteEntry {
    pub share_id: Uuid,
    pub share_name: String,
    pub relative_path: String,
    pub name: String,
    pub size: u64,
    pub is_directory: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorkspaceView {
    pub device_id: Uuid,
    pub roots: Vec<SharedRoot>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferView {
    pub id: Uuid,
    pub peer_device_id: Uuid,
    pub peer_name: String,
    pub name: String,
    pub direction: TransferDirection,
    pub state: TransferState,
    pub bytes_done: u64,
    pub total_bytes: u64,
    pub detail: String,
    pub bytes_per_second: u64,
    pub elapsed_millis: u64,
    pub estimated_remaining_seconds: Option<u64>,
    #[serde(skip)]
    started_at: Instant,
    #[serde(skip)]
    data_finished_at: Option<Instant>,
    #[serde(skip)]
    finished_at: Option<Instant>,
}

impl TransferView {
    pub fn new(
        id: Uuid,
        peer: (Uuid, String),
        name: String,
        direction: TransferDirection,
        state: TransferState,
        total_bytes: u64,
        detail: String,
    ) -> Self {
        Self {
            id,
            peer_device_id: peer.0,
            peer_name: peer.1,
            name,
            direction,
            state,
            bytes_done: 0,
            total_bytes,
            detail,
            bytes_per_second: 0,
            elapsed_millis: 0,
            estimated_remaining_seconds: None,
            started_at: Instant::now(),
            data_finished_at: None,
            finished_at: None,
        }
    }

    pub(crate) fn refresh_metrics(&mut self, now: Instant) {
        if self.total_bytes > 0
            && self.bytes_done >= self.total_bytes
            && self.data_finished_at.is_none()
        {
            self.data_finished_at = Some(now);
        }
        if self.is_terminal() && self.finished_at.is_none() {
            self.finished_at = Some(now);
        }

        let task_end = self.finished_at.unwrap_or(now);
        let elapsed = task_end.duration_since(self.started_at);
        self.elapsed_millis = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);

        let data_end = self.data_finished_at.unwrap_or(now);
        let data_seconds = data_end.duration_since(self.started_at).as_secs_f64();
        self.bytes_per_second = if self.bytes_done > 0 && data_seconds > 0.0 {
            (self.bytes_done as f64 / data_seconds).round() as u64
        } else {
            0
        };

        self.estimated_remaining_seconds = if self.state == TransferState::Transferring
            && self.bytes_done < self.total_bytes
            && self.bytes_per_second > 0
        {
            Some(
                ((self.total_bytes - self.bytes_done) as f64 / self.bytes_per_second as f64).ceil()
                    as u64,
            )
        } else {
            None
        };
    }

    pub(crate) fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            TransferState::Completed | TransferState::Failed | TransferState::Canceled
        )
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    Sending,
    Receiving,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransferState {
    Queued,
    Transferring,
    WaitingForPeer,
    Cancelling,
    Canceled,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransferCancelResult {
    Canceled,
    AlreadyCompleted,
    NotFound,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryPageView {
    pub device_id: Uuid,
    pub share_id: Uuid,
    pub relative_path: String,
    pub offset: u32,
    pub next_offset: Option<u32>,
    pub entries: Vec<RemoteEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    pub level: NoticeLevel,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Success,
    Error,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn transfer_metrics_include_rate_elapsed_time_and_eta() {
        let started_at = Instant::now();
        let mut transfer = TransferView::new(
            Uuid::nil(),
            (Uuid::nil(), "测试设备".into()),
            "测试文件.bin".into(),
            TransferDirection::Sending,
            TransferState::Transferring,
            10 * 1024 * 1024,
            "正在发送".into(),
        );
        transfer.started_at = started_at;
        transfer.bytes_done = 4 * 1024 * 1024;
        transfer.refresh_metrics(started_at + Duration::from_secs(2));

        assert_eq!(transfer.bytes_per_second, 2 * 1024 * 1024);
        assert_eq!(transfer.elapsed_millis, 2_000);
        assert_eq!(transfer.estimated_remaining_seconds, Some(3));
    }

    #[test]
    fn completed_transfer_freezes_elapsed_time_and_has_no_eta() {
        let started_at = Instant::now();
        let mut transfer = TransferView::new(
            Uuid::nil(),
            (Uuid::nil(), "测试设备".into()),
            "测试文件.bin".into(),
            TransferDirection::Receiving,
            TransferState::Completed,
            1024,
            "已完成".into(),
        );
        transfer.started_at = started_at;
        transfer.bytes_done = 1024;
        transfer.refresh_metrics(started_at + Duration::from_secs(2));
        transfer.refresh_metrics(started_at + Duration::from_secs(8));

        assert_eq!(transfer.elapsed_millis, 2_000);
        assert_eq!(transfer.bytes_per_second, 512);
        assert_eq!(transfer.estimated_remaining_seconds, None);
    }
}
