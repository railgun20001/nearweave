use std::time::Duration;

use tauri::AppHandle;
use uuid::Uuid;
use windows::{
    Devices::{
        Bluetooth::{
            BluetoothCacheMode, BluetoothDevice, BluetoothError,
            Rfcomm::{RfcommDeviceService, RfcommServiceId, RfcommServiceProvider},
        },
        Enumeration::DeviceInformation,
    },
    Foundation::TypedEventHandler,
    Networking::Sockets::{
        SocketProtectionLevel, StreamSocket, StreamSocketListener,
        StreamSocketListenerConnectionReceivedEventArgs,
    },
    Storage::Streams::{ByteOrder, DataReader, DataWriter, InputStreamOptions},
    core::GUID,
};

use crate::{
    error::{AppError, AppResult},
    handlers::handle_frame,
    models::{ConnectionKind, NearbyDevice, NoticeLevel},
    protocol::{
        CAPABILITY_LAZY_DIRECTORY, CAPABILITY_TRANSFER_CANCEL, FRAME_PREFIX_SIZE, Frame, Message,
        PROTOCOL_VERSION, decode_prefix,
    },
    state::{AppState, ReconnectTarget},
    transport::{TransportCommand, TransportReceiver, TransportSender, transport_channel},
};

// 该 UUID 是 NearWeave 应用协议的服务标识，后续平台实现必须保持一致。
const SERVICE_UUID: GUID = GUID::from_u128(0x6a144fb2_f5c7_507a_8293_a3976e0e1a34);
const SDP_DEVICE_ID_ATTRIBUTE: u32 = 0x0300;
const SDP_PROTOCOL_VERSION_ATTRIBUTE: u32 = 0x0301;
const SDP_UUID128_ELEMENT: u8 = 0x1c;
const SDP_UINT16_ELEMENT: u8 = 0x09;
const OUTBOUND_QUEUE_CAPACITY: usize = 24;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(90);

pub struct ListenerHandle {
    provider: RfcommServiceProvider,
    listener: StreamSocketListener,
    event_token: i64,
}

pub async fn scan_bluetooth_devices() -> AppResult<Vec<NearbyDevice>> {
    let selector = BluetoothDevice::GetDeviceSelectorFromPairingState(true)?;
    let collection = DeviceInformation::FindAllAsyncAqsFilter(&selector)?.await?;
    let paired_devices = collection
        .into_iter()
        .map(|device| -> windows::core::Result<_> {
            Ok((device.Id()?.to_string(), device.Name()?.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut devices = Vec::new();
    for (device_id, fallback_name) in paired_devices {
        let (name, nearweave_enabled, app_device_id) = match resolve_live_service(&device_id).await
        {
            Ok(service) => {
                let name = service
                    .Device()
                    .and_then(|device| device.Name())
                    .map(|name| name.to_string())
                    .unwrap_or(fallback_name);
                let app_device_id = read_nearweave_device_id(&service).await.ok().flatten();
                let _ = service.Close();
                (name, true, app_device_id)
            }
            Err(_) => (fallback_name, false, None),
        };
        devices.push(NearbyDevice {
            id: app_device_id
                .map(|value| value.to_string())
                .unwrap_or_else(|| format!("bluetooth:{device_id}")),
            device_id: app_device_id,
            name,
            bluetooth_endpoint: Some(device_id),
            lan_endpoint: None,
            bluetooth_paired: true,
            nearweave_enabled,
            trusted: false,
        });
    }
    devices.sort_by(|left, right| left.name.cmp(&right.name));
    devices.dedup_by(|left, right| left.id == right.id);
    Ok(devices)
}

pub async fn start_bluetooth_listener(app: AppHandle, state: AppState) -> AppResult<()> {
    if state
        .inner
        .listener
        .lock()
        .expect("监听实例锁损坏")
        .is_some()
    {
        return Ok(());
    }

    let service_id = RfcommServiceId::FromUuid(SERVICE_UUID)?;
    let provider = RfcommServiceProvider::CreateAsync(&service_id)?.await?;
    install_sdp_attributes(&provider, state.inner.device_id)?;
    let listener = StreamSocketListener::new()?;
    let service_name = provider.ServiceId()?.AsString()?;
    listener
        .BindServiceNameWithProtectionLevelAsync(
            &service_name,
            SocketProtectionLevel::BluetoothEncryptionWithAuthentication,
        )?
        .await?;

    // TypedEventHandler 不是 Send，因此在最后一个 await 之后再创建和注册。
    let callback_app = app.clone();
    let callback_state = state.clone();
    let event_handler = TypedEventHandler::<
        StreamSocketListener,
        StreamSocketListenerConnectionReceivedEventArgs,
    >::new(move |_sender, args| {
        let Some(args) = args.as_ref() else {
            return Ok(());
        };
        let socket = args.Socket()?;
        let app = callback_app.clone();
        let state = callback_state.clone();
        tauri::async_runtime::spawn(async move {
            if !state.receiver_enabled() {
                let _ = socket.Close();
                return;
            }
            if let Err(error) = attach_socket(
                app.clone(),
                state.clone(),
                socket,
                "附近设备".into(),
                None,
                None,
            )
            .await
            {
                state.emit_notice(&app, NoticeLevel::Error, error.to_string());
            }
        });
        Ok(())
    });
    let event_token = listener.ConnectionReceived(&event_handler)?;
    provider.StartAdvertisingWithRadioDiscoverability(&listener, true)?;

    *state.inner.listener.lock().expect("监听实例锁损坏") = Some(ListenerHandle {
        provider,
        listener,
        event_token,
    });
    state.emit_snapshot(&app);
    Ok(())
}

pub async fn stop_bluetooth_listener(state: &AppState) -> AppResult<()> {
    if let Some(handle) = state.inner.listener.lock().expect("监听实例锁损坏").take() {
        let _ = handle.provider.StopAdvertising();
        let _ = handle.listener.RemoveConnectionReceived(handle.event_token);
        let _ = handle.listener.Close();
    }
    Ok(())
}

pub(crate) async fn connect_bluetooth_target(
    app: AppHandle,
    state: AppState,
    target: ReconnectTarget,
    reconnect_session: Uuid,
) -> AppResult<()> {
    if !state.is_reconnect_session_active(reconnect_session) {
        return Err(AppError::Bluetooth("自动重连已取消".into()));
    }

    let endpoint = target
        .bluetooth_endpoint
        .as_deref()
        .ok_or_else(|| AppError::Bluetooth("设备没有可用的蓝牙服务地址".into()))?;
    let service = resolve_live_service(endpoint).await?;
    let socket = StreamSocket::new()?;
    socket
        .ConnectWithProtectionLevelAsync(
            &service.ConnectionHostName()?,
            &service.ConnectionServiceName()?,
            SocketProtectionLevel::BluetoothEncryptionWithAuthentication,
        )?
        .await?;
    let _ = service.Close();
    if !state.is_reconnect_session_active(reconnect_session) {
        let _ = socket.Close();
        return Err(AppError::Bluetooth("自动重连已取消".into()));
    }
    attach_socket(
        app,
        state,
        socket,
        target.display_name,
        Some(endpoint.to_string()),
        Some(reconnect_session),
    )
    .await
}

async fn resolve_live_service(device_id: &str) -> AppResult<RfcommDeviceService> {
    let device: BluetoothDevice = BluetoothDevice::FromIdAsync(&device_id.into())?.await?;
    let service_id = RfcommServiceId::FromUuid(SERVICE_UUID)?;
    let result = device
        .GetRfcommServicesForIdWithCacheModeAsync(&service_id, BluetoothCacheMode::Uncached)?
        .await?;

    let status = result.Error()?;
    if status != BluetoothError::Success {
        return Err(AppError::Bluetooth(format!(
            "实时查询 NearWeave 服务失败：{}",
            bluetooth_error_text(status)
        )));
    }
    let services = result.Services()?;
    if services.Size()? == 0 {
        return Err(AppError::Bluetooth("对方当前未开启 NearWeave 连接".into()));
    }
    services.GetAt(0).map_err(AppError::from)
}

fn install_sdp_attributes(provider: &RfcommServiceProvider, device_id: Uuid) -> AppResult<()> {
    let attributes = provider.SdpRawAttributes()?;

    let device_writer = DataWriter::new()?;
    device_writer.WriteByte(SDP_UUID128_ELEMENT)?;
    device_writer.WriteBytes(device_id.as_bytes())?;
    let device_buffer = device_writer.DetachBuffer()?;
    attributes.Insert(SDP_DEVICE_ID_ATTRIBUTE, &device_buffer)?;

    let version_writer = DataWriter::new()?;
    version_writer.SetByteOrder(ByteOrder::BigEndian)?;
    version_writer.WriteByte(SDP_UINT16_ELEMENT)?;
    version_writer.WriteUInt16(PROTOCOL_VERSION)?;
    let version_buffer = version_writer.DetachBuffer()?;
    attributes.Insert(SDP_PROTOCOL_VERSION_ATTRIBUTE, &version_buffer)?;
    Ok(())
}

async fn read_nearweave_device_id(service: &RfcommDeviceService) -> AppResult<Option<Uuid>> {
    let attributes = service
        .GetSdpRawAttributesWithCacheModeAsync(BluetoothCacheMode::Uncached)?
        .await?;
    if !attributes.HasKey(SDP_DEVICE_ID_ATTRIBUTE)? {
        return Ok(None);
    }
    let buffer = attributes.Lookup(SDP_DEVICE_ID_ATTRIBUTE)?;
    let reader = DataReader::FromBuffer(&buffer)?;
    if reader.UnconsumedBufferLength()? != 17 || reader.ReadByte()? != SDP_UUID128_ELEMENT {
        return Ok(None);
    }
    let mut bytes = [0_u8; 16];
    reader.ReadBytes(&mut bytes)?;
    Ok(Some(Uuid::from_bytes(bytes)))
}

fn bluetooth_error_text(error: BluetoothError) -> &'static str {
    match error {
        BluetoothError::RadioNotAvailable => "蓝牙适配器不可用",
        BluetoothError::ResourceInUse => "蓝牙资源正在被占用",
        BluetoothError::DeviceNotConnected => "对方设备当前不可达",
        BluetoothError::DisabledByPolicy => "蓝牙被系统策略禁用",
        BluetoothError::NotSupported => "当前蓝牙适配器不支持该操作",
        BluetoothError::DisabledByUser => "蓝牙已被用户关闭",
        BluetoothError::ConsentRequired => "Windows 尚未授权访问该蓝牙服务",
        BluetoothError::TransportNotSupported => "对方不支持 RFCOMM 传输",
        BluetoothError::Success => "成功",
        _ => "未知蓝牙错误",
    }
}

async fn attach_socket(
    app: AppHandle,
    state: AppState,
    socket: StreamSocket,
    peer_hint: String,
    bluetooth_endpoint: Option<String>,
    reconnect_session: Option<Uuid>,
) -> AppResult<()> {
    if reconnect_session.is_none()
        && let Some(device_id) = bluetooth_endpoint
            .as_deref()
            .and_then(|endpoint| state.find_device(endpoint))
            .and_then(|device| device.device_id)
    {
        state.cancel_reconnect_for_peer(device_id);
    }

    let (sender, receiver) = transport_channel(OUTBOUND_QUEUE_CAPACITY);
    let generation = state.install_connection(
        sender.clone(),
        ConnectionKind::Bluetooth,
        peer_hint,
        None,
        bluetooth_endpoint,
        reconnect_session,
    )?;
    state.emit_snapshot(&app);

    let network_offer = crate::transport::network_offer(&state);
    let mut capabilities = vec![
        "files".into(),
        "shared_directories".into(),
        "clipboard_text".into(),
        CAPABILITY_TRANSFER_CANCEL.into(),
        CAPABILITY_LAZY_DIRECTORY.into(),
    ];
    if network_offer.is_some() {
        capabilities.push("lan_encrypted_tcp".into());
    }
    sender
        .send(TransportCommand::Send(Frame::new(Message::Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: state.inner.device_id,
            device_name: state.inner.device_name.clone(),
            capabilities,
            network_offer,
        })))
        .await
        .map_err(|_| AppError::NotConnected)?;

    // IInputStream/IOutputStream 本身不是 Send，必须在最后一个 await 之后创建，
    // 再包装为可在线程间移动的 DataReader/DataWriter。
    let input = socket.InputStream()?;
    let output = socket.OutputStream()?;
    let reader = DataReader::CreateDataReader(&input)?;
    reader.SetInputStreamOptions(InputStreamOptions::Partial)?;
    let writer = DataWriter::CreateDataWriter(&output)?;

    spawn_writer(
        app.clone(),
        state.clone(),
        generation,
        socket.clone(),
        writer,
        receiver,
    );
    spawn_reader(
        app.clone(),
        state.clone(),
        generation,
        socket,
        reader,
        sender.clone(),
    );
    spawn_heartbeat(app, state, generation, sender);
    Ok(())
}

fn spawn_writer(
    app: AppHandle,
    state: AppState,
    generation: Uuid,
    socket: StreamSocket,
    writer: DataWriter,
    mut receiver: TransportReceiver,
) {
    tauri::async_runtime::spawn(async move {
        let mut failure = None;
        while let Some(command) = receiver.recv().await {
            match command {
                TransportCommand::Send(frame) => match frame.encode() {
                    Ok(bytes) => {
                        let result = async {
                            writer.WriteBytes(&bytes)?;
                            writer.StoreAsync()?.await?;
                            AppResult::<()>::Ok(())
                        }
                        .await;
                        if let Err(error) = result {
                            failure = Some(error.to_string());
                            break;
                        }
                    }
                    Err(error) => {
                        failure = Some(error.to_string());
                        break;
                    }
                },
                TransportCommand::Close { reason } => {
                    failure = reason;
                    break;
                }
            }
        }
        let _ = writer.Close();
        let _ = socket.Close();
        finish_connection(&app, &state, generation, failure).await;
    });
}

fn spawn_reader(
    app: AppHandle,
    state: AppState,
    generation: Uuid,
    socket: StreamSocket,
    reader: DataReader,
    sender: TransportSender,
) {
    tauri::async_runtime::spawn(async move {
        let failure = loop {
            match read_frame(&reader).await {
                Ok(frame) => {
                    if let Err(error) =
                        handle_frame(app.clone(), state.clone(), generation, frame).await
                    {
                        state.emit_notice(&app, NoticeLevel::Error, error.to_string());
                    }
                }
                Err(error) => {
                    break Some(error.to_string());
                }
            }
        };
        let _ = sender.send(TransportCommand::Close { reason: None }).await;
        let _ = reader.Close();
        let _ = socket.Close();
        finish_connection(&app, &state, generation, failure).await;
    });
}

fn spawn_heartbeat(app: AppHandle, state: AppState, generation: Uuid, sender: TransportSender) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(HEARTBEAT_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(elapsed) = state.pong_elapsed(generation) else {
                break;
            };
            if heartbeat_timed_out(elapsed) {
                let reason = format!(
                    "{} 秒未收到 Pong，连接已判定失联",
                    HEARTBEAT_TIMEOUT.as_secs()
                );
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

async fn read_frame(reader: &DataReader) -> AppResult<Frame> {
    let mut prefix = [0_u8; FRAME_PREFIX_SIZE];
    read_exact(reader, &mut prefix).await?;
    let (header_len, payload_len) = decode_prefix(&prefix)?;
    let remaining = header_len
        .checked_add(payload_len)
        .ok_or_else(|| AppError::Protocol("消息长度溢出".into()))?;
    let mut encoded = Vec::with_capacity(FRAME_PREFIX_SIZE + remaining);
    encoded.extend_from_slice(&prefix);
    encoded.resize(FRAME_PREFIX_SIZE + remaining, 0);
    read_exact(reader, &mut encoded[FRAME_PREFIX_SIZE..]).await?;
    Frame::decode(&encoded)
}

async fn read_exact(reader: &DataReader, output: &mut [u8]) -> AppResult<()> {
    let mut offset = 0;
    while offset < output.len() {
        let requested = u32::try_from(output.len() - offset)
            .map_err(|_| AppError::Protocol("单次读取长度超出限制".into()))?;
        let loaded = reader.LoadAsync(requested)?.await? as usize;
        if loaded == 0 {
            return Err(AppError::Bluetooth("对方已断开连接".into()));
        }
        if loaded > output.len() - offset {
            return Err(AppError::Protocol("底层读取返回了超量数据".into()));
        }
        reader.ReadBytes(&mut output[offset..offset + loaded])?;
        offset += loaded;
    }
    Ok(())
}

async fn finish_connection(
    app: &AppHandle,
    state: &AppState,
    generation: Uuid,
    failure: Option<String>,
) {
    crate::transport::finish_primary_connection(app, state, generation, failure).await;
}

fn heartbeat_timed_out(elapsed: Duration) -> bool {
    elapsed >= HEARTBEAT_TIMEOUT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_timeout_starts_at_ninety_seconds() {
        assert!(!heartbeat_timed_out(Duration::from_secs(89)));
        assert!(heartbeat_timed_out(Duration::from_secs(90)));
    }

    #[test]
    fn bluetooth_service_errors_have_actionable_messages() {
        assert_eq!(
            bluetooth_error_text(BluetoothError::DeviceNotConnected),
            "对方设备当前不可达"
        );
        assert_eq!(
            bluetooth_error_text(BluetoothError::ConsentRequired),
            "Windows 尚未授权访问该蓝牙服务"
        );
    }
}
