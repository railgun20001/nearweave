use std::{
    collections::{HashMap, HashSet, VecDeque},
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use sha2::Sha256;
use tauri::{AppHandle, Emitter};
use tokio::{
    fs::File,
    sync::{Mutex as AsyncMutex, oneshot},
};
use uuid::Uuid;

use crate::{
    error::{AppError, AppResult},
    identity::{
        DeviceIdentity, TrustedDevice, fingerprint, now_unix_seconds, save_trusted_devices,
    },
    models::{
        ActiveLink, AppSnapshot, ConnectionKind, ConnectionServiceState, DirectoryPageView,
        ListenerStatus, LocalShare, LocalShareView, NearbyDevice, Notice, NoticeLevel,
        PairingRequestView, PeerConnectionView, RemoteWorkspaceView, ServiceStatus, SharedRoot,
        TransferState, TransferView, TrustedDeviceView,
    },
    protocol::{Frame, NetworkOffer},
    settings::{self, UserSettings},
    transport::{ListenerHandle, NetworkRuntime, TransportCommand, TransportSender},
};

#[derive(Clone)]
pub struct AppState {
    pub(crate) inner: Arc<SharedState>,
}

pub(crate) struct SharedState {
    pub device_id: Uuid,
    pub device_name: String,
    pub receive_directory: PathBuf,
    pub legacy_receive_directory: Option<PathBuf>,
    pub settings_path: PathBuf,
    pub trust_path: PathBuf,
    pub identity: DeviceIdentity,
    pub listening: Mutex<bool>,
    pub connection_service_state: Mutex<ConnectionServiceState>,
    pub service_transition: AsyncMutex<()>,
    pub service_retry_token: Mutex<Option<Uuid>>,
    pub listener: Mutex<Option<ListenerHandle>>,
    pub bluetooth_error: Mutex<Option<String>>,
    pub discovery_error: Mutex<Option<String>>,
    pub tcp_error: Mutex<Option<String>>,
    pub tcp_port: Mutex<Option<u16>>,
    pub local_addresses: Mutex<Vec<String>>,
    pub connections: Mutex<HashMap<Uuid, ConnectionMeta>>,
    pub network_runtime: Mutex<Option<NetworkRuntime>>,
    pub network_peers: Mutex<HashMap<Uuid, NetworkPeer>>,
    pub network_connections: Mutex<HashMap<Uuid, NetworkConnectionMeta>>,
    pub network_connecting: Mutex<HashSet<Uuid>>,
    pub pending_handshakes: Mutex<HashSet<Uuid>>,
    pub reconnects: Mutex<HashMap<Uuid, ReconnectPlan>>,
    pub clipboard_enabled: Mutex<bool>,
    pub autostart_enabled: Mutex<bool>,
    pub lan_enabled: Mutex<bool>,
    pub lan_setup_decided: Mutex<bool>,
    pub devices: Mutex<Vec<NearbyDevice>>,
    pub lan_last_seen: Mutex<HashMap<Uuid, Instant>>,
    pub trusted_devices: Mutex<Vec<TrustedDevice>>,
    pub pending_pairings: Mutex<HashMap<Uuid, PendingPairing>>,
    pub local_shares: Mutex<Vec<LocalShare>>,
    pub remote_shares: Mutex<HashMap<Uuid, Vec<SharedRoot>>>,
    pub remote_share_revisions: Mutex<HashMap<Uuid, Uuid>>,
    pub remote_directory_cache: Mutex<HashMap<Uuid, VecDeque<DirectoryPageView>>>,
    pub peer_capabilities: Mutex<HashMap<Uuid, HashSet<String>>>,
    pub directory_requests: AsyncMutex<DirectoryRequestMap>,
    pub transfers: Mutex<Vec<TransferView>>,
    pub send_locks: AsyncMutex<HashMap<Uuid, Arc<AsyncMutex<()>>>>,
    pub transfer_epoch: AtomicU64,
    pub incoming: AsyncMutex<HashMap<(Uuid, Uuid), IncomingTransfer>>,
    pub reserved_destinations: AsyncMutex<HashSet<PathBuf>>,
    pub transfer_cancellations: Mutex<HashMap<(Uuid, Uuid), Arc<AtomicBool>>>,
    pub transfer_acks: Mutex<TransferAckMap>,
    pub canceled_incoming: Mutex<HashMap<(Uuid, Uuid), Instant>>,
    pub pending_cancel_acks: Mutex<HashSet<(Uuid, Uuid)>>,
    pub clipboard_commands: Mutex<Option<std::sync::mpsc::Sender<ClipboardCommand>>>,
}

type DirectoryRequestMap = HashMap<(Uuid, Uuid), oneshot::Sender<AppResult<DirectoryPageView>>>;
type TransferAckMap = HashMap<(Uuid, Uuid), oneshot::Sender<(bool, String)>>;

pub(crate) struct ConnectionMeta {
    pub generation: Uuid,
    pub kind: ConnectionKind,
    pub peer_name: String,
    pub sender: TransportSender,
    pub last_pong: Instant,
    pub last_active: Instant,
    pub reconnect_session: Option<Uuid>,
    pub peer_device_id: Option<Uuid>,
    pub bluetooth_endpoint: Option<String>,
}

#[derive(Clone)]
pub(crate) struct NetworkPeer {
    pub bluetooth_generation: Uuid,
    pub device_id: Uuid,
    pub session_id: Uuid,
    pub key: [u8; 32],
}

pub(crate) struct NetworkConnectionMeta {
    pub generation: Uuid,
    pub sender: TransportSender,
}

#[derive(Clone)]
pub(crate) struct ReconnectTarget {
    pub device_id: Option<Uuid>,
    pub display_name: String,
    pub lan_endpoint: Option<SocketAddr>,
    pub bluetooth_endpoint: Option<String>,
}

pub(crate) struct ReconnectPlan {
    pub target: ReconnectTarget,
    pub task_running: bool,
    pub reconnecting: bool,
}

pub(crate) struct ClosedConnection {
    pub reconnect_session: Option<Uuid>,
    pub peer_device_id: Option<Uuid>,
}

pub(crate) struct PendingPairing {
    pub view: PairingRequestView,
    pub decision: Option<oneshot::Sender<bool>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrustMatch {
    Trusted,
    Unknown,
    IdentityChanged,
}

pub(crate) struct IncomingTransfer {
    pub final_path: PathBuf,
    pub temporary_path: PathBuf,
    pub file: File,
    pub hasher: Sha256,
    pub received: u64,
    pub expected: u64,
}

#[derive(Debug)]
pub(crate) enum ClipboardCommand {
    ApplyRemoteText(Uuid, String, String),
    LocalClipboardChanged,
    SyncPeer(Uuid),
    Stop,
}

pub(crate) struct AppStateConfig {
    pub device_id: Uuid,
    pub device_name: String,
    pub receive_directory: PathBuf,
    pub legacy_receive_directory: Option<PathBuf>,
    pub settings_path: PathBuf,
    pub trust_path: PathBuf,
    pub identity: DeviceIdentity,
    pub trusted_devices: Vec<TrustedDevice>,
    pub clipboard_enabled: bool,
    pub autostart_enabled: bool,
    pub lan_enabled: bool,
    pub lan_setup_decided: bool,
}

impl AppState {
    pub(crate) fn new(config: AppStateConfig) -> Self {
        Self {
            inner: Arc::new(SharedState {
                device_id: config.device_id,
                device_name: config.device_name,
                receive_directory: config.receive_directory,
                legacy_receive_directory: config.legacy_receive_directory,
                settings_path: config.settings_path,
                trust_path: config.trust_path,
                identity: config.identity,
                listening: Mutex::new(true),
                connection_service_state: Mutex::new(ConnectionServiceState::Stopped),
                service_transition: AsyncMutex::new(()),
                service_retry_token: Mutex::new(None),
                listener: Mutex::new(None),
                bluetooth_error: Mutex::new(None),
                discovery_error: Mutex::new(None),
                tcp_error: Mutex::new(None),
                tcp_port: Mutex::new(None),
                local_addresses: Mutex::new(Vec::new()),
                connections: Mutex::new(HashMap::new()),
                network_runtime: Mutex::new(None),
                network_peers: Mutex::new(HashMap::new()),
                network_connections: Mutex::new(HashMap::new()),
                network_connecting: Mutex::new(HashSet::new()),
                pending_handshakes: Mutex::new(HashSet::new()),
                reconnects: Mutex::new(HashMap::new()),
                clipboard_enabled: Mutex::new(config.clipboard_enabled),
                autostart_enabled: Mutex::new(config.autostart_enabled),
                lan_enabled: Mutex::new(config.lan_enabled),
                lan_setup_decided: Mutex::new(config.lan_setup_decided),
                devices: Mutex::new(Vec::new()),
                lan_last_seen: Mutex::new(HashMap::new()),
                trusted_devices: Mutex::new(config.trusted_devices),
                pending_pairings: Mutex::new(HashMap::new()),
                local_shares: Mutex::new(Vec::new()),
                remote_shares: Mutex::new(HashMap::new()),
                remote_share_revisions: Mutex::new(HashMap::new()),
                remote_directory_cache: Mutex::new(HashMap::new()),
                peer_capabilities: Mutex::new(HashMap::new()),
                directory_requests: AsyncMutex::new(HashMap::new()),
                transfers: Mutex::new(Vec::new()),
                send_locks: AsyncMutex::new(HashMap::new()),
                transfer_epoch: AtomicU64::new(0),
                incoming: AsyncMutex::new(HashMap::new()),
                reserved_destinations: AsyncMutex::new(HashSet::new()),
                transfer_cancellations: Mutex::new(HashMap::new()),
                transfer_acks: Mutex::new(HashMap::new()),
                canceled_incoming: Mutex::new(HashMap::new()),
                pending_cancel_acks: Mutex::new(HashSet::new()),
                clipboard_commands: Mutex::new(None),
            }),
        }
    }

    pub fn snapshot(&self) -> AppSnapshot {
        let connections = self.inner.connections.lock().expect("连接状态锁损坏");
        let network_connections = self
            .inner
            .network_connections
            .lock()
            .expect("局域网连接状态锁损坏");
        let reconnects = self.inner.reconnects.lock().expect("自动重连状态锁损坏");
        let mut connection_views = connections
            .values()
            .filter_map(|connection| {
                let device_id = connection.peer_device_id?;
                let reconnecting = reconnects
                    .values()
                    .any(|value| value.target.device_id == Some(device_id) && value.reconnecting);
                Some(PeerConnectionView {
                    device_id,
                    name: connection.peer_name.clone(),
                    connection_kind: connection.kind,
                    bluetooth_connected: connection.kind == ConnectionKind::Bluetooth,
                    network_connected: connection.kind == ConnectionKind::Lan
                        || network_connections.contains_key(&device_id),
                    reconnecting,
                })
            })
            .collect::<Vec<_>>();
        connection_views.sort_by_key(|view| {
            std::cmp::Reverse(
                connections
                    .values()
                    .find(|connection| connection.peer_device_id == Some(view.device_id))
                    .map(|connection| connection.last_active),
            )
        });
        let first_connection = connections.values().next();
        let primary_kind = first_connection.map(|value| value.kind);
        let network_connected = first_connection.is_some_and(|connection| {
            connection.kind == ConnectionKind::Lan
                || connection
                    .peer_device_id
                    .is_some_and(|device_id| network_connections.contains_key(&device_id))
        });
        let listening = *self.inner.listening.lock().expect("监听状态锁损坏");
        let connection_service_state = *self
            .inner
            .connection_service_state
            .lock()
            .expect("连接服务状态锁损坏");
        let listener_status = self.listener_status(listening);
        let pairing_requests = self
            .inner
            .pending_pairings
            .lock()
            .expect("配对状态锁损坏")
            .values()
            .map(|value| value.view.clone())
            .collect::<Vec<_>>();
        let remote_workspaces = {
            let shares = self.inner.remote_shares.lock().expect("远端共享锁损坏");
            shares
                .iter()
                .map(|(device_id, roots)| RemoteWorkspaceView {
                    device_id: *device_id,
                    roots: roots.clone(),
                })
                .collect()
        };
        AppSnapshot {
            platform: std::env::consts::OS.into(),
            device_id: self.inner.device_id,
            device_name: self.inner.device_name.clone(),
            listening,
            connection_service_state,
            listener_status,
            connected: !connections.is_empty(),
            network_connected,
            active_link: if network_connected {
                ActiveLink::Network
            } else if !connections.is_empty() {
                ActiveLink::Bluetooth
            } else {
                ActiveLink::None
            },
            reconnecting: reconnects.values().any(|value| value.reconnecting),
            peer_name: first_connection.map(|value| value.peer_name.clone()),
            connection_kind: primary_kind,
            pairing_request: pairing_requests.first().cloned(),
            pairing_requests,
            connections: connection_views,
            trusted_devices: self
                .inner
                .trusted_devices
                .lock()
                .expect("信任设备锁损坏")
                .iter()
                .map(|value| TrustedDeviceView {
                    device_id: value.device_id,
                    name: value.name.clone(),
                    fingerprint: value.fingerprint.clone(),
                    last_seen_at: value.last_seen_at,
                })
                .collect(),
            clipboard_enabled: *self
                .inner
                .clipboard_enabled
                .lock()
                .expect("剪贴板状态锁损坏"),
            autostart_enabled: *self
                .inner
                .autostart_enabled
                .lock()
                .expect("开机自启状态锁损坏"),
            lan_enabled: self.lan_enabled(),
            lan_setup_required: !*self
                .inner
                .lan_setup_decided
                .lock()
                .expect("局域网首次设置状态锁损坏"),
            receive_directory: self.inner.receive_directory.to_string_lossy().into_owned(),
            legacy_receive_directory: self
                .inner
                .legacy_receive_directory
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            devices: self.inner.devices.lock().expect("设备列表锁损坏").clone(),
            local_shares: self
                .inner
                .local_shares
                .lock()
                .expect("共享目录锁损坏")
                .iter()
                .map(LocalShareView::from)
                .collect(),
            remote_workspaces,
            transfers: {
                let mut transfers = self.inner.transfers.lock().expect("传输状态锁损坏");
                let now = Instant::now();
                for transfer in transfers.iter_mut() {
                    transfer.refresh_metrics(now);
                }
                transfers.clone()
            },
        }
    }

    fn listener_status(&self, listening: bool) -> ListenerStatus {
        let bluetooth_error = self
            .inner
            .bluetooth_error
            .lock()
            .expect("蓝牙服务状态锁损坏")
            .clone();
        let discovery_error = self
            .inner
            .discovery_error
            .lock()
            .expect("局域网发现状态锁损坏")
            .clone();
        let tcp_error = self
            .inner
            .tcp_error
            .lock()
            .expect("局域网接收状态锁损坏")
            .clone();
        let tcp_port = *self.inner.tcp_port.lock().expect("TCP 端口状态锁损坏");
        let component = |enabled: bool, available: bool, error: &Option<String>| {
            if !listening || !enabled {
                ServiceStatus::Off
            } else if error.is_some() || !available {
                ServiceStatus::Error
            } else {
                ServiceStatus::Ready
            }
        };
        ListenerStatus {
            bluetooth: component(
                true,
                self.inner
                    .listener
                    .lock()
                    .expect("监听实例锁损坏")
                    .is_some(),
                &bluetooth_error,
            ),
            discovery: component(
                self.lan_enabled(),
                self.network_runtime()
                    .is_some_and(|runtime| runtime.discovery_available()),
                &discovery_error,
            ),
            tcp: component(self.lan_enabled(), tcp_port.is_some(), &tcp_error),
            bluetooth_error,
            discovery_error,
            tcp_error,
            tcp_port,
            local_addresses: self
                .inner
                .local_addresses
                .lock()
                .expect("本机地址状态锁损坏")
                .clone(),
        }
    }

    pub fn emit_snapshot(&self, app: &AppHandle) {
        let _ = app.emit("nearweave://state", self.snapshot());
    }

    pub fn emit_notice(&self, app: &AppHandle, level: NoticeLevel, message: impl Into<String>) {
        let _ = app.emit(
            "nearweave://notice",
            Notice {
                level,
                message: message.into(),
            },
        );
    }

    pub fn install_connection(
        &self,
        sender: TransportSender,
        kind: ConnectionKind,
        peer_hint: impl Into<String>,
        peer_device_id: Option<Uuid>,
        bluetooth_endpoint: Option<String>,
        reconnect_session: Option<Uuid>,
    ) -> AppResult<Uuid> {
        if !self.receiver_enabled() {
            return Err(AppError::InvalidInput("连接服务已停止".into()));
        }
        let generation = Uuid::new_v4();
        let mut connections = self.inner.connections.lock().expect("连接状态锁损坏");
        if !self.receiver_enabled() {
            return Err(AppError::InvalidInput("连接服务已停止".into()));
        }
        let unique_peers = connections
            .values()
            .filter_map(|value| value.peer_device_id)
            .collect::<HashSet<_>>()
            .len()
            + connections
                .values()
                .filter(|value| value.peer_device_id.is_none())
                .count();
        if unique_peers >= 8
            && peer_device_id.is_none_or(|device_id| {
                !connections
                    .values()
                    .any(|value| value.peer_device_id == Some(device_id))
            })
        {
            return Err(AppError::Protocol("最多同时连接 8 台设备".into()));
        }
        if let Some(device_id) = peer_device_id {
            let duplicate = connections
                .iter()
                .find(|(_, value)| value.peer_device_id == Some(device_id))
                .map(|(generation, _)| *generation);
            if let Some(duplicate) = duplicate
                && let Some(previous) = connections.remove(&duplicate)
            {
                let _ = previous
                    .sender
                    .try_send(TransportCommand::Close { reason: None });
            }
        }
        connections.insert(
            generation,
            ConnectionMeta {
                generation,
                kind,
                peer_name: peer_hint.into(),
                sender,
                last_pong: Instant::now(),
                last_active: Instant::now(),
                reconnect_session,
                peer_device_id,
                bluetooth_endpoint,
            },
        );
        drop(connections);
        if let Some(session) = reconnect_session {
            self.mark_reconnected(session);
        }
        Ok(generation)
    }

    pub fn update_peer_identity(
        &self,
        generation: Uuid,
        device_id: Uuid,
        peer_name: String,
        capabilities: Vec<String>,
    ) -> bool {
        let mut connections = self.inner.connections.lock().expect("连接状态锁损坏");
        if connections.iter().any(|(candidate, value)| {
            *candidate != generation && value.peer_device_id == Some(device_id)
        }) {
            if let Some(duplicate) = connections.remove(&generation) {
                let _ = duplicate.sender.try_send(TransportCommand::Close {
                    reason: Some("已存在同一设备连接".into()),
                });
            }
            return false;
        }
        let bluetooth_endpoint = if let Some(connection) = connections.get_mut(&generation) {
            connection.peer_device_id = Some(device_id);
            connection.peer_name = peer_name.clone();
            connection.bluetooth_endpoint.clone()
        } else {
            return false;
        };
        drop(connections);
        self.inner
            .peer_capabilities
            .lock()
            .expect("设备能力锁损坏")
            .insert(device_id, capabilities.into_iter().collect());
        if let Some(endpoint) = bluetooth_endpoint {
            self.associate_bluetooth_identity(&endpoint, device_id, &peer_name);
        }
        true
    }

    pub fn record_pong(&self, generation: Uuid) {
        let mut connections = self.inner.connections.lock().expect("连接状态锁损坏");
        if let Some(connection) = connections.get_mut(&generation) {
            connection.last_pong = Instant::now();
            connection.last_active = Instant::now();
        }
    }

    pub fn record_activity(&self, generation: Uuid) {
        if let Some(connection) = self
            .inner
            .connections
            .lock()
            .expect("连接状态锁损坏")
            .get_mut(&generation)
        {
            connection.last_active = Instant::now();
        }
    }

    pub fn pong_elapsed(&self, generation: Uuid) -> Option<Duration> {
        self.inner
            .connections
            .lock()
            .expect("连接状态锁损坏")
            .get(&generation)
            .map(|value| value.last_pong.elapsed())
    }

    pub fn close_connection(&self, generation: Uuid, reason: Option<String>) {
        if let Some(connection) = self
            .inner
            .connections
            .lock()
            .expect("连接状态锁损坏")
            .get(&generation)
        {
            let _ = connection
                .sender
                .try_send(TransportCommand::Close { reason });
        }
    }

    pub fn clear_connection(&self, generation: Uuid) -> Option<ClosedConnection> {
        let connection = self
            .inner
            .connections
            .lock()
            .expect("连接状态锁损坏")
            .remove(&generation)?;
        if let Some(device_id) = connection.peer_device_id {
            self.cancel_transfer_acks_for_peer(device_id);
            self.disconnect_network(device_id);
            self.inner
                .remote_shares
                .lock()
                .expect("远端共享锁损坏")
                .remove(&device_id);
            self.inner
                .remote_share_revisions
                .lock()
                .expect("远端共享版本锁损坏")
                .remove(&device_id);
            self.clear_remote_directory_cache(device_id);
            self.inner
                .peer_capabilities
                .lock()
                .expect("设备能力锁损坏")
                .remove(&device_id);
        }
        Some(ClosedConnection {
            reconnect_session: connection.reconnect_session,
            peer_device_id: connection.peer_device_id,
        })
    }

    pub fn disconnect(&self) {
        self.cancel_reconnect();
        self.cancel_all_transfer_acks();
        for (_, connection) in self
            .inner
            .connections
            .lock()
            .expect("连接状态锁损坏")
            .drain()
        {
            let _ = connection
                .sender
                .try_send(TransportCommand::Close { reason: None });
        }
        self.disconnect_all_networks();
        self.inner
            .remote_shares
            .lock()
            .expect("远端共享锁损坏")
            .clear();
        self.inner
            .remote_share_revisions
            .lock()
            .expect("远端共享版本锁损坏")
            .clear();
        self.inner
            .remote_directory_cache
            .lock()
            .expect("远端目录缓存锁损坏")
            .clear();
    }

    pub fn connection_kind(&self, generation: Uuid) -> Option<ConnectionKind> {
        self.inner
            .connections
            .lock()
            .expect("连接状态锁损坏")
            .get(&generation)
            .map(|value| value.kind)
    }

    pub fn connection_generation_for_peer(&self, device_id: Uuid) -> Option<Uuid> {
        self.inner
            .connections
            .lock()
            .expect("连接状态锁损坏")
            .iter()
            .find(|(_, value)| value.peer_device_id == Some(device_id))
            .map(|(generation, _)| *generation)
    }

    pub fn peer_for_generation(&self, generation: Uuid) -> Option<Uuid> {
        self.inner
            .connections
            .lock()
            .expect("连接状态锁损坏")
            .get(&generation)
            .and_then(|value| value.peer_device_id)
    }

    pub fn peer_name(&self, device_id: Uuid) -> String {
        self.inner
            .connections
            .lock()
            .expect("连接状态锁损坏")
            .values()
            .find(|value| value.peer_device_id == Some(device_id))
            .map(|value| value.peer_name.clone())
            .unwrap_or_else(|| device_id.to_string())
    }

    pub fn begin_reconnect_session(&self, target: ReconnectTarget) -> Uuid {
        let session = Uuid::new_v4();
        self.inner
            .reconnects
            .lock()
            .expect("自动重连状态锁损坏")
            .insert(
                session,
                ReconnectPlan {
                    target,
                    task_running: false,
                    reconnecting: false,
                },
            );
        session
    }

    pub fn reconnect_target(&self, session: Uuid) -> Option<ReconnectTarget> {
        self.inner
            .reconnects
            .lock()
            .expect("自动重连状态锁损坏")
            .get(&session)
            .map(|value| value.target.clone())
    }

    pub fn reconnect_target_for_peer(&self, device_id: Uuid) -> Option<ReconnectTarget> {
        self.inner
            .reconnects
            .lock()
            .expect("自动重连状态锁损坏")
            .values()
            .find(|value| value.target.device_id == Some(device_id))
            .map(|value| value.target.clone())
    }

    pub fn update_reconnect_target(
        &self,
        session: Uuid,
        device_id: Uuid,
        display_name: String,
        lan_endpoint: Option<SocketAddr>,
    ) {
        if let Some(reconnect) = self
            .inner
            .reconnects
            .lock()
            .expect("自动重连状态锁损坏")
            .get_mut(&session)
        {
            reconnect.target.device_id = Some(device_id);
            reconnect.target.display_name = display_name;
            if lan_endpoint.is_some() {
                reconnect.target.lan_endpoint = lan_endpoint;
            }
        }
    }

    pub fn is_reconnect_session_active(&self, session: Uuid) -> bool {
        self.inner
            .reconnects
            .lock()
            .expect("自动重连状态锁损坏")
            .contains_key(&session)
    }

    pub fn start_reconnect_task(&self, session: Uuid) -> bool {
        let mut reconnects = self.inner.reconnects.lock().expect("自动重连状态锁损坏");
        let Some(reconnect) = reconnects
            .get_mut(&session)
            .filter(|value| !value.task_running)
        else {
            return false;
        };
        reconnect.task_running = true;
        reconnect.reconnecting = true;
        true
    }

    pub fn mark_reconnected(&self, session: Uuid) {
        if let Some(reconnect) = self
            .inner
            .reconnects
            .lock()
            .expect("自动重连状态锁损坏")
            .get_mut(&session)
        {
            reconnect.task_running = false;
            reconnect.reconnecting = false;
        }
    }

    pub fn cancel_reconnect(&self) {
        self.inner
            .reconnects
            .lock()
            .expect("自动重连状态锁损坏")
            .clear();
    }

    pub fn cancel_reconnect_for_peer(&self, device_id: Uuid) {
        self.inner
            .reconnects
            .lock()
            .expect("自动重连状态锁损坏")
            .retain(|_, value| value.target.device_id != Some(device_id));
    }

    pub fn cancel_reconnect_session(&self, session: Uuid) {
        self.inner
            .reconnects
            .lock()
            .expect("自动重连状态锁损坏")
            .remove(&session);
    }

    pub fn disconnect_peer(&self, device_id: Uuid) {
        self.cancel_reconnect_for_peer(device_id);
        self.cancel_transfer_acks_for_peer(device_id);
        let generations = self
            .inner
            .connections
            .lock()
            .expect("连接状态锁损坏")
            .iter()
            .filter_map(|(generation, value)| {
                (value.peer_device_id == Some(device_id)).then_some(*generation)
            })
            .collect::<Vec<_>>();
        let mut connections = self.inner.connections.lock().expect("连接状态锁损坏");
        for generation in generations {
            if let Some(connection) = connections.remove(&generation) {
                let _ = connection
                    .sender
                    .try_send(TransportCommand::Close { reason: None });
            }
        }
        drop(connections);
        self.disconnect_network(device_id);
        self.inner
            .remote_shares
            .lock()
            .expect("远端共享锁损坏")
            .remove(&device_id);
        self.inner
            .remote_share_revisions
            .lock()
            .expect("远端共享版本锁损坏")
            .remove(&device_id);
        self.clear_remote_directory_cache(device_id);
    }

    pub async fn send_frame_to(&self, device_id: Uuid, frame: Frame) -> AppResult<()> {
        let network_sender = frame
            .message
            .prefers_network()
            .then(|| {
                self.inner
                    .network_connections
                    .lock()
                    .expect("局域网连接状态锁损坏")
                    .get(&device_id)
                    .map(|value| value.sender.clone())
            })
            .flatten();
        if let Some(sender) = network_sender
            && sender
                .send(TransportCommand::Send(frame.clone()))
                .await
                .is_ok()
        {
            return Ok(());
        }
        let sender = {
            let mut connections = self.inner.connections.lock().expect("连接状态锁损坏");
            let connection = connections
                .values_mut()
                .find(|value| value.peer_device_id == Some(device_id))
                .ok_or(AppError::NotConnected)?;
            connection.last_active = Instant::now();
            connection.sender.clone()
        };
        sender
            .send(TransportCommand::Send(frame))
            .await
            .map_err(|_| AppError::NotConnected)
    }

    pub fn try_send_frame(&self, frame: Frame) -> AppResult<()> {
        let peers = self.connected_peer_ids();
        if peers.is_empty() {
            return Err(AppError::NotConnected);
        }
        let mut sent = false;
        for peer in peers {
            if self.try_send_frame_to(peer, frame.clone()).is_ok() {
                sent = true;
            }
        }
        sent.then_some(())
            .ok_or_else(|| AppError::Protocol("发送队列繁忙，请稍后重试".into()))
    }

    pub fn try_send_frame_to(&self, device_id: Uuid, frame: Frame) -> AppResult<()> {
        if frame.message.prefers_network()
            && let Some(sender) = self
                .inner
                .network_connections
                .lock()
                .expect("局域网连接状态锁损坏")
                .get(&device_id)
                .map(|value| value.sender.clone())
            && sender
                .try_send(TransportCommand::Send(frame.clone()))
                .is_ok()
        {
            return Ok(());
        }
        let sender = {
            let mut connections = self.inner.connections.lock().expect("连接状态锁损坏");
            let connection = connections
                .values_mut()
                .find(|value| value.peer_device_id == Some(device_id))
                .ok_or(AppError::NotConnected)?;
            connection.last_active = Instant::now();
            connection.sender.clone()
        };
        sender
            .try_send(TransportCommand::Send(frame))
            .map_err(|_| AppError::Protocol("发送队列繁忙，请稍后重试".into()))
    }

    pub async fn send_frame_for_generation(&self, generation: Uuid, frame: Frame) -> AppResult<()> {
        if let Some(peer) = self.peer_for_generation(generation) {
            return self.send_frame_to(peer, frame).await;
        }
        let sender = self
            .inner
            .connections
            .lock()
            .expect("连接状态锁损坏")
            .get(&generation)
            .map(|value| value.sender.clone())
            .ok_or(AppError::NotConnected)?;
        sender
            .send(TransportCommand::Send(frame))
            .await
            .map_err(|_| AppError::NotConnected)
    }

    pub fn connected_peer_ids(&self) -> Vec<Uuid> {
        self.inner
            .connections
            .lock()
            .expect("连接状态锁损坏")
            .values()
            .filter_map(|value| value.peer_device_id)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn add_transfer(&self, transfer: TransferView) {
        let mut transfers = self.inner.transfers.lock().expect("传输状态锁损坏");
        if let Some(index) = transfers.iter().position(|value| {
            value.id == transfer.id && value.peer_device_id == transfer.peer_device_id
        }) {
            transfers.remove(index);
        }
        transfers.insert(0, transfer);
        transfers.truncate(100);
    }

    pub async fn send_lock(&self, device_id: Uuid) -> Arc<AsyncMutex<()>> {
        self.inner
            .send_locks
            .lock()
            .await
            .entry(device_id)
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    pub fn transfer_epoch(&self) -> u64 {
        self.inner.transfer_epoch.load(Ordering::Acquire)
    }

    pub fn transfer_epoch_is_current(&self, epoch: u64) -> bool {
        self.inner.transfer_epoch.load(Ordering::Acquire) == epoch
    }

    pub fn update_transfer(
        &self,
        id: Uuid,
        bytes_done: Option<u64>,
        state: Option<TransferState>,
        detail: Option<String>,
    ) {
        let mut transfers = self.inner.transfers.lock().expect("传输状态锁损坏");
        if let Some(transfer) = transfers.iter_mut().find(|value| value.id == id) {
            if let Some(bytes_done) = bytes_done {
                transfer.bytes_done = bytes_done;
            }
            if let Some(state) = state {
                transfer.state = state;
            }
            if let Some(detail) = detail {
                transfer.detail = detail;
            }
            transfer.refresh_metrics(Instant::now());
        }
    }

    pub fn transfer(&self, device_id: Uuid, transfer_id: Uuid) -> Option<TransferView> {
        self.inner
            .transfers
            .lock()
            .expect("传输状态锁损坏")
            .iter()
            .find(|value| value.id == transfer_id && value.peer_device_id == device_id)
            .cloned()
    }

    pub fn register_transfer_cancellation(
        &self,
        device_id: Uuid,
        transfer_id: Uuid,
    ) -> Arc<AtomicBool> {
        let token = Arc::new(AtomicBool::new(false));
        self.inner
            .transfer_cancellations
            .lock()
            .expect("传输取消锁损坏")
            .insert((device_id, transfer_id), token.clone());
        token
    }

    pub fn begin_transfer_ack(
        &self,
        device_id: Uuid,
        transfer_id: Uuid,
    ) -> oneshot::Receiver<(bool, String)> {
        let (sender, receiver) = oneshot::channel();
        self.inner
            .transfer_acks
            .lock()
            .expect("传输完成确认锁损坏")
            .insert((device_id, transfer_id), sender);
        receiver
    }

    pub fn resolve_transfer_ack(
        &self,
        device_id: Uuid,
        transfer_id: Uuid,
        accepted: bool,
        detail: String,
    ) {
        if let Some(sender) = self
            .inner
            .transfer_acks
            .lock()
            .expect("传输完成确认锁损坏")
            .remove(&(device_id, transfer_id))
        {
            let _ = sender.send((accepted, detail));
        }
    }

    pub fn cancel_transfer_ack(&self, device_id: Uuid, transfer_id: Uuid) {
        self.inner
            .transfer_acks
            .lock()
            .expect("传输完成确认锁损坏")
            .remove(&(device_id, transfer_id));
    }

    pub fn request_transfer_cancellation(&self, device_id: Uuid, transfer_id: Uuid) -> bool {
        self.inner
            .transfer_cancellations
            .lock()
            .expect("传输取消锁损坏")
            .get(&(device_id, transfer_id))
            .is_some_and(|token| {
                token.store(true, Ordering::Release);
                true
            })
    }

    pub fn finish_transfer_cancellation(&self, device_id: Uuid, transfer_id: Uuid) {
        self.inner
            .transfer_cancellations
            .lock()
            .expect("传输取消锁损坏")
            .remove(&(device_id, transfer_id));
    }

    pub fn begin_cancel_ack_wait(&self, device_id: Uuid, transfer_id: Uuid) {
        self.inner
            .pending_cancel_acks
            .lock()
            .expect("传输取消确认锁损坏")
            .insert((device_id, transfer_id));
    }

    pub fn finish_cancel_ack_wait(&self, device_id: Uuid, transfer_id: Uuid) -> bool {
        self.inner
            .pending_cancel_acks
            .lock()
            .expect("传输取消确认锁损坏")
            .remove(&(device_id, transfer_id))
    }

    pub fn active_transfer_keys(&self) -> Vec<(Uuid, Uuid)> {
        self.inner
            .transfers
            .lock()
            .expect("传输状态锁损坏")
            .iter()
            .filter(|transfer| !transfer.is_terminal())
            .map(|transfer| (transfer.peer_device_id, transfer.id))
            .collect()
    }

    pub fn cancel_all_transfers(&self) {
        self.inner.transfer_epoch.fetch_add(1, Ordering::AcqRel);
        for token in self
            .inner
            .transfer_cancellations
            .lock()
            .expect("传输取消锁损坏")
            .values()
        {
            token.store(true, Ordering::Release);
        }
        let mut transfers = self.inner.transfers.lock().expect("传输状态锁损坏");
        for transfer in transfers.iter_mut().filter(|value| !value.is_terminal()) {
            transfer.state = TransferState::Cancelling;
            transfer.detail = "正在停止连接并取消任务".into();
        }
    }

    pub fn mark_all_transfers_canceled(&self, detail: &str) {
        let mut transfers = self.inner.transfers.lock().expect("传输状态锁损坏");
        for transfer in transfers.iter_mut().filter(|value| !value.is_terminal()) {
            transfer.state = TransferState::Canceled;
            transfer.detail = detail.into();
        }
        self.inner
            .transfer_cancellations
            .lock()
            .expect("传输取消锁损坏")
            .clear();
        self.inner
            .pending_cancel_acks
            .lock()
            .expect("传输取消确认锁损坏")
            .clear();
    }

    pub fn supports_capability(&self, device_id: Uuid, capability: &str) -> bool {
        self.inner
            .peer_capabilities
            .lock()
            .expect("设备能力锁损坏")
            .get(&device_id)
            .is_some_and(|values| values.contains(capability))
    }

    pub fn set_remote_roots(&self, device_id: Uuid, revision: Uuid, roots: Vec<SharedRoot>) {
        let changed = self
            .inner
            .remote_share_revisions
            .lock()
            .expect("远端共享版本锁损坏")
            .insert(device_id, revision)
            != Some(revision);
        if changed {
            self.clear_remote_directory_cache(device_id);
        }
        self.inner
            .remote_shares
            .lock()
            .expect("远端共享锁损坏")
            .insert(device_id, roots);
    }

    pub fn cached_remote_directory(
        &self,
        device_id: Uuid,
        share_id: Uuid,
        relative_path: &str,
        offset: u32,
    ) -> Option<DirectoryPageView> {
        self.inner
            .remote_directory_cache
            .lock()
            .expect("远端目录缓存锁损坏")
            .get(&device_id)
            .and_then(|pages| {
                pages.iter().find(|page| {
                    page.share_id == share_id
                        && page.relative_path == relative_path
                        && page.offset == offset
                })
            })
            .cloned()
    }

    pub fn cache_remote_directory(&self, page: DirectoryPageView) {
        let mut cache = self
            .inner
            .remote_directory_cache
            .lock()
            .expect("远端目录缓存锁损坏");
        let pages = cache.entry(page.device_id).or_default();
        pages.retain(|cached| {
            cached.share_id != page.share_id
                || cached.relative_path != page.relative_path
                || cached.offset != page.offset
        });
        pages.push_front(page);
        while pages.len() > 64 {
            pages.pop_back();
        }
    }

    pub fn clear_remote_directory_cache(&self, device_id: Uuid) {
        self.inner
            .remote_directory_cache
            .lock()
            .expect("远端目录缓存锁损坏")
            .remove(&device_id);
    }

    pub fn remote_roots(&self, device_id: Uuid) -> Vec<SharedRoot> {
        self.inner
            .remote_shares
            .lock()
            .expect("远端共享锁损坏")
            .get(&device_id)
            .cloned()
            .unwrap_or_default()
    }

    pub async fn begin_directory_request(
        &self,
        device_id: Uuid,
        request_id: Uuid,
    ) -> oneshot::Receiver<AppResult<DirectoryPageView>> {
        let (sender, receiver) = oneshot::channel();
        self.inner
            .directory_requests
            .lock()
            .await
            .insert((device_id, request_id), sender);
        receiver
    }

    pub async fn resolve_directory_request(
        &self,
        device_id: Uuid,
        request_id: Uuid,
        page: AppResult<DirectoryPageView>,
    ) {
        if let Some(sender) = self
            .inner
            .directory_requests
            .lock()
            .await
            .remove(&(device_id, request_id))
        {
            let _ = sender.send(page);
        }
    }

    pub async fn cancel_directory_request(&self, device_id: Uuid, request_id: Uuid) {
        self.inner
            .directory_requests
            .lock()
            .await
            .remove(&(device_id, request_id));
    }

    pub async fn cancel_all_directory_requests(&self) {
        for (_, sender) in self.inner.directory_requests.lock().await.drain() {
            let _ = sender.send(Err(AppError::NotConnected));
        }
    }

    pub fn cancel_all_transfer_acks(&self) {
        self.inner
            .transfer_acks
            .lock()
            .expect("传输完成确认锁损坏")
            .clear();
    }

    pub fn cancel_transfer_acks_for_peer(&self, device_id: Uuid) {
        self.inner
            .transfer_acks
            .lock()
            .expect("传输完成确认锁损坏")
            .retain(|(peer_device_id, _), _| *peer_device_id != device_id);
    }

    pub fn mark_canceled_incoming(&self, device_id: Uuid, transfer_id: Uuid) {
        let mut canceled = self
            .inner
            .canceled_incoming
            .lock()
            .expect("取消接收记录锁损坏");
        canceled.retain(|_, value| value.elapsed() < Duration::from_secs(120));
        canceled.insert((device_id, transfer_id), Instant::now());
    }

    pub fn is_canceled_incoming(&self, device_id: Uuid, transfer_id: Uuid) -> bool {
        self.inner
            .canceled_incoming
            .lock()
            .expect("取消接收记录锁损坏")
            .get(&(device_id, transfer_id))
            .is_some_and(|value| value.elapsed() < Duration::from_secs(120))
    }

    pub fn remove_transfer(&self, id: Uuid) -> AppResult<String> {
        let mut transfers = self.inner.transfers.lock().expect("传输状态锁损坏");
        let index = transfers
            .iter()
            .position(|transfer| transfer.id == id)
            .ok_or_else(|| AppError::InvalidInput("未找到要删除的传输任务".into()))?;
        if !transfers[index].is_terminal() {
            return Err(AppError::InvalidInput(
                "传输进行中，完成后才能删除记录".into(),
            ));
        }
        Ok(transfers.remove(index).name)
    }

    pub fn clear_transfer_history(&self) -> usize {
        let mut transfers = self.inner.transfers.lock().expect("传输状态锁损坏");
        let original_len = transfers.len();
        transfers.retain(|transfer| !transfer.is_terminal());
        original_len - transfers.len()
    }

    pub fn set_clipboard_command_sender(&self, sender: std::sync::mpsc::Sender<ClipboardCommand>) {
        *self
            .inner
            .clipboard_commands
            .lock()
            .expect("剪贴板命令锁损坏") = Some(sender);
    }

    pub fn send_clipboard_command(&self, command: ClipboardCommand) {
        if let Some(sender) = self
            .inner
            .clipboard_commands
            .lock()
            .expect("剪贴板命令锁损坏")
            .as_ref()
        {
            let _ = sender.send(command);
        }
    }

    pub fn set_clipboard_enabled(&self, enabled: bool) -> AppResult<()> {
        *self
            .inner
            .clipboard_enabled
            .lock()
            .expect("剪贴板状态锁损坏") = enabled;
        self.save_settings()
    }

    pub fn lan_enabled(&self) -> bool {
        *self.inner.lan_enabled.lock().expect("局域网启用状态锁损坏")
    }

    pub fn set_lan_enabled(&self, enabled: bool) -> AppResult<()> {
        *self.inner.lan_enabled.lock().expect("局域网启用状态锁损坏") = enabled;
        *self
            .inner
            .lan_setup_decided
            .lock()
            .expect("局域网首次设置状态锁损坏") = true;
        self.save_settings()
    }

    pub fn dismiss_lan_setup(&self) -> AppResult<()> {
        *self
            .inner
            .lan_setup_decided
            .lock()
            .expect("局域网首次设置状态锁损坏") = true;
        self.save_settings()
    }

    pub fn receiver_enabled(&self) -> bool {
        *self.inner.listening.lock().expect("监听状态锁损坏")
    }

    fn save_settings(&self) -> AppResult<()> {
        settings::save(
            &self.inner.settings_path,
            &UserSettings {
                clipboard_enabled: *self
                    .inner
                    .clipboard_enabled
                    .lock()
                    .expect("剪贴板状态锁损坏"),
                lan_enabled: self.lan_enabled(),
                lan_setup_decided: *self
                    .inner
                    .lan_setup_decided
                    .lock()
                    .expect("局域网首次设置状态锁损坏"),
            },
        )
    }

    pub fn connection_service_state(&self) -> ConnectionServiceState {
        *self
            .inner
            .connection_service_state
            .lock()
            .expect("连接服务状态锁损坏")
    }

    pub fn set_connection_service_state(&self, value: ConnectionServiceState) {
        *self
            .inner
            .connection_service_state
            .lock()
            .expect("连接服务状态锁损坏") = value;
        *self.inner.listening.lock().expect("监听状态锁损坏") = !matches!(
            value,
            ConnectionServiceState::Stopped | ConnectionServiceState::Stopping
        );
    }

    pub fn begin_service_retry(&self) -> Uuid {
        let token = Uuid::new_v4();
        *self
            .inner
            .service_retry_token
            .lock()
            .expect("服务重试状态锁损坏") = Some(token);
        token
    }

    pub fn service_retry_active(&self, token: Uuid) -> bool {
        self.inner
            .service_retry_token
            .lock()
            .expect("服务重试状态锁损坏")
            .is_some_and(|active| active == token)
            && self.receiver_enabled()
    }

    pub fn cancel_service_retry(&self) {
        self.inner
            .service_retry_token
            .lock()
            .expect("服务重试状态锁损坏")
            .take();
    }

    pub fn install_network_runtime(
        &self,
        runtime: NetworkRuntime,
        discovery_error: Option<String>,
        local_addresses: Vec<String>,
    ) {
        *self.inner.tcp_port.lock().expect("TCP 端口状态锁损坏") = Some(runtime.tcp_port);
        *self
            .inner
            .discovery_error
            .lock()
            .expect("局域网发现状态锁损坏") = discovery_error;
        *self.inner.tcp_error.lock().expect("局域网接收状态锁损坏") = None;
        *self
            .inner
            .local_addresses
            .lock()
            .expect("本机地址状态锁损坏") = local_addresses;
        *self
            .inner
            .network_runtime
            .lock()
            .expect("局域网运行状态锁损坏") = Some(runtime);
    }

    pub fn network_runtime(&self) -> Option<NetworkRuntime> {
        self.inner
            .network_runtime
            .lock()
            .expect("局域网运行状态锁损坏")
            .clone()
    }

    pub fn update_local_addresses(&self, addresses: Vec<String>) -> bool {
        let mut current = self
            .inner
            .local_addresses
            .lock()
            .expect("本机地址状态锁损坏");
        if *current == addresses {
            return false;
        }
        *current = addresses;
        true
    }

    pub fn take_network_runtime(&self) -> Option<NetworkRuntime> {
        *self.inner.tcp_port.lock().expect("TCP 端口状态锁损坏") = None;
        self.inner
            .local_addresses
            .lock()
            .expect("本机地址状态锁损坏")
            .clear();
        self.inner
            .network_runtime
            .lock()
            .expect("局域网运行状态锁损坏")
            .take()
    }

    pub fn set_network_start_error(&self, error: impl Into<String>) {
        let error = error.into();
        *self.inner.tcp_error.lock().expect("局域网接收状态锁损坏") = Some(error.clone());
        *self
            .inner
            .discovery_error
            .lock()
            .expect("局域网发现状态锁损坏") = Some(error);
        *self.inner.tcp_port.lock().expect("TCP 端口状态锁损坏") = None;
    }

    pub fn set_discovery_error(&self, error: Option<String>) {
        *self
            .inner
            .discovery_error
            .lock()
            .expect("局域网发现状态锁损坏") = error;
    }

    pub fn set_bluetooth_error(&self, error: Option<String>) {
        *self
            .inner
            .bluetooth_error
            .lock()
            .expect("蓝牙服务状态锁损坏") = error;
    }

    pub fn replace_bluetooth_devices(&self, bluetooth_devices: Vec<NearbyDevice>) {
        let mut devices = self.inner.devices.lock().expect("设备列表锁损坏");
        for device in devices.iter_mut() {
            device.bluetooth_endpoint = None;
            device.bluetooth_paired = false;
            device.nearweave_enabled = false;
        }
        devices.retain(|device| device.lan_endpoint.is_some());

        for incoming in bluetooth_devices {
            let existing_index = incoming
                .device_id
                .and_then(|device_id| {
                    devices
                        .iter()
                        .position(|device| device.device_id == Some(device_id))
                })
                .or_else(|| {
                    devices.iter().position(|device| {
                        device.bluetooth_endpoint == incoming.bluetooth_endpoint
                            && incoming.bluetooth_endpoint.is_some()
                    })
                });
            if let Some(existing) = existing_index.and_then(|index| devices.get_mut(index)) {
                existing.name = incoming.name;
                existing.bluetooth_endpoint = incoming.bluetooth_endpoint;
                existing.bluetooth_paired = incoming.bluetooth_paired;
                existing.nearweave_enabled = incoming.nearweave_enabled;
                if existing.device_id.is_none() {
                    existing.device_id = incoming.device_id;
                }
                existing.id = existing
                    .device_id
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| incoming.id.clone());
                existing.trusted = existing
                    .device_id
                    .is_some_and(|value| self.is_device_trusted(value));
            } else {
                devices.push(incoming);
            }
        }
        devices.sort_by(|left, right| left.name.cmp(&right.name));
    }

    pub fn upsert_lan_device(
        &self,
        device_id: Uuid,
        name: String,
        endpoint: SocketAddr,
        advertised_fingerprint: &str,
    ) {
        self.inner
            .lan_last_seen
            .lock()
            .expect("局域网设备时间锁损坏")
            .insert(device_id, Instant::now());
        let trusted = self
            .inner
            .trusted_devices
            .lock()
            .expect("信任设备锁损坏")
            .iter()
            .any(|value| {
                value.device_id == device_id && value.fingerprint == advertised_fingerprint
            });
        let mut devices = self.inner.devices.lock().expect("设备列表锁损坏");
        if let Some(device) = devices
            .iter_mut()
            .find(|device| device.device_id == Some(device_id))
        {
            device.name = name;
            device.lan_endpoint = Some(endpoint.to_string());
            device.trusted = trusted;
        } else {
            devices.push(NearbyDevice {
                id: device_id.to_string(),
                device_id: Some(device_id),
                name,
                bluetooth_endpoint: None,
                lan_endpoint: Some(endpoint.to_string()),
                bluetooth_paired: false,
                nearweave_enabled: false,
                trusted,
            });
        }
        devices.sort_by(|left, right| left.name.cmp(&right.name));
    }

    pub fn expire_lan_devices(&self, ttl: Duration) -> bool {
        let expired = {
            let mut last_seen = self
                .inner
                .lan_last_seen
                .lock()
                .expect("局域网设备时间锁损坏");
            let expired = last_seen
                .iter()
                .filter_map(|(device_id, seen)| (seen.elapsed() > ttl).then_some(*device_id))
                .collect::<Vec<_>>();
            for device_id in &expired {
                last_seen.remove(device_id);
            }
            expired
        };
        if expired.is_empty() {
            return false;
        }

        let mut devices = self.inner.devices.lock().expect("设备列表锁损坏");
        for device in devices.iter_mut() {
            if device
                .device_id
                .is_some_and(|device_id| expired.contains(&device_id))
            {
                device.lan_endpoint = None;
            }
        }
        devices.retain(|device| device.lan_endpoint.is_some() || device.bluetooth_paired);
        true
    }

    pub fn find_device(&self, id: &str) -> Option<NearbyDevice> {
        self.inner
            .devices
            .lock()
            .expect("设备列表锁损坏")
            .iter()
            .find(|device| {
                device.id == id
                    || device
                        .device_id
                        .is_some_and(|value| value.to_string() == id)
            })
            .cloned()
    }

    pub fn associate_bluetooth_identity(
        &self,
        bluetooth_endpoint: &str,
        device_id: Uuid,
        name: &str,
    ) {
        let mut devices = self.inner.devices.lock().expect("设备列表锁损坏");
        let bluetooth_index = devices
            .iter()
            .position(|device| device.bluetooth_endpoint.as_deref() == Some(bluetooth_endpoint));
        let Some(bluetooth_index) = bluetooth_index else {
            return;
        };
        let bluetooth = devices.remove(bluetooth_index);
        if let Some(existing) = devices
            .iter_mut()
            .find(|device| device.device_id == Some(device_id))
        {
            existing.bluetooth_endpoint = bluetooth.bluetooth_endpoint;
            existing.bluetooth_paired = true;
            existing.nearweave_enabled = bluetooth.nearweave_enabled;
        } else {
            devices.push(NearbyDevice {
                id: device_id.to_string(),
                device_id: Some(device_id),
                name: name.into(),
                bluetooth_endpoint: bluetooth.bluetooth_endpoint,
                lan_endpoint: None,
                bluetooth_paired: true,
                nearweave_enabled: bluetooth.nearweave_enabled,
                trusted: self.is_device_trusted(device_id),
            });
        }
    }

    pub fn trust_match(&self, device_id: Uuid, public_key: &[u8]) -> TrustMatch {
        self.inner
            .trusted_devices
            .lock()
            .expect("信任设备锁损坏")
            .iter()
            .find(|value| value.device_id == device_id)
            .map_or(TrustMatch::Unknown, |value| {
                if value.public_key == public_key {
                    TrustMatch::Trusted
                } else {
                    TrustMatch::IdentityChanged
                }
            })
    }

    pub fn is_device_trusted(&self, device_id: Uuid) -> bool {
        self.inner
            .trusted_devices
            .lock()
            .expect("信任设备锁损坏")
            .iter()
            .any(|value| value.device_id == device_id)
    }

    pub fn remember_trusted_device(
        &self,
        device_id: Uuid,
        name: String,
        public_key: Vec<u8>,
    ) -> AppResult<()> {
        let now = now_unix_seconds();
        let mut devices = self.inner.trusted_devices.lock().expect("信任设备锁损坏");
        let mut updated = devices.clone();
        if let Some(device) = updated
            .iter_mut()
            .find(|value| value.device_id == device_id)
        {
            device.name = name;
            device.public_key = public_key.clone();
            device.fingerprint = fingerprint(&public_key);
            device.last_seen_at = now;
        } else {
            updated.push(TrustedDevice {
                device_id,
                name,
                fingerprint: fingerprint(&public_key),
                public_key,
                created_at: now,
                last_seen_at: now,
            });
        }
        save_trusted_devices(&self.inner.trust_path, &updated)?;
        *devices = updated;
        drop(devices);
        if let Some(device) = self
            .inner
            .devices
            .lock()
            .expect("设备列表锁损坏")
            .iter_mut()
            .find(|value| value.device_id == Some(device_id))
        {
            device.trusted = true;
        }
        Ok(())
    }

    pub fn touch_trusted_device(&self, device_id: Uuid, name: &str) -> AppResult<()> {
        let mut devices = self.inner.trusted_devices.lock().expect("信任设备锁损坏");
        let mut updated = devices.clone();
        if let Some(device) = updated
            .iter_mut()
            .find(|value| value.device_id == device_id)
        {
            device.name = name.into();
            device.last_seen_at = now_unix_seconds();
            save_trusted_devices(&self.inner.trust_path, &updated)?;
            *devices = updated;
        }
        Ok(())
    }

    pub fn remove_trusted_device(&self, device_id: Uuid) -> AppResult<Option<String>> {
        let mut devices = self.inner.trusted_devices.lock().expect("信任设备锁损坏");
        let mut updated = devices.clone();
        let removed = updated
            .iter()
            .position(|value| value.device_id == device_id)
            .map(|index| updated.remove(index));
        if removed.is_some() {
            save_trusted_devices(&self.inner.trust_path, &updated)?;
            *devices = updated;
        }
        drop(devices);
        if let Some(device) = self
            .inner
            .devices
            .lock()
            .expect("设备列表锁损坏")
            .iter_mut()
            .find(|value| value.device_id == Some(device_id))
        {
            device.trusted = false;
        }
        Ok(removed.map(|value| value.name))
    }

    pub fn begin_pairing(
        &self,
        device_id: Uuid,
        device_name: String,
        verification_code: String,
    ) -> AppResult<(Uuid, oneshot::Receiver<bool>)> {
        let (sender, receiver) = oneshot::channel();
        let request_id = Uuid::new_v4();
        let mut pending = self.inner.pending_pairings.lock().expect("配对状态锁损坏");
        if pending.len() >= 8 {
            return Err(AppError::Security(
                "待确认的设备配对请求过多，请先处理当前请求".into(),
            ));
        }
        if pending
            .values()
            .any(|value| value.view.device_id == device_id)
        {
            return Err(AppError::Security("该设备已有待确认的配对请求".into()));
        }
        pending.insert(
            request_id,
            PendingPairing {
                view: PairingRequestView {
                    request_id,
                    device_id,
                    device_name,
                    verification_code,
                },
                decision: Some(sender),
            },
        );
        Ok((request_id, receiver))
    }

    pub fn resolve_pairing(&self, request_id: Uuid, accepted: bool) -> AppResult<()> {
        let mut pending = self.inner.pending_pairings.lock().expect("配对状态锁损坏");
        let Some(mut request) = pending.remove(&request_id) else {
            return Err(AppError::InvalidInput("当前没有待确认的设备配对".into()));
        };
        if let Some(sender) = request.decision.take() {
            let _ = sender.send(accepted);
        }
        Ok(())
    }

    pub fn clear_pairing(&self, request_id: Uuid) {
        self.inner
            .pending_pairings
            .lock()
            .expect("配对状态锁损坏")
            .remove(&request_id);
    }

    pub fn cancel_all_pairings(&self) {
        for (_, mut request) in self
            .inner
            .pending_pairings
            .lock()
            .expect("配对状态锁损坏")
            .drain()
        {
            if let Some(sender) = request.decision.take() {
                let _ = sender.send(false);
            }
        }
    }

    pub fn set_network_peer(
        &self,
        bluetooth_generation: Uuid,
        device_id: Uuid,
        offer: NetworkOffer,
    ) -> AppResult<()> {
        let key: [u8; 32] = offer
            .key
            .try_into()
            .map_err(|_| AppError::Protocol("局域网链路密钥长度无效".into()))?;
        if !self.is_bluetooth_generation_active(bluetooth_generation, Some(device_id)) {
            return Err(AppError::NotConnected);
        }
        self.inner
            .network_peers
            .lock()
            .expect("局域网对端状态锁损坏")
            .insert(
                device_id,
                NetworkPeer {
                    bluetooth_generation,
                    device_id,
                    session_id: offer.session_id,
                    key,
                },
            );
        Ok(())
    }

    pub fn network_peer(&self, device_id: Uuid) -> Option<NetworkPeer> {
        self.inner
            .network_peers
            .lock()
            .expect("局域网对端状态锁损坏")
            .get(&device_id)
            .cloned()
    }

    pub fn is_bluetooth_generation_active(
        &self,
        generation: Uuid,
        device_id: Option<Uuid>,
    ) -> bool {
        self.inner
            .connections
            .lock()
            .expect("连接状态锁损坏")
            .get(&generation)
            .is_some_and(|connection| {
                connection.kind == ConnectionKind::Bluetooth
                    && connection.generation == generation
                    && device_id.is_none_or(|value| connection.peer_device_id == Some(value))
            })
    }

    pub fn begin_network_connect(&self, device_id: Uuid) -> bool {
        self.inner
            .network_connecting
            .lock()
            .expect("局域网连接中状态锁损坏")
            .insert(device_id)
    }

    pub fn begin_pending_handshake(&self, handshake_id: Uuid) -> bool {
        let mut pending = self
            .inner
            .pending_handshakes
            .lock()
            .expect("待握手连接锁损坏");
        if pending.len() >= 8 {
            return false;
        }
        pending.insert(handshake_id)
    }

    pub fn finish_pending_handshake(&self, handshake_id: Uuid) {
        self.inner
            .pending_handshakes
            .lock()
            .expect("待握手连接锁损坏")
            .remove(&handshake_id);
    }

    pub fn finish_network_connect(&self, device_id: Uuid) {
        self.inner
            .network_connecting
            .lock()
            .expect("局域网连接中状态锁损坏")
            .remove(&device_id);
    }

    pub fn install_network_connection(
        &self,
        device_id: Uuid,
        sender: TransportSender,
    ) -> AppResult<Uuid> {
        if !self.receiver_enabled() || self.connection_generation_for_peer(device_id).is_none() {
            return Err(AppError::NotConnected);
        }
        let generation = Uuid::new_v4();
        let mut connections = self
            .inner
            .network_connections
            .lock()
            .expect("局域网连接状态锁损坏");
        if let Some(previous) = connections.remove(&device_id) {
            let _ = previous
                .sender
                .try_send(TransportCommand::Close { reason: None });
        }
        connections.insert(device_id, NetworkConnectionMeta { generation, sender });
        self.finish_network_connect(device_id);
        Ok(generation)
    }

    pub fn clear_network_connection(&self, generation: Uuid) -> bool {
        let mut connections = self
            .inner
            .network_connections
            .lock()
            .expect("局域网连接状态锁损坏");
        if let Some(device_id) = connections
            .iter()
            .find(|(_, value)| value.generation == generation)
            .map(|(device_id, _)| *device_id)
        {
            connections.remove(&device_id);
            self.finish_network_connect(device_id);
            return true;
        }
        false
    }

    pub fn has_network_connection(&self, device_id: Uuid) -> bool {
        self.inner
            .network_connections
            .lock()
            .expect("局域网连接状态锁损坏")
            .contains_key(&device_id)
    }

    fn disconnect_network(&self, device_id: Uuid) {
        self.inner
            .network_peers
            .lock()
            .expect("局域网对端状态锁损坏")
            .remove(&device_id);
        self.finish_network_connect(device_id);
        if let Some(connection) = self
            .inner
            .network_connections
            .lock()
            .expect("局域网连接状态锁损坏")
            .remove(&device_id)
        {
            let _ = connection
                .sender
                .try_send(TransportCommand::Close { reason: None });
        }
    }

    fn disconnect_all_networks(&self) {
        self.inner
            .network_peers
            .lock()
            .expect("局域网对端状态锁损坏")
            .clear();
        self.inner
            .network_connecting
            .lock()
            .expect("局域网连接中状态锁损坏")
            .clear();
        for (_, connection) in self
            .inner
            .network_connections
            .lock()
            .expect("局域网连接状态锁损坏")
            .drain()
        {
            let _ = connection
                .sender
                .try_send(TransportCommand::Close { reason: None });
        }
    }

    pub async fn cancel_all_incoming(&self, reason: &str) {
        self.cleanup_incoming_with_state(None, reason, TransferState::Canceled)
            .await;
    }

    async fn cleanup_incoming_with_state(
        &self,
        only_device: Option<Uuid>,
        reason: &str,
        final_state: TransferState,
    ) {
        let incoming = {
            let mut guard = self.inner.incoming.lock().await;
            if let Some(device_id) = only_device {
                let keys = guard
                    .keys()
                    .filter_map(|key| (key.0 == device_id).then_some(*key))
                    .collect::<Vec<_>>();
                keys.into_iter()
                    .filter_map(|key| guard.remove(&key).map(|value| (key, value)))
                    .collect::<HashMap<_, _>>()
            } else {
                std::mem::take(&mut *guard)
            }
        };
        for ((peer_device_id, transfer_id), transfer) in incoming {
            let temporary_path = transfer.temporary_path.clone();
            let final_path = transfer.final_path.clone();
            drop(transfer.file);
            let _ = tokio::fs::remove_file(&temporary_path).await;
            self.inner
                .reserved_destinations
                .lock()
                .await
                .remove(&final_path);
            if final_state == TransferState::Canceled {
                self.mark_canceled_incoming(peer_device_id, transfer_id);
            }
            self.update_transfer(
                transfer_id,
                None,
                Some(final_state.clone()),
                Some(reason.to_string()),
            );
        }
    }

    pub async fn cleanup_incoming_for_peer(&self, device_id: Uuid, reason: &str) {
        self.cleanup_incoming_with_state(Some(device_id), reason, TransferState::Failed)
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{identity::DeviceIdentity, protocol::Message, transport::transport_channel};

    fn test_state() -> AppState {
        AppState::new(AppStateConfig {
            device_id: Uuid::nil(),
            device_name: "test".into(),
            receive_directory: PathBuf::from("received"),
            legacy_receive_directory: None,
            settings_path: PathBuf::from("settings.json"),
            trust_path: PathBuf::from("trusted-devices.json"),
            identity: DeviceIdentity {
                private_key: vec![1; 32],
                public_key: vec![2; 32],
            },
            trusted_devices: Vec::new(),
            clipboard_enabled: false,
            autostart_enabled: false,
            lan_enabled: false,
            lan_setup_decided: true,
        })
    }

    #[test]
    fn data_prefers_network_while_control_stays_on_bluetooth() {
        let state = test_state();
        let peer_device_id = Uuid::new_v4();
        let (bluetooth_sender, mut bluetooth_receiver) = transport_channel(4);
        state
            .install_connection(
                bluetooth_sender,
                ConnectionKind::Bluetooth,
                "peer",
                Some(peer_device_id),
                None,
                None,
            )
            .expect("应安装蓝牙连接");
        let (network_sender, mut network_receiver) = transport_channel(4);
        state
            .install_network_connection(peer_device_id, network_sender)
            .expect("应安装局域网连接");

        tauri::async_runtime::block_on(state.send_frame_to(
            peer_device_id,
            Frame::new(Message::ShareRootsRequest {
                request_id: Uuid::nil(),
            }),
        ))
        .expect("数据消息应发送");
        assert!(matches!(
            network_receiver.try_recv(),
            Ok(TransportCommand::Send(_))
        ));
        assert!(bluetooth_receiver.try_recv().is_err());

        tauri::async_runtime::block_on(state.send_frame_to(
            peer_device_id,
            Frame::new(Message::Ping { nonce: Uuid::nil() }),
        ))
        .expect("控制消息应发送");
        assert!(matches!(
            bluetooth_receiver.try_recv(),
            Ok(TransportCommand::Send(_))
        ));
        assert!(network_receiver.try_recv().is_err());
    }

    #[test]
    fn unavailable_network_queue_falls_back_to_bluetooth() {
        let state = test_state();
        let peer_device_id = Uuid::new_v4();
        let (bluetooth_sender, mut bluetooth_receiver) = transport_channel(4);
        state
            .install_connection(
                bluetooth_sender,
                ConnectionKind::Bluetooth,
                "peer",
                Some(peer_device_id),
                None,
                None,
            )
            .expect("应安装蓝牙连接");
        let (network_sender, network_receiver) = transport_channel(1);
        drop(network_receiver);
        state
            .install_network_connection(peer_device_id, network_sender)
            .expect("应安装局域网连接");

        tauri::async_runtime::block_on(state.send_frame_to(
            peer_device_id,
            Frame::new(Message::ShareRootsRequest {
                request_id: Uuid::nil(),
            }),
        ))
        .expect("局域网失效时应回退蓝牙");

        assert!(matches!(
            bluetooth_receiver.try_recv(),
            Ok(TransportCommand::Send(_))
        ));
    }

    #[test]
    fn removes_only_the_selected_terminal_transfer() {
        let state = test_state();
        let completed_id = Uuid::new_v4();
        let failed_id = Uuid::new_v4();
        state.add_transfer(TransferView::new(
            completed_id,
            (Uuid::nil(), "测试设备".into()),
            "已完成.txt".into(),
            crate::models::TransferDirection::Sending,
            TransferState::Completed,
            128,
            "已完成".into(),
        ));
        state.add_transfer(TransferView::new(
            failed_id,
            (Uuid::nil(), "测试设备".into()),
            "失败.txt".into(),
            crate::models::TransferDirection::Receiving,
            TransferState::Failed,
            256,
            "失败".into(),
        ));

        assert_eq!(
            state
                .remove_transfer(completed_id)
                .expect("应能删除指定的终态任务"),
            "已完成.txt"
        );
        let transfers = state.inner.transfers.lock().expect("传输状态锁损坏");
        assert_eq!(transfers.len(), 1);
        assert_eq!(transfers[0].id, failed_id);
    }

    #[test]
    fn refuses_to_delete_an_active_transfer() {
        let state = test_state();
        let transfer_id = Uuid::new_v4();
        state.add_transfer(TransferView::new(
            transfer_id,
            (Uuid::nil(), "测试设备".into()),
            "发送中.txt".into(),
            crate::models::TransferDirection::Sending,
            TransferState::Transferring,
            128,
            "正在发送".into(),
        ));

        let error = state
            .remove_transfer(transfer_id)
            .expect_err("进行中任务不能被误当成取消任务删除");
        assert!(error.to_string().contains("完成后才能删除"));
        assert_eq!(
            state.inner.transfers.lock().expect("传输状态锁损坏").len(),
            1
        );
    }

    #[test]
    fn limits_active_and_pending_connections_to_eight_each() {
        let state = test_state();
        let mut receivers = Vec::new();
        for index in 0..8_u128 {
            let (sender, receiver) = transport_channel(1);
            receivers.push(receiver);
            state
                .install_connection(
                    sender,
                    ConnectionKind::Lan,
                    format!("peer-{index}"),
                    Some(Uuid::from_u128(index + 1)),
                    None,
                    None,
                )
                .expect("前八台设备应允许连接");
            assert!(state.begin_pending_handshake(Uuid::from_u128(index + 100)));
        }

        let (ninth_sender, ninth_receiver) = transport_channel(1);
        receivers.push(ninth_receiver);
        assert!(
            state
                .install_connection(
                    ninth_sender,
                    ConnectionKind::Lan,
                    "peer-9",
                    Some(Uuid::from_u128(9)),
                    None,
                    None,
                )
                .is_err()
        );
        assert!(!state.begin_pending_handshake(Uuid::from_u128(999)));
    }
}
