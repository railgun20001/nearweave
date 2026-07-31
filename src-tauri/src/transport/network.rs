use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};

use chacha20poly1305::{
    ChaCha20Poly1305, Key, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use if_addrs::IfAddr;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use snow::{Builder, HandshakeState, TransportState, params::NoiseParams};
use tauri::AppHandle;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{Mutex as AsyncMutex, watch},
};
use uuid::Uuid;

use crate::{
    error::{AppError, AppResult},
    handlers::handle_frame,
    identity::fingerprint,
    models::{ConnectionKind, NoticeLevel},
    protocol::{
        CAPABILITY_LAZY_DIRECTORY, CAPABILITY_TRANSFER_CANCEL, FRAME_PREFIX_SIZE, Frame,
        MAX_HEADER_SIZE, MAX_PAYLOAD_SIZE, Message, NetworkOffer, PROTOCOL_VERSION,
    },
    state::{AppState, NetworkPeer, ReconnectTarget, TrustMatch},
    transport::{TransportCommand, TransportReceiver, TransportSender, transport_channel},
};

pub const DISCOVERY_PORT: u16 = 37991;
pub const PREFERRED_TCP_PORT: u16 = 37992;

const DISCOVERY_MAGIC: &str = "nearweave-discovery-v1";
const LAN_AEAD_AAD: &[u8] = b"nearweave-lan-aead-v1";
const DISCOVERY_VERSION: u16 = 1;
const PURE_LAN_STREAM_MAGIC: &[u8; 4] = b"NWL1";
const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_SHA256";
const OUTBOUND_QUEUE_CAPACITY: usize = 64;
const NETWORK_CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
const NETWORK_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(8);
const PAIRING_TIMEOUT: Duration = Duration::from_secs(60);
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(5);
const DISCOVERY_TTL: Duration = Duration::from_secs(7);
const MANUAL_QUERY_TIMEOUT: Duration = Duration::from_secs(3);
const NETWORK_PACKET_OVERHEAD: usize = 12 + 16;
const MAX_NETWORK_PACKET_SIZE: usize =
    FRAME_PREFIX_SIZE + MAX_HEADER_SIZE + MAX_PAYLOAD_SIZE + NETWORK_PACKET_OVERHEAD;
const MAX_DISCOVERY_PACKET_SIZE: usize = 1024;
const MAX_HANDSHAKE_PACKET_SIZE: usize = 4096;
const MAX_NOISE_PACKET_SIZE: usize = 65_535;
const NOISE_FRAME_CHUNK_SIZE: usize = 60 * 1024;
const MAX_ENCODED_FRAME_SIZE: usize = FRAME_PREFIX_SIZE + MAX_HEADER_SIZE + MAX_PAYLOAD_SIZE;

#[derive(Clone)]
pub struct NetworkRuntime {
    pub(crate) session_id: Uuid,
    pub(crate) key: [u8; 32],
    pub(crate) tcp_port: u16,
    instance_id: Uuid,
    discovery_socket: Arc<Mutex<Option<Arc<UdpSocket>>>>,
    used_handshakes: Arc<Mutex<HashSet<Uuid>>>,
    shutdown: watch::Sender<bool>,
}

impl NetworkRuntime {
    pub fn discovery_available(&self) -> bool {
        self.discovery_socket
            .lock()
            .expect("局域网发现套接字锁损坏")
            .is_some()
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RfcommLanDiscoveryPacket {
    magic: String,
    device_id: Uuid,
    session_id: Uuid,
    tcp_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DiscoveryPacket {
    Announcement {
        magic: String,
        version: u16,
        device_id: Uuid,
        device_name: String,
        tcp_port: u16,
        fingerprint: String,
        instance_id: Uuid,
    },
    Query {
        magic: String,
        version: u16,
        request_id: Uuid,
    },
    Response {
        magic: String,
        version: u16,
        request_id: Uuid,
        device_id: Uuid,
        device_name: String,
        tcp_port: u16,
        fingerprint: String,
        instance_id: Uuid,
    },
}

#[derive(Debug, Clone)]
struct DiscoveredLanPeer {
    device_id: Uuid,
    device_name: String,
    endpoint: SocketAddr,
    fingerprint: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LanHandshakeMessage {
    Hello {
        device_id: Uuid,
        device_name: String,
        tcp_port: u16,
    },
    Trust {
        status: WireTrustStatus,
    },
    Decision {
        accepted: bool,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum WireTrustStatus {
    Trusted,
    Unknown,
    IdentityChanged,
}

struct EstablishedLan {
    stream: TcpStream,
    noise: Arc<AsyncMutex<TransportState>>,
    peer_device_id: Uuid,
    peer_name: String,
    peer_tcp_port: u16,
    peer_fingerprint: String,
}

pub async fn start_network_transport(app: AppHandle, state: AppState) -> AppResult<()> {
    let listener = bind_tcp_listener().await?;
    let tcp_port = listener.local_addr()?.port();
    let (discovery_socket, discovery_error) = bind_discovery_socket().await;
    let (shutdown, shutdown_receiver) = watch::channel(false);

    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let mut key = [0_u8; 32];
    key[..16].copy_from_slice(first.as_bytes());
    key[16..].copy_from_slice(second.as_bytes());
    let runtime = NetworkRuntime {
        session_id: Uuid::new_v4(),
        key,
        tcp_port,
        instance_id: Uuid::new_v4(),
        discovery_socket: Arc::new(Mutex::new(discovery_socket.clone())),
        used_handshakes: Arc::new(Mutex::new(HashSet::new())),
        shutdown,
    };
    state.install_network_runtime(
        runtime.clone(),
        discovery_error.clone(),
        local_connection_addresses(tcp_port),
    );

    let accept_app = app.clone();
    let accept_state = state.clone();
    let accept_shutdown = shutdown_receiver.clone();
    tauri::async_runtime::spawn(async move {
        accept_loop(accept_app, accept_state, listener, accept_shutdown).await;
    });
    let maintenance_app = app.clone();
    let maintenance_state = state.clone();
    let maintenance_shutdown = shutdown_receiver.clone();
    tauri::async_runtime::spawn(async move {
        network_maintenance_loop(maintenance_app, maintenance_state, maintenance_shutdown).await;
    });
    if let Some(socket) = discovery_socket {
        let receive_app = app.clone();
        let receive_state = state.clone();
        let receive_socket = socket.clone();
        let receive_shutdown = shutdown_receiver.clone();
        tauri::async_runtime::spawn(async move {
            discovery_loop(receive_app, receive_state, receive_socket, receive_shutdown).await;
        });
        tauri::async_runtime::spawn(async move {
            announcement_loop(state, socket, shutdown_receiver).await;
        });
    }
    Ok(())
}

pub async fn stop_network_transport(state: &AppState) {
    if let Some(runtime) = state.take_network_runtime() {
        let _ = runtime.shutdown.send(true);
    }
}

pub async fn retry_network_discovery(app: AppHandle, state: AppState) -> AppResult<()> {
    let runtime = state
        .network_runtime()
        .ok_or_else(|| AppError::Protocol("局域网 TCP 服务尚未就绪".into()))?;
    if runtime.discovery_available() {
        state.set_discovery_error(None);
        return Ok(());
    }
    let (socket, error) = bind_discovery_socket().await;
    let Some(socket) = socket else {
        let detail = error.unwrap_or_else(|| "局域网发现服务不可用".into());
        state.set_discovery_error(Some(detail.clone()));
        return Err(AppError::Protocol(detail));
    };
    *runtime
        .discovery_socket
        .lock()
        .expect("局域网发现套接字锁损坏") = Some(socket.clone());
    state.set_discovery_error(None);

    let receive_app = app.clone();
    let receive_state = state.clone();
    let receive_socket = socket.clone();
    let receive_shutdown = runtime.shutdown.subscribe();
    tauri::async_runtime::spawn(async move {
        discovery_loop(receive_app, receive_state, receive_socket, receive_shutdown).await;
    });
    tauri::async_runtime::spawn(async move {
        announcement_loop(state, socket, runtime.shutdown.subscribe()).await;
    });
    Ok(())
}

async fn bind_tcp_listener() -> AppResult<TcpListener> {
    match TcpListener::bind((Ipv4Addr::UNSPECIFIED, PREFERRED_TCP_PORT)).await {
        Ok(listener) => Ok(listener),
        Err(preferred_error) => Ok(TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0))
            .await
            .map_err(|dynamic_error| {
                AppError::Io(std::io::Error::other(format!(
                    "固定端口 {PREFERRED_TCP_PORT} 被占用（{preferred_error}），动态端口也无法监听（{dynamic_error}）"
                )))
            })?),
    }
}

async fn bind_discovery_socket() -> (Option<Arc<UdpSocket>>, Option<String>) {
    match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT)).await {
        Ok(socket) => match socket.set_broadcast(true) {
            Ok(()) => (Some(Arc::new(socket)), None),
            Err(error) => (None, Some(format!("无法启用局域网广播：{error}"))),
        },
        Err(error) => (
            None,
            Some(format!(
                "局域网发现端口 UDP {DISCOVERY_PORT} 被占用或不可用：{error}"
            )),
        ),
    }
}

pub fn network_offer(state: &AppState) -> Option<NetworkOffer> {
    state.network_runtime().map(|runtime| NetworkOffer {
        session_id: runtime.session_id,
        key: runtime.key.to_vec(),
    })
}

pub fn negotiate_network(
    app: &AppHandle,
    state: &AppState,
    bluetooth_generation: Uuid,
    peer_device_id: Uuid,
    offer: NetworkOffer,
) -> AppResult<()> {
    if state.connection_kind(bluetooth_generation) != Some(ConnectionKind::Bluetooth) {
        return Ok(());
    }
    state.set_network_peer(bluetooth_generation, peer_device_id, offer)?;
    announce_rfcomm_lan(app.clone(), state.clone());
    Ok(())
}

pub async fn connect_lan_target(
    app: AppHandle,
    state: AppState,
    target: ReconnectTarget,
    reconnect_session: Uuid,
) -> AppResult<()> {
    let endpoint = resolve_target_endpoint(&state, &target)
        .ok_or_else(|| AppError::InvalidInput("设备当前没有可用的局域网地址".into()))?;
    if !state.is_reconnect_session_active(reconnect_session) {
        return Err(AppError::NotConnected);
    }
    let mut stream = tokio::time::timeout(NETWORK_CONNECT_TIMEOUT, TcpStream::connect(endpoint))
        .await
        .map_err(|_| AppError::Protocol(format!("连接局域网设备 {endpoint} 超时")))??;
    stream.set_nodelay(true)?;
    stream.write_all(PURE_LAN_STREAM_MAGIC).await?;
    stream.flush().await?;
    let established =
        establish_noise_session(app.clone(), state.clone(), stream, true, target.device_id).await?;
    state.update_reconnect_target(
        reconnect_session,
        established.peer_device_id,
        established.peer_name.clone(),
        Some(endpoint),
    );
    attach_primary_lan(app, state, established, Some(reconnect_session), endpoint).await
}

pub async fn resolve_manual_target(state: &AppState, input: &str) -> AppResult<ReconnectTarget> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidInput("请输入 IP 地址或 IP:端口".into()));
    }
    if let Ok(endpoint) = trimmed.parse::<SocketAddr>() {
        return Ok(ReconnectTarget {
            device_id: None,
            display_name: endpoint.ip().to_string(),
            lan_endpoint: Some(endpoint),
            bluetooth_endpoint: None,
        });
    }
    let ip = trimmed
        .parse::<IpAddr>()
        .map_err(|_| AppError::InvalidInput("请输入有效的 IPv4、IPv6 或 IP:端口".into()))?;
    let peer = query_device(ip).await?;
    state.upsert_lan_device(
        peer.device_id,
        peer.device_name.clone(),
        peer.endpoint,
        &peer.fingerprint,
    );
    Ok(ReconnectTarget {
        device_id: Some(peer.device_id),
        display_name: peer.device_name,
        lan_endpoint: Some(peer.endpoint),
        bluetooth_endpoint: None,
    })
}

fn resolve_target_endpoint(state: &AppState, target: &ReconnectTarget) -> Option<SocketAddr> {
    target
        .device_id
        .and_then(|device_id| state.find_device(&device_id.to_string()))
        .and_then(|device| device.lan_endpoint)
        .and_then(|value| value.parse().ok())
        .or(target.lan_endpoint)
}

async fn query_device(ip: IpAddr) -> AppResult<DiscoveredLanPeer> {
    let bind_address = match ip {
        IpAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        IpAddr::V6(_) => "[::]:0".parse().expect("固定 IPv6 任意地址必须有效"),
    };
    let socket = UdpSocket::bind(bind_address).await?;
    let request_id = Uuid::new_v4();
    let payload = serde_json::to_vec(&DiscoveryPacket::Query {
        magic: DISCOVERY_MAGIC.into(),
        version: DISCOVERY_VERSION,
        request_id,
    })?;
    socket
        .send_to(&payload, SocketAddr::new(ip, DISCOVERY_PORT))
        .await?;

    let mut buffer = [0_u8; MAX_DISCOVERY_PACKET_SIZE];
    let (length, source) =
        tokio::time::timeout(MANUAL_QUERY_TIMEOUT, socket.recv_from(&mut buffer))
            .await
            .map_err(|_| {
                AppError::Protocol(format!(
                    "未收到 {ip} 的局域网查询响应，请确认对方已开启 NearWeave 连接，或输入 IP:端口"
                ))
            })??;
    let packet: DiscoveryPacket = serde_json::from_slice(&buffer[..length])?;
    match packet {
        DiscoveryPacket::Response {
            magic,
            version,
            request_id: response_id,
            device_id,
            device_name,
            tcp_port,
            fingerprint,
            ..
        } if magic == DISCOVERY_MAGIC
            && version == DISCOVERY_VERSION
            && response_id == request_id
            && tcp_port != 0 =>
        {
            validate_discovery_identity(&device_name, &fingerprint)?;
            Ok(DiscoveredLanPeer {
                device_id,
                device_name,
                endpoint: SocketAddr::new(source.ip(), tcp_port),
                fingerprint,
            })
        }
        _ => Err(AppError::Protocol("局域网查询响应无效".into())),
    }
}

async fn announcement_loop(
    state: AppState,
    socket: Arc<UdpSocket>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(DISCOVERY_INTERVAL);
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
                continue;
            }
        }
        if state.receiver_enabled()
            && let Some(runtime) = state.network_runtime()
            && let Ok(payload) = serde_json::to_vec(&announcement_packet(&state, &runtime))
        {
            for target in local_broadcast_targets() {
                let _ = socket.send_to(&payload, target).await;
            }
        }
    }
}

async fn network_maintenance_loop(
    app: AppHandle,
    state: AppState,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(DISCOVERY_INTERVAL);
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
                continue;
            }
        }
        let mut changed = state.expire_lan_devices(DISCOVERY_TTL);
        if let Some(runtime) = state.network_runtime() {
            changed |= state.update_local_addresses(local_connection_addresses(runtime.tcp_port));
        }
        if changed {
            state.emit_snapshot(&app);
        }
    }
}

fn announcement_packet(state: &AppState, runtime: &NetworkRuntime) -> DiscoveryPacket {
    DiscoveryPacket::Announcement {
        magic: DISCOVERY_MAGIC.into(),
        version: DISCOVERY_VERSION,
        device_id: state.inner.device_id,
        device_name: state.inner.device_name.clone(),
        tcp_port: runtime.tcp_port,
        fingerprint: state.inner.identity.fingerprint(),
        instance_id: runtime.instance_id,
    }
}

fn response_packet(
    state: &AppState,
    runtime: &NetworkRuntime,
    request_id: Uuid,
) -> DiscoveryPacket {
    DiscoveryPacket::Response {
        magic: DISCOVERY_MAGIC.into(),
        version: DISCOVERY_VERSION,
        request_id,
        device_id: state.inner.device_id,
        device_name: state.inner.device_name.clone(),
        tcp_port: runtime.tcp_port,
        fingerprint: state.inner.identity.fingerprint(),
        instance_id: runtime.instance_id,
    }
}

async fn discovery_loop(
    app: AppHandle,
    state: AppState,
    socket: Arc<UdpSocket>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut buffer = [0_u8; MAX_DISCOVERY_PACKET_SIZE];
    loop {
        let received = tokio::select! {
            result = socket.recv_from(&mut buffer) => result,
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
                continue;
            }
        };
        let (length, source) = match received {
            Ok(value) => value,
            Err(error) => {
                if let Some(runtime) = state.network_runtime() {
                    let mut active = runtime
                        .discovery_socket
                        .lock()
                        .expect("局域网发现套接字锁损坏");
                    if active
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &socket))
                    {
                        active.take();
                    }
                }
                state.set_discovery_error(Some(format!("局域网发现接收失败：{error}")));
                state.emit_snapshot(&app);
                return;
            }
        };
        if let Ok(packet) = serde_json::from_slice::<DiscoveryPacket>(&buffer[..length]) {
            match packet {
                DiscoveryPacket::Query {
                    magic,
                    version,
                    request_id,
                } if magic == DISCOVERY_MAGIC
                    && version == DISCOVERY_VERSION
                    && state.receiver_enabled() =>
                {
                    if let Some(runtime) = state.network_runtime()
                        && let Ok(payload) =
                            serde_json::to_vec(&response_packet(&state, &runtime, request_id))
                    {
                        let _ = socket.send_to(&payload, source).await;
                    }
                }
                DiscoveryPacket::Announcement {
                    magic,
                    version,
                    device_id,
                    device_name,
                    tcp_port,
                    fingerprint,
                    ..
                }
                | DiscoveryPacket::Response {
                    magic,
                    version,
                    device_id,
                    device_name,
                    tcp_port,
                    fingerprint,
                    ..
                } if magic == DISCOVERY_MAGIC
                    && version == DISCOVERY_VERSION
                    && device_id != state.inner.device_id
                    && tcp_port != 0
                    && validate_discovery_identity(&device_name, &fingerprint).is_ok() =>
                {
                    state.upsert_lan_device(
                        device_id,
                        device_name,
                        SocketAddr::new(source.ip(), tcp_port),
                        &fingerprint,
                    );
                    state.emit_snapshot(&app);
                }
                _ => {}
            }
            continue;
        }

        let Ok(packet) = serde_json::from_slice::<RfcommLanDiscoveryPacket>(&buffer[..length])
        else {
            continue;
        };
        process_rfcomm_lan_discovery(app.clone(), state.clone(), source, packet);
    }
}

fn validate_discovery_identity(device_name: &str, public_fingerprint: &str) -> AppResult<()> {
    if device_name.trim().is_empty() || device_name.chars().count() > 128 {
        return Err(AppError::Protocol("局域网设备名称无效".into()));
    }
    if public_fingerprint.len() != 64
        || !public_fingerprint
            .bytes()
            .all(|value| value.is_ascii_hexdigit())
    {
        return Err(AppError::Protocol("局域网设备身份指纹无效".into()));
    }
    Ok(())
}

fn announce_rfcomm_lan(app: AppHandle, state: AppState) {
    let Some(runtime) = state.network_runtime() else {
        return;
    };
    let Some(socket) = runtime
        .discovery_socket
        .lock()
        .expect("局域网发现套接字锁损坏")
        .clone()
    else {
        state.emit_notice(
            &app,
            NoticeLevel::Info,
            format!("UDP {DISCOVERY_PORT} 不可用，无法建立蓝牙加速链路"),
        );
        return;
    };
    let packet = RfcommLanDiscoveryPacket {
        magic: DISCOVERY_MAGIC.into(),
        device_id: state.inner.device_id,
        session_id: runtime.session_id,
        tcp_port: runtime.tcp_port,
    };
    let Ok(payload) = serde_json::to_vec(&packet) else {
        return;
    };

    tauri::async_runtime::spawn(async move {
        for _ in 0..6 {
            for target in local_broadcast_targets() {
                if let Err(error) = socket.send_to(&payload, target).await {
                    state.emit_notice(
                        &app,
                        NoticeLevel::Info,
                        format!("局域网高速链路广播失败：{error}"),
                    );
                }
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
    });
}

fn process_rfcomm_lan_discovery(
    app: AppHandle,
    state: AppState,
    source: SocketAddr,
    packet: RfcommLanDiscoveryPacket,
) {
    if packet.magic != DISCOVERY_MAGIC || packet.device_id == state.inner.device_id {
        return;
    }
    let Some(peer) = state.network_peer(packet.device_id) else {
        return;
    };
    if peer.device_id != packet.device_id || peer.session_id != packet.session_id {
        return;
    }
    if state.inner.device_id.as_bytes() >= peer.device_id.as_bytes()
        || state.has_network_connection(peer.device_id)
        || !state.begin_network_connect(peer.device_id)
    {
        return;
    }

    let target = SocketAddr::new(source.ip(), packet.tcp_port);
    tauri::async_runtime::spawn(async move {
        if let Err(error) = connect_rfcomm_lan(app.clone(), state.clone(), target, peer).await {
            state.finish_network_connect(packet.device_id);
            state.emit_notice(
                &app,
                NoticeLevel::Info,
                format!("局域网高速链路连接失败，继续使用蓝牙：{error}"),
            );
        }
    });
}

async fn connect_rfcomm_lan(
    app: AppHandle,
    state: AppState,
    target: SocketAddr,
    peer: NetworkPeer,
) -> AppResult<()> {
    let mut stream = tokio::time::timeout(NETWORK_CONNECT_TIMEOUT, TcpStream::connect(target))
        .await
        .map_err(|_| AppError::Protocol("连接局域网链路超时".into()))??;
    stream.set_nodelay(true)?;
    let hello = Frame::new(Message::NetworkHello {
        device_id: state.inner.device_id,
        session_id: peer.session_id,
        connection_id: Uuid::new_v4(),
    });
    write_encrypted_frame(&mut stream, &peer.key, &hello).await?;
    attach_rfcomm_lan(app, state, stream, peer.key, peer.bluetooth_generation).await
}

async fn accept_loop(
    app: AppHandle,
    state: AppState,
    listener: TcpListener,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        let accepted = tokio::select! {
            result = listener.accept() => result,
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
                continue;
            }
        };
        let Ok((mut stream, _source)) = accepted else {
            return;
        };
        let handshake_id = Uuid::new_v4();
        if !state.begin_pending_handshake(handshake_id) {
            state.emit_notice(&app, NoticeLevel::Info, "待握手连接已达 8 个，已拒绝新连接");
            continue;
        }
        let connection_app = app.clone();
        let connection_state = state.clone();
        tauri::async_runtime::spawn(async move {
            let result = async {
                stream.set_nodelay(true)?;
                let mut prefix = [0_u8; 4];
                tokio::time::timeout(NETWORK_HANDSHAKE_TIMEOUT, stream.read_exact(&mut prefix))
                    .await
                    .map_err(|_| AppError::Protocol("局域网连接未及时发送握手".into()))??;
                if &prefix == PURE_LAN_STREAM_MAGIC {
                    if !connection_state.receiver_enabled() {
                        return Err(AppError::Protocol("对方尚未开启 NearWeave 连接".into()));
                    }
                    let endpoint = stream.peer_addr()?;
                    let established = establish_noise_session(
                        connection_app.clone(),
                        connection_state.clone(),
                        stream,
                        false,
                        None,
                    )
                    .await?;
                    attach_primary_lan(
                        connection_app.clone(),
                        connection_state.clone(),
                        established,
                        None,
                        endpoint,
                    )
                    .await
                } else {
                    accept_rfcomm_lan(
                        connection_app.clone(),
                        connection_state.clone(),
                        stream,
                        prefix,
                    )
                    .await
                }
            }
            .await;
            connection_state.finish_pending_handshake(handshake_id);
            if let Err(error) = result {
                connection_state.emit_notice(
                    &connection_app,
                    NoticeLevel::Info,
                    format!("已拒绝无效的局域网连接：{error}"),
                );
            }
        });
    }
}

async fn accept_rfcomm_lan(
    app: AppHandle,
    state: AppState,
    mut stream: TcpStream,
    length_prefix: [u8; 4],
) -> AppResult<()> {
    let runtime = state
        .network_runtime()
        .ok_or_else(|| AppError::Protocol("局域网服务尚未就绪".into()))?;
    let frame = tokio::time::timeout(
        NETWORK_HANDSHAKE_TIMEOUT,
        read_encrypted_frame_with_prefix(&mut stream, &runtime.key, length_prefix),
    )
    .await
    .map_err(|_| AppError::Protocol("局域网链路握手超时".into()))??;

    let (device_id, session_id, connection_id) = match frame.message {
        Message::NetworkHello {
            device_id,
            session_id,
            connection_id,
        } if frame.payload.is_empty() => (device_id, session_id, connection_id),
        _ => return Err(AppError::Protocol("局域网链路缺少有效握手".into())),
    };
    if session_id != runtime.session_id {
        return Err(AppError::Protocol("局域网链路会话已过期".into()));
    }
    if !runtime
        .used_handshakes
        .lock()
        .expect("局域网握手记录锁损坏")
        .insert(connection_id)
    {
        return Err(AppError::Protocol("局域网链路握手已被使用".into()));
    }
    let peer = state
        .network_peer(device_id)
        .ok_or_else(|| AppError::Protocol("局域网设备未通过当前蓝牙连接授权".into()))?;
    if !state.is_bluetooth_generation_active(peer.bluetooth_generation, Some(device_id)) {
        return Err(AppError::NotConnected);
    }
    attach_rfcomm_lan(app, state, stream, runtime.key, peer.bluetooth_generation).await
}

async fn establish_noise_session(
    app: AppHandle,
    state: AppState,
    mut stream: TcpStream,
    initiator: bool,
    expected_device_id: Option<Uuid>,
) -> AppResult<EstablishedLan> {
    let params: NoiseParams = NOISE_PATTERN
        .parse()
        .map_err(|error| AppError::Security(format!("Noise 参数无效：{error}")))?;
    let builder = Builder::new(params)
        .local_private_key(&state.inner.identity.private_key)
        .map_err(|error| AppError::Security(format!("设备身份私钥无效：{error}")))?;
    let mut handshake = if initiator {
        builder.build_initiator()
    } else {
        builder.build_responder()
    }
    .map_err(|error| AppError::Security(format!("无法初始化 Noise 握手：{error}")))?;

    tokio::time::timeout(
        NETWORK_HANDSHAKE_TIMEOUT,
        perform_noise_handshake(&mut stream, &mut handshake, initiator),
    )
    .await
    .map_err(|_| AppError::Security("Noise 握手超时".into()))??;

    let handshake_hash = handshake.get_handshake_hash().to_vec();
    let remote_public_key = handshake
        .get_remote_static()
        .ok_or_else(|| AppError::Security("Noise 握手未提供远端静态公钥".into()))?
        .to_vec();
    let remote_fingerprint = fingerprint(&remote_public_key);
    let noise = Arc::new(AsyncMutex::new(handshake.into_transport_mode().map_err(
        |error| AppError::Security(format!("Noise 握手无法进入传输模式：{error}")),
    )?));

    let local_hello = LanHandshakeMessage::Hello {
        device_id: state.inner.device_id,
        device_name: state.inner.device_name.clone(),
        tcp_port: state
            .network_runtime()
            .map(|runtime| runtime.tcp_port)
            .ok_or_else(|| AppError::Protocol("本机局域网接收服务不可用".into()))?,
    };
    let remote_hello = if initiator {
        write_noise_control(&mut stream, &noise, &local_hello).await?;
        read_noise_control(&mut stream, &noise).await?
    } else {
        let remote = read_noise_control(&mut stream, &noise).await?;
        write_noise_control(&mut stream, &noise, &local_hello).await?;
        remote
    };
    let (peer_device_id, peer_name, peer_tcp_port) = match remote_hello {
        LanHandshakeMessage::Hello {
            device_id,
            device_name,
            tcp_port,
        } if !device_name.trim().is_empty()
            && device_name.chars().count() <= 128
            && tcp_port != 0 =>
        {
            (device_id, device_name, tcp_port)
        }
        _ => return Err(AppError::Security("远端设备身份信息无效".into())),
    };
    if peer_device_id == state.inner.device_id {
        return Err(AppError::Security("拒绝连接到本机设备身份".into()));
    }
    if expected_device_id.is_some_and(|value| value != peer_device_id) {
        return Err(AppError::Security(
            "局域网发现的设备身份与握手身份不一致".into(),
        ));
    }

    let preferred_connection = if initiator {
        state.inner.device_id.as_bytes() < peer_device_id.as_bytes()
    } else {
        peer_device_id.as_bytes() < state.inner.device_id.as_bytes()
    };
    if !preferred_connection {
        tokio::time::sleep(Duration::from_millis(350)).await;
        if state
            .connection_generation_for_peer(peer_device_id)
            .is_some()
        {
            return Err(AppError::Security("已保留双方同时发起的另一条连接".into()));
        }
    }

    let local_trust = state.trust_match(peer_device_id, &remote_public_key);
    let local_wire_trust = match local_trust {
        TrustMatch::Trusted => WireTrustStatus::Trusted,
        TrustMatch::Unknown => WireTrustStatus::Unknown,
        TrustMatch::IdentityChanged => WireTrustStatus::IdentityChanged,
    };
    let remote_trust = exchange_control(
        &mut stream,
        &noise,
        initiator,
        LanHandshakeMessage::Trust {
            status: local_wire_trust,
        },
    )
    .await?;
    let remote_wire_trust = match remote_trust {
        LanHandshakeMessage::Trust { status } => status,
        _ => return Err(AppError::Security("远端未返回身份信任状态".into())),
    };
    if local_wire_trust == WireTrustStatus::IdentityChanged
        || remote_wire_trust == WireTrustStatus::IdentityChanged
    {
        return Err(AppError::Security(
            "设备身份已变化，请在设置中移除原信任后重新配对".into(),
        ));
    }

    if local_wire_trust != WireTrustStatus::Trusted || remote_wire_trust != WireTrustStatus::Trusted
    {
        let code = verification_code(&handshake_hash, state.inner.device_id, peer_device_id);
        let (request_id, decision) =
            state.begin_pairing(peer_device_id, peer_name.clone(), code)?;
        state.emit_snapshot(&app);
        let accepted = tokio::time::timeout(PAIRING_TIMEOUT, decision)
            .await
            .map_err(|_| AppError::Security("设备配对确认超时".into()))?
            .unwrap_or(false);
        state.clear_pairing(request_id);
        state.emit_snapshot(&app);

        let remote_decision = exchange_control(
            &mut stream,
            &noise,
            initiator,
            LanHandshakeMessage::Decision { accepted },
        )
        .await?;
        let remote_accepted = matches!(
            remote_decision,
            LanHandshakeMessage::Decision { accepted: true }
        );
        if !accepted || !remote_accepted {
            return Err(AppError::Security("设备配对已被拒绝".into()));
        }
        state.remember_trusted_device(peer_device_id, peer_name.clone(), remote_public_key)?;
    } else {
        state.touch_trusted_device(peer_device_id, &peer_name)?;
    }

    Ok(EstablishedLan {
        stream,
        noise,
        peer_device_id,
        peer_name,
        peer_tcp_port,
        peer_fingerprint: remote_fingerprint,
    })
}

async fn perform_noise_handshake(
    stream: &mut TcpStream,
    handshake: &mut HandshakeState,
    initiator: bool,
) -> AppResult<()> {
    let mut message = vec![0_u8; MAX_HANDSHAKE_PACKET_SIZE];
    let mut payload = vec![0_u8; MAX_HANDSHAKE_PACKET_SIZE];
    if initiator {
        let length = handshake
            .write_message(&[], &mut message)
            .map_err(|error| AppError::Security(format!("Noise 握手消息 1 失败：{error}")))?;
        write_plain_packet(stream, &message[..length]).await?;
        let incoming = read_plain_packet(stream, MAX_HANDSHAKE_PACKET_SIZE).await?;
        handshake
            .read_message(&incoming, &mut payload)
            .map_err(|error| AppError::Security(format!("Noise 握手消息 2 无效：{error}")))?;
        let length = handshake
            .write_message(&[], &mut message)
            .map_err(|error| AppError::Security(format!("Noise 握手消息 3 失败：{error}")))?;
        write_plain_packet(stream, &message[..length]).await?;
    } else {
        let incoming = read_plain_packet(stream, MAX_HANDSHAKE_PACKET_SIZE).await?;
        handshake
            .read_message(&incoming, &mut payload)
            .map_err(|error| AppError::Security(format!("Noise 握手消息 1 无效：{error}")))?;
        let length = handshake
            .write_message(&[], &mut message)
            .map_err(|error| AppError::Security(format!("Noise 握手消息 2 失败：{error}")))?;
        write_plain_packet(stream, &message[..length]).await?;
        let incoming = read_plain_packet(stream, MAX_HANDSHAKE_PACKET_SIZE).await?;
        handshake
            .read_message(&incoming, &mut payload)
            .map_err(|error| AppError::Security(format!("Noise 握手消息 3 无效：{error}")))?;
    }
    Ok(())
}

async fn exchange_control(
    stream: &mut TcpStream,
    noise: &Arc<AsyncMutex<TransportState>>,
    initiator: bool,
    local: LanHandshakeMessage,
) -> AppResult<LanHandshakeMessage> {
    if initiator {
        write_noise_control(stream, noise, &local).await?;
        read_noise_control(stream, noise).await
    } else {
        let remote = read_noise_control(stream, noise).await?;
        write_noise_control(stream, noise, &local).await?;
        Ok(remote)
    }
}

async fn write_noise_control(
    stream: &mut TcpStream,
    noise: &Arc<AsyncMutex<TransportState>>,
    message: &LanHandshakeMessage,
) -> AppResult<()> {
    let encoded = serde_json::to_vec(message)?;
    write_noise_payload(stream, noise, &encoded).await
}

async fn read_noise_control(
    stream: &mut TcpStream,
    noise: &Arc<AsyncMutex<TransportState>>,
) -> AppResult<LanHandshakeMessage> {
    let encoded = read_noise_payload(stream, noise).await?;
    if encoded.len() > MAX_HANDSHAKE_PACKET_SIZE {
        return Err(AppError::Security("局域网握手控制消息过大".into()));
    }
    serde_json::from_slice(&encoded).map_err(AppError::from)
}

fn verification_code(handshake_hash: &[u8], first: Uuid, second: Uuid) -> String {
    let (lower, upper) = if first.as_bytes() <= second.as_bytes() {
        (first, second)
    } else {
        (second, first)
    };
    let mut hasher = Sha256::new();
    hasher.update(handshake_hash);
    hasher.update(lower.as_bytes());
    hasher.update(upper.as_bytes());
    hasher.update(b"nearweave-pairing-code-v1");
    let digest = hasher.finalize();
    let value = u32::from_be_bytes(digest[..4].try_into().expect("摘要长度固定")) % 1_000_000;
    format!("{value:06}")
}

async fn attach_primary_lan(
    app: AppHandle,
    state: AppState,
    established: EstablishedLan,
    reconnect_session: Option<Uuid>,
    endpoint: SocketAddr,
) -> AppResult<()> {
    if state
        .connection_generation_for_peer(established.peer_device_id)
        .is_some()
    {
        return Err(AppError::Protocol("该设备已经连接".into()));
    }
    let reconnect_endpoint = SocketAddr::new(endpoint.ip(), established.peer_tcp_port);
    state.upsert_lan_device(
        established.peer_device_id,
        established.peer_name.clone(),
        reconnect_endpoint,
        &established.peer_fingerprint,
    );
    let effective_reconnect_session = if let Some(session) = reconnect_session {
        state.update_reconnect_target(
            session,
            established.peer_device_id,
            established.peer_name.clone(),
            Some(reconnect_endpoint),
        );
        Some(session)
    } else {
        let target = ReconnectTarget {
            device_id: Some(established.peer_device_id),
            display_name: established.peer_name.clone(),
            lan_endpoint: Some(reconnect_endpoint),
            bluetooth_endpoint: state
                .find_device(&established.peer_device_id.to_string())
                .and_then(|device| device.bluetooth_endpoint),
        };
        Some(state.begin_reconnect_session(target))
    };

    let (sender, receiver) = transport_channel(OUTBOUND_QUEUE_CAPACITY);
    let generation = state.install_connection(
        sender.clone(),
        ConnectionKind::Lan,
        established.peer_name.clone(),
        Some(established.peer_device_id),
        None,
        effective_reconnect_session,
    )?;
    let (reader, writer) = tokio::io::split(established.stream);
    spawn_noise_writer(
        app.clone(),
        state.clone(),
        generation,
        writer,
        receiver,
        established.noise.clone(),
    );
    spawn_noise_reader(
        app.clone(),
        state.clone(),
        generation,
        reader,
        sender.clone(),
        established.noise,
    );
    spawn_lan_heartbeat(app.clone(), state.clone(), generation, sender.clone());
    sender
        .send(TransportCommand::Send(Frame::new(Message::Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: state.inner.device_id,
            device_name: state.inner.device_name.clone(),
            capabilities: vec![
                "files".into(),
                "shared_directories".into(),
                "clipboard_text".into(),
                "lan_noise_xx".into(),
                CAPABILITY_TRANSFER_CANCEL.into(),
                CAPABILITY_LAZY_DIRECTORY.into(),
            ],
            network_offer: None,
        })))
        .await
        .map_err(|_| AppError::NotConnected)?;
    state.emit_snapshot(&app);
    state.emit_notice(
        &app,
        NoticeLevel::Success,
        format!("已通过纯局域网连接 {}", established.peer_name),
    );
    Ok(())
}

fn spawn_noise_writer(
    app: AppHandle,
    state: AppState,
    generation: Uuid,
    mut writer: WriteHalf<TcpStream>,
    mut receiver: TransportReceiver,
    noise: Arc<AsyncMutex<TransportState>>,
) {
    tauri::async_runtime::spawn(async move {
        let mut failure = None;
        while let Some(command) = receiver.recv().await {
            match command {
                TransportCommand::Send(frame) => {
                    if let Err(error) = write_noise_frame(&mut writer, &noise, &frame).await {
                        failure = Some(error.to_string());
                        break;
                    }
                }
                TransportCommand::Close { reason } => {
                    failure = reason;
                    break;
                }
            }
        }
        let _ = writer.shutdown().await;
        crate::transport::finish_primary_connection(&app, &state, generation, failure).await;
    });
}

fn spawn_noise_reader(
    app: AppHandle,
    state: AppState,
    generation: Uuid,
    mut reader: ReadHalf<TcpStream>,
    sender: TransportSender,
    noise: Arc<AsyncMutex<TransportState>>,
) {
    tauri::async_runtime::spawn(async move {
        let failure = loop {
            match read_noise_frame(&mut reader, &noise).await {
                Ok(frame) => {
                    if let Err(error) =
                        handle_frame(app.clone(), state.clone(), generation, frame).await
                    {
                        state.emit_notice(&app, NoticeLevel::Error, error.to_string());
                    }
                }
                Err(error) => break Some(error.to_string()),
            }
        };
        let _ = sender.send(TransportCommand::Close { reason: None }).await;
        crate::transport::finish_primary_connection(&app, &state, generation, failure).await;
    });
}

fn spawn_lan_heartbeat(app: AppHandle, state: AppState, generation: Uuid, sender: TransportSender) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(elapsed) = state.pong_elapsed(generation) else {
                break;
            };
            if elapsed >= Duration::from_secs(90) {
                let reason = "90 秒未收到 Pong，局域网连接已判定失联".to_string();
                state.emit_notice(&app, NoticeLevel::Info, &reason);
                let _ = sender
                    .send(TransportCommand::Close {
                        reason: Some(reason),
                    })
                    .await;
                break;
            }
            if sender
                .send(TransportCommand::Send(Frame::new(Message::Ping {
                    nonce: Uuid::new_v4(),
                })))
                .await
                .is_err()
            {
                break;
            }
        }
    });
}

async fn write_noise_frame<W>(
    writer: &mut W,
    noise: &Arc<AsyncMutex<TransportState>>,
    frame: &Frame,
) -> AppResult<()>
where
    W: AsyncWriteExt + Unpin,
{
    let encoded = frame.encode()?;
    for (index, chunk) in encoded.chunks(NOISE_FRAME_CHUNK_SIZE).enumerate() {
        let final_chunk = (index + 1) * NOISE_FRAME_CHUNK_SIZE >= encoded.len();
        let mut payload = Vec::with_capacity(chunk.len() + 1);
        payload.push(u8::from(final_chunk));
        payload.extend_from_slice(chunk);
        write_noise_payload(writer, noise, &payload).await?;
    }
    Ok(())
}

async fn read_noise_frame<R>(
    reader: &mut R,
    noise: &Arc<AsyncMutex<TransportState>>,
) -> AppResult<Frame>
where
    R: AsyncReadExt + Unpin,
{
    let mut encoded = Vec::new();
    loop {
        let payload = read_noise_payload(reader, noise).await?;
        let Some((&final_flag, chunk)) = payload.split_first() else {
            return Err(AppError::Protocol("Noise 数据分片为空".into()));
        };
        if final_flag > 1 {
            return Err(AppError::Protocol("Noise 数据分片标记无效".into()));
        }
        if encoded.len().saturating_add(chunk.len()) > MAX_ENCODED_FRAME_SIZE {
            return Err(AppError::Protocol("Noise 协议帧超过大小限制".into()));
        }
        encoded.extend_from_slice(chunk);
        if final_flag == 1 {
            return Frame::decode(&encoded);
        }
    }
}

async fn write_noise_payload<W>(
    writer: &mut W,
    noise: &Arc<AsyncMutex<TransportState>>,
    payload: &[u8],
) -> AppResult<()>
where
    W: AsyncWriteExt + Unpin,
{
    if payload.len() + 16 > MAX_NOISE_PACKET_SIZE {
        return Err(AppError::Protocol("Noise 明文分片超过大小限制".into()));
    }
    let mut encrypted = vec![0_u8; payload.len() + 16];
    let length = noise
        .lock()
        .await
        .write_message(payload, &mut encrypted)
        .map_err(|error| AppError::Security(format!("Noise 加密失败：{error}")))?;
    write_plain_packet(writer, &encrypted[..length]).await
}

async fn read_noise_payload<R>(
    reader: &mut R,
    noise: &Arc<AsyncMutex<TransportState>>,
) -> AppResult<Vec<u8>>
where
    R: AsyncReadExt + Unpin,
{
    let encrypted = read_plain_packet(reader, MAX_NOISE_PACKET_SIZE).await?;
    let mut payload = vec![0_u8; encrypted.len()];
    let length = noise
        .lock()
        .await
        .read_message(&encrypted, &mut payload)
        .map_err(|error| AppError::Security(format!("Noise 数据认证失败：{error}")))?;
    payload.truncate(length);
    Ok(payload)
}

async fn write_plain_packet<W>(writer: &mut W, packet: &[u8]) -> AppResult<()>
where
    W: AsyncWriteExt + Unpin,
{
    let length =
        u32::try_from(packet.len()).map_err(|_| AppError::Protocol("网络数据包长度溢出".into()))?;
    writer.write_all(&length.to_be_bytes()).await?;
    writer.write_all(packet).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_plain_packet<R>(reader: &mut R, maximum: usize) -> AppResult<Vec<u8>>
where
    R: AsyncReadExt + Unpin,
{
    let length = reader.read_u32().await? as usize;
    if length == 0 || length > maximum {
        return Err(AppError::Protocol(format!(
            "网络数据包长度 {length} 超出限制"
        )));
    }
    let mut packet = vec![0_u8; length];
    reader.read_exact(&mut packet).await?;
    Ok(packet)
}

async fn attach_rfcomm_lan(
    app: AppHandle,
    state: AppState,
    stream: TcpStream,
    key: [u8; 32],
    bluetooth_generation: Uuid,
) -> AppResult<()> {
    if !state.is_bluetooth_generation_active(bluetooth_generation, None) {
        return Err(AppError::NotConnected);
    }
    let device_id = state
        .peer_for_generation(bluetooth_generation)
        .ok_or(AppError::NotConnected)?;
    if state.has_network_connection(device_id) {
        state.finish_network_connect(device_id);
        return Ok(());
    }

    let (sender, receiver) = transport_channel(OUTBOUND_QUEUE_CAPACITY);
    let generation = state.install_network_connection(device_id, sender.clone())?;
    let (reader, writer) = tokio::io::split(stream);
    spawn_rfcomm_lan_writer(
        app.clone(),
        state.clone(),
        generation,
        writer,
        receiver,
        key,
    );
    spawn_rfcomm_lan_reader(
        app.clone(),
        state.clone(),
        generation,
        bluetooth_generation,
        reader,
        sender,
        key,
    );
    state.emit_snapshot(&app);
    state.emit_notice(
        &app,
        NoticeLevel::Success,
        "已建立加密局域网高速链路，数据传输将自动优先使用",
    );
    Ok(())
}

fn spawn_rfcomm_lan_writer(
    app: AppHandle,
    state: AppState,
    generation: Uuid,
    mut writer: WriteHalf<TcpStream>,
    mut receiver: TransportReceiver,
    key: [u8; 32],
) {
    tauri::async_runtime::spawn(async move {
        let mut failure = None;
        while let Some(command) = receiver.recv().await {
            match command {
                TransportCommand::Send(frame) => {
                    if let Err(error) = write_encrypted_frame(&mut writer, &key, &frame).await {
                        failure = Some(error.to_string());
                        break;
                    }
                }
                TransportCommand::Close { reason } => {
                    failure = reason;
                    break;
                }
            }
        }
        let _ = writer.shutdown().await;
        finish_rfcomm_lan(&app, &state, generation, failure);
    });
}

fn spawn_rfcomm_lan_reader(
    app: AppHandle,
    state: AppState,
    generation: Uuid,
    bluetooth_generation: Uuid,
    mut reader: ReadHalf<TcpStream>,
    sender: TransportSender,
    key: [u8; 32],
) {
    tauri::async_runtime::spawn(async move {
        let failure = loop {
            match read_encrypted_frame(&mut reader, &key).await {
                Ok(frame) if frame.message.prefers_network() => {
                    if let Err(error) =
                        handle_frame(app.clone(), state.clone(), bluetooth_generation, frame).await
                    {
                        break Some(error.to_string());
                    }
                }
                Ok(_) => break Some("局域网高速链路收到了控制消息".into()),
                Err(error) => break Some(error.to_string()),
            }
        };
        let _ = sender.send(TransportCommand::Close { reason: None }).await;
        finish_rfcomm_lan(&app, &state, generation, failure);
    });
}

fn finish_rfcomm_lan(app: &AppHandle, state: &AppState, generation: Uuid, failure: Option<String>) {
    if state.clear_network_connection(generation) {
        state.emit_snapshot(app);
        state.emit_notice(
            app,
            NoticeLevel::Info,
            match failure {
                Some(reason) => {
                    format!("局域网高速链路已断开，数据将回退到蓝牙：{reason}")
                }
                None => "局域网高速链路已断开，数据将回退到蓝牙".into(),
            },
        );
    }
}

async fn write_encrypted_frame<W>(writer: &mut W, key: &[u8; 32], frame: &Frame) -> AppResult<()>
where
    W: AsyncWriteExt + Unpin,
{
    let packet = encrypt_frame(key, frame)?;
    write_plain_packet(writer, &packet).await
}

async fn read_encrypted_frame<R>(reader: &mut R, key: &[u8; 32]) -> AppResult<Frame>
where
    R: AsyncReadExt + Unpin,
{
    let packet = read_plain_packet(reader, MAX_NETWORK_PACKET_SIZE).await?;
    if packet.len() < NETWORK_PACKET_OVERHEAD {
        return Err(AppError::Protocol("局域网加密帧不完整".into()));
    }
    decrypt_frame(key, &packet)
}

async fn read_encrypted_frame_with_prefix<R>(
    reader: &mut R,
    key: &[u8; 32],
    prefix: [u8; 4],
) -> AppResult<Frame>
where
    R: AsyncReadExt + Unpin,
{
    let length = u32::from_be_bytes(prefix) as usize;
    if !(NETWORK_PACKET_OVERHEAD..=MAX_NETWORK_PACKET_SIZE).contains(&length) {
        return Err(AppError::Protocol(format!(
            "局域网加密帧长度 {length} 超出限制"
        )));
    }
    let mut packet = vec![0_u8; length];
    reader.read_exact(&mut packet).await?;
    decrypt_frame(key, &packet)
}

fn encrypt_frame(key: &[u8; 32], frame: &Frame) -> AppResult<Vec<u8>> {
    let encoded = frame.encode()?;
    let nonce_source = Uuid::new_v4();
    let nonce_bytes = &nonce_source.as_bytes()[..12];
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let encrypted = cipher
        .encrypt(
            Nonce::from_slice(nonce_bytes),
            Payload {
                msg: &encoded,
                aad: LAN_AEAD_AAD,
            },
        )
        .map_err(|_| AppError::Protocol("无法加密局域网帧".into()))?;
    let mut packet = Vec::with_capacity(NETWORK_PACKET_OVERHEAD + encoded.len());
    packet.extend_from_slice(nonce_bytes);
    packet.extend_from_slice(&encrypted);
    Ok(packet)
}

fn decrypt_frame(key: &[u8; 32], packet: &[u8]) -> AppResult<Frame> {
    if packet.len() < NETWORK_PACKET_OVERHEAD {
        return Err(AppError::Protocol("局域网加密帧不完整".into()));
    }
    let (nonce, ciphertext) = packet.split_at(12);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let encoded = cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: LAN_AEAD_AAD,
            },
        )
        .map_err(|_| AppError::Protocol("局域网帧认证失败".into()))?;
    Frame::decode(&encoded)
}

fn local_connection_addresses(port: u16) -> Vec<String> {
    let mut addresses = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|interface| match interface.addr {
            IfAddr::V4(address)
                if !address.ip.is_loopback()
                    && !address.ip.is_link_local()
                    && !address.ip.is_unspecified() =>
            {
                Some(format!(
                    "{} · {}",
                    interface.name,
                    SocketAddr::new(IpAddr::V4(address.ip), port)
                ))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    addresses.sort();
    addresses.dedup();
    addresses
}

fn local_broadcast_targets() -> Vec<SocketAddr> {
    let mut targets = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|interface| match interface.addr {
            IfAddr::V4(address)
                if !address.ip.is_loopback()
                    && !address.ip.is_link_local()
                    && !address.ip.is_unspecified() =>
            {
                let broadcast = address.broadcast.unwrap_or_else(|| {
                    Ipv4Addr::from(u32::from(address.ip) | !u32::from(address.netmask))
                });
                Some(SocketAddr::from((broadcast, DISCOVERY_PORT)))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    targets.push(SocketAddr::from((Ipv4Addr::BROADCAST, DISCOVERY_PORT)));
    targets.sort_unstable();
    targets.dedup();
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_lan_frame_round_trip_preserves_payload() {
        let key = [7_u8; 32];
        let frame = Frame::with_payload(
            Message::ClipboardText {
                message_id: Uuid::nil(),
                sha256: "test".into(),
            },
            b"hello".to_vec(),
        );

        let encrypted = encrypt_frame(&key, &frame).expect("应加密");
        let decoded = decrypt_frame(&key, &encrypted).expect("应解密");

        assert!(matches!(decoded.message, Message::ClipboardText { .. }));
        assert_eq!(decoded.payload, b"hello");
    }

    #[test]
    fn tampered_lan_frame_is_rejected() {
        let key = [9_u8; 32];
        let mut encrypted = encrypt_frame(
            &key,
            &Frame::new(Message::ShareRootsRequest {
                request_id: Uuid::nil(),
            }),
        )
        .expect("应加密");
        let last = encrypted.len() - 1;
        encrypted[last] ^= 1;

        assert!(decrypt_frame(&key, &encrypted).is_err());
    }

    #[test]
    fn pairing_code_is_stable_for_reversed_device_order() {
        let first = Uuid::from_u128(1);
        let second = Uuid::from_u128(2);
        assert_eq!(
            verification_code(b"handshake", first, second),
            verification_code(b"handshake", second, first)
        );
        assert_eq!(verification_code(b"handshake", first, second).len(), 6);
    }

    #[test]
    fn discovery_identity_rejects_oversized_names_and_bad_fingerprints() {
        assert!(validate_discovery_identity("设备", &"a".repeat(64)).is_ok());
        assert!(validate_discovery_identity(&"名".repeat(129), &"a".repeat(64)).is_err());
        assert!(validate_discovery_identity("设备", "not-a-fingerprint").is_err());
    }

    #[test]
    fn occupied_preferred_tcp_port_falls_back_to_dynamic_port() {
        let occupied =
            std::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, PREFERRED_TCP_PORT)).ok();
        let listener =
            tauri::async_runtime::block_on(bind_tcp_listener()).expect("动态端口应可监听");
        let actual = listener.local_addr().expect("应读取监听地址").port();

        if occupied.is_some() {
            assert_ne!(actual, PREFERRED_TCP_PORT);
        }
        assert_ne!(actual, 0);
    }

    #[test]
    fn occupied_udp_port_reports_discovery_only_failure() {
        let Ok(_occupied) = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT))
        else {
            return;
        };
        let (socket, error) = tauri::async_runtime::block_on(bind_discovery_socket());

        assert!(socket.is_none());
        assert!(
            error
                .as_deref()
                .is_some_and(|value| value.contains("UDP 37991"))
        );
    }

    #[test]
    fn noise_transport_reassembles_large_nearweave_frame() {
        let (initiator, responder) = noise_transport_pair();
        let initiator = Arc::new(AsyncMutex::new(initiator));
        let responder = Arc::new(AsyncMutex::new(responder));
        let frame = Frame::with_payload(
            Message::ClipboardText {
                message_id: Uuid::nil(),
                sha256: "large".into(),
            },
            vec![5_u8; 200 * 1024],
        );

        tauri::async_runtime::block_on(async {
            let (mut writer, mut reader) = tokio::io::duplex(512 * 1024);
            write_noise_frame(&mut writer, &initiator, &frame)
                .await
                .expect("应分片加密");
            let decoded = read_noise_frame(&mut reader, &responder)
                .await
                .expect("应重组解密");
            assert_eq!(decoded.payload, frame.payload);
            assert!(matches!(decoded.message, Message::ClipboardText { .. }));
        });
    }

    fn noise_transport_pair() -> (TransportState, TransportState) {
        let params: NoiseParams = NOISE_PATTERN.parse().expect("Noise 参数应有效");
        let generator = Builder::new(params.clone());
        let initiator_key = generator.generate_keypair().expect("应生成发起方密钥");
        let responder_key = generator.generate_keypair().expect("应生成响应方密钥");
        let mut initiator = Builder::new(params.clone())
            .local_private_key(&initiator_key.private)
            .expect("发起方私钥应有效")
            .build_initiator()
            .expect("应创建发起方握手");
        let mut responder = Builder::new(params)
            .local_private_key(&responder_key.private)
            .expect("响应方私钥应有效")
            .build_responder()
            .expect("应创建响应方握手");
        let mut message = vec![0_u8; MAX_HANDSHAKE_PACKET_SIZE];
        let mut payload = vec![0_u8; MAX_HANDSHAKE_PACKET_SIZE];

        let length = initiator
            .write_message(&[], &mut message)
            .expect("应写入握手消息 1");
        responder
            .read_message(&message[..length], &mut payload)
            .expect("应读取握手消息 1");
        let length = responder
            .write_message(&[], &mut message)
            .expect("应写入握手消息 2");
        initiator
            .read_message(&message[..length], &mut payload)
            .expect("应读取握手消息 2");
        let length = initiator
            .write_message(&[], &mut message)
            .expect("应写入握手消息 3");
        responder
            .read_message(&message[..length], &mut payload)
            .expect("应读取握手消息 3");

        (
            initiator
                .into_transport_mode()
                .expect("发起方应进入传输模式"),
            responder
                .into_transport_mode()
                .expect("响应方应进入传输模式"),
        )
    }
}
