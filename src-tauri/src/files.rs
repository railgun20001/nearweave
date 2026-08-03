use std::{
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};
use tauri::AppHandle;
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
};
use uuid::Uuid;

use crate::{
    error::{AppError, AppResult},
    models::{
        DirectoryPageView, NoticeLevel, RemoteEntry, SharedRoot, TransferCancelResult,
        TransferDirection, TransferState, TransferView,
    },
    protocol::{
        CAPABILITY_LAZY_DIRECTORY, CAPABILITY_TRANSFER_CANCEL, FILE_CHUNK_SIZE, Frame, Message,
        TransferSource,
    },
    state::{AppState, IncomingTransfer},
};

const DIRECTORY_PAGE_SIZE: usize = 200;
const TRANSFER_RETRY_LIMIT: usize = 2;
const TRANSFER_RECONNECT_WAIT: Duration = Duration::from_secs(45);

pub struct DirectoryPageMeta {
    pub request_id: Uuid,
    pub share_id: Uuid,
    pub relative_path: String,
    pub offset: u32,
    pub next_offset: Option<u32>,
}

pub async fn send_direct_files(
    app: AppHandle,
    state: AppState,
    device_id: Uuid,
    paths: Vec<PathBuf>,
) {
    let epoch = state.transfer_epoch();
    let send_lock = state.send_lock(device_id).await;
    let _send_guard = send_lock.lock().await;
    if !state.transfer_epoch_is_current(epoch) || !state.receiver_enabled() {
        return;
    }
    for path in paths {
        if !state.transfer_epoch_is_current(epoch) || !state.receiver_enabled() {
            return;
        }
        let result = send_file(&app, &state, device_id, path, None, TransferSource::Direct).await;
        if let Err(error) = result {
            state.emit_notice(&app, NoticeLevel::Error, error.to_string());
        }
    }
}

pub async fn send_shared_file(
    app: AppHandle,
    state: AppState,
    device_id: Uuid,
    request_id: Uuid,
    share_id: Uuid,
    relative_path: String,
) -> AppResult<()> {
    let epoch = state.transfer_epoch();
    let send_lock = state.send_lock(device_id).await;
    let _send_guard = send_lock.lock().await;
    if !state.transfer_epoch_is_current(epoch) || !state.receiver_enabled() {
        return Err(AppError::TransferCanceled);
    }
    let (path, display_name) = resolve_shared_file(&state, share_id, &relative_path)?;
    if let Err(error) = send_file(
        &app,
        &state,
        device_id,
        path,
        Some(display_name),
        TransferSource::SharedDirectory,
    )
    .await
    {
        let _ = state
            .send_frame_to(
                device_id,
                Frame::new(Message::Error {
                    request_id: Some(request_id),
                    message: error.to_string(),
                }),
            )
            .await;
        return Err(error);
    }
    Ok(())
}

async fn send_file(
    app: &AppHandle,
    state: &AppState,
    device_id: Uuid,
    path: PathBuf,
    display_name: Option<String>,
    source: TransferSource,
) -> AppResult<()> {
    let canonical = fs::canonicalize(&path).await?;
    let metadata = fs::metadata(&canonical).await?;
    if !metadata.is_file() {
        return Err(AppError::InvalidInput(format!(
            "{} 不是普通文件",
            canonical.display()
        )));
    }

    let name = display_name.unwrap_or_else(|| {
        canonical
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| "未命名文件".into())
    });
    let transfer_id = Uuid::new_v4();
    state.add_transfer(TransferView::new(
        transfer_id,
        (device_id, state.peer_name(device_id)),
        name.clone(),
        TransferDirection::Sending,
        TransferState::Queued,
        metadata.len(),
        "等待发送".into(),
    ));
    state.emit_snapshot(app);
    let cancellation = state.register_transfer_cancellation(device_id, transfer_id);

    let mut retry_count = 0;
    loop {
        if cancellation.load(Ordering::Acquire) {
            state.update_transfer(
                transfer_id,
                None,
                Some(TransferState::Canceled),
                Some("传输已取消".into()),
            );
            state.finish_transfer_cancellation(device_id, transfer_id);
            state.emit_snapshot(app);
            return Err(AppError::TransferCanceled);
        }
        let connection_generation = state.connection_generation_for_peer(device_id);
        let ack_receiver = state.begin_transfer_ack(device_id, transfer_id);
        let attempt_result = send_file_attempt(
            app,
            state,
            OutgoingFileAttempt {
                device_id,
                canonical: &canonical,
                transfer_id,
                name: &name,
                size: metadata.len(),
                source: source.clone(),
                cancellation: cancellation.clone(),
            },
        )
        .await;
        let attempt_result = match attempt_result {
            Ok(()) => {
                wait_for_transfer_ack(
                    state,
                    device_id,
                    transfer_id,
                    ack_receiver,
                    cancellation.clone(),
                )
                .await
            }
            Err(error) => {
                state.cancel_transfer_ack(device_id, transfer_id);
                Err(error)
            }
        };
        match attempt_result {
            Ok(()) => {
                state.finish_transfer_cancellation(device_id, transfer_id);
                state.emit_snapshot(app);
                return Ok(());
            }
            Err(error)
                if matches!(&error, AppError::NotConnected | AppError::Bluetooth(_))
                    && retry_count < TRANSFER_RETRY_LIMIT
                    && connection_generation.is_some()
                    && wait_for_reconnected_transport(
                        app,
                        state,
                        device_id,
                        transfer_id,
                        connection_generation,
                    )
                    .await =>
            {
                retry_count += 1;
                state.update_transfer(
                    transfer_id,
                    Some(0),
                    Some(TransferState::Queued),
                    Some(format!(
                        "链路已切换，正在从头重试（{retry_count}/{TRANSFER_RETRY_LIMIT}）"
                    )),
                );
                state.emit_snapshot(app);
            }
            Err(error) => {
                let canceled = matches!(error, AppError::TransferCanceled)
                    || cancellation.load(Ordering::Acquire);
                state.update_transfer(
                    transfer_id,
                    None,
                    Some(if canceled {
                        TransferState::Canceled
                    } else {
                        TransferState::Failed
                    }),
                    Some(if canceled {
                        "传输已取消".into()
                    } else {
                        error.to_string()
                    }),
                );
                state.finish_transfer_cancellation(device_id, transfer_id);
                state.emit_snapshot(app);
                return Err(error);
            }
        }
    }
}

async fn wait_for_transfer_ack(
    state: &AppState,
    device_id: Uuid,
    transfer_id: Uuid,
    mut receiver: tokio::sync::oneshot::Receiver<(bool, String)>,
    cancellation: Arc<AtomicBool>,
) -> AppResult<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        tokio::select! {
            result = &mut receiver => {
                let (accepted, detail) = result.map_err(|_| AppError::NotConnected)?;
                return if accepted {
                    Ok(())
                } else {
                    Err(AppError::Protocol(detail))
                };
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if cancellation.load(Ordering::Acquire) {
                    state.cancel_transfer_ack(device_id, transfer_id);
                    return Err(AppError::TransferCanceled);
                }
                if Instant::now() >= deadline {
                    state.cancel_transfer_ack(device_id, transfer_id);
                    return Err(AppError::Protocol("等待对方确认传输完成超时".into()));
                }
            }
        }
    }
}

struct OutgoingFileAttempt<'a> {
    device_id: Uuid,
    canonical: &'a Path,
    transfer_id: Uuid,
    name: &'a str,
    size: u64,
    source: TransferSource,
    cancellation: Arc<AtomicBool>,
}

async fn send_file_attempt(
    app: &AppHandle,
    state: &AppState,
    attempt: OutgoingFileAttempt<'_>,
) -> AppResult<()> {
    let OutgoingFileAttempt {
        device_id,
        canonical,
        transfer_id,
        name,
        size,
        source,
        cancellation,
    } = attempt;
    state
        .send_frame_to(
            device_id,
            Frame::new(Message::FileOffer {
                transfer_id,
                name: name.into(),
                size,
                source,
            }),
        )
        .await?;
    state.update_transfer(
        transfer_id,
        Some(0),
        Some(TransferState::Transferring),
        Some("正在发送".into()),
    );

    let mut file = File::open(canonical).await?;
    let mut hasher = Sha256::new();
    let mut offset = 0_u64;
    let mut buffer = vec![0_u8; FILE_CHUNK_SIZE];
    let mut last_emit = Instant::now();

    loop {
        if cancellation.load(Ordering::Acquire) {
            return Err(AppError::TransferCanceled);
        }
        let count = file.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let payload = buffer[..count].to_vec();
        hasher.update(&payload);
        state
            .send_frame_to(
                device_id,
                Frame::with_payload(
                    Message::FileChunk {
                        transfer_id,
                        offset,
                    },
                    payload,
                ),
            )
            .await?;
        offset += count as u64;
        state.update_transfer(transfer_id, Some(offset), None, None);
        if last_emit.elapsed() >= Duration::from_millis(250) {
            state.emit_snapshot(app);
            last_emit = Instant::now();
        }
    }

    state
        .send_frame_to(
            device_id,
            Frame::new(Message::FileComplete {
                transfer_id,
                sha256: format!("{:x}", hasher.finalize()),
            }),
        )
        .await?;
    state.update_transfer(
        transfer_id,
        Some(offset),
        Some(TransferState::WaitingForPeer),
        Some("等待对方校验".into()),
    );
    Ok(())
}

async fn wait_for_reconnected_transport(
    app: &AppHandle,
    state: &AppState,
    device_id: Uuid,
    transfer_id: Uuid,
    previous_generation: Option<Uuid>,
) -> bool {
    if state.reconnect_target_for_peer(device_id).is_none() {
        return false;
    }
    state.update_transfer(
        transfer_id,
        None,
        Some(TransferState::Queued),
        Some("连接中断，等待切换链路后自动重试".into()),
    );
    state.emit_snapshot(app);

    let deadline = Instant::now() + TRANSFER_RECONNECT_WAIT;
    while Instant::now() < deadline {
        if let Some(generation) = state.connection_generation_for_peer(device_id)
            && Some(generation) != previous_generation
        {
            return true;
        }
        if state.reconnect_target_for_peer(device_id).is_none() {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    false
}

pub async fn accept_file_offer(
    app: &AppHandle,
    state: &AppState,
    device_id: Uuid,
    transfer_id: Uuid,
    name: String,
    size: u64,
) -> AppResult<()> {
    if state.is_canceled_incoming(device_id, transfer_id) {
        return Ok(());
    }
    if state
        .inner
        .incoming
        .lock()
        .await
        .contains_key(&(device_id, transfer_id))
    {
        return Err(AppError::Protocol("收到重复的文件发送请求".into()));
    }
    fs::create_dir_all(&state.inner.receive_directory).await?;
    let safe_name = sanitize_file_name(&name);
    let final_path = reserve_unique_destination(state, &safe_name).await;
    let temporary_path = state
        .inner
        .receive_directory
        .join(format!(".nearweave-{}.part", Uuid::new_v4()));
    let file = match OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary_path)
        .await
    {
        Ok(file) => file,
        Err(error) => {
            release_reserved_destination(state, &final_path).await;
            return Err(AppError::Io(std::io::Error::new(
                error.kind(),
                format!("无法创建临时接收文件 {}：{error}", temporary_path.display()),
            )));
        }
    };

    state.inner.incoming.lock().await.insert(
        (device_id, transfer_id),
        IncomingTransfer {
            final_path,
            temporary_path,
            file,
            hasher: Sha256::new(),
            received: 0,
            expected: size,
        },
    );
    state.add_transfer(TransferView::new(
        transfer_id,
        (device_id, state.peer_name(device_id)),
        name,
        TransferDirection::Receiving,
        TransferState::Transferring,
        size,
        "正在接收".into(),
    ));
    state.emit_snapshot(app);
    Ok(())
}

pub async fn accept_file_chunk(
    app: &AppHandle,
    state: &AppState,
    device_id: Uuid,
    transfer_id: Uuid,
    offset: u64,
    payload: Vec<u8>,
) -> AppResult<()> {
    if state.is_canceled_incoming(device_id, transfer_id) {
        return Ok(());
    }
    let mut incoming = state.inner.incoming.lock().await;
    let transfer = incoming
        .get_mut(&(device_id, transfer_id))
        .ok_or_else(|| AppError::Protocol("收到未知文件的分块".into()))?;
    if transfer.received != offset {
        return Err(AppError::Protocol(format!(
            "文件分块偏移错误，期望 {}，收到 {offset}",
            transfer.received
        )));
    }
    if transfer.received + payload.len() as u64 > transfer.expected {
        return Err(AppError::Protocol("接收数据超过文件声明大小".into()));
    }

    transfer.file.write_all(&payload).await?;
    transfer.hasher.update(&payload);
    transfer.received += payload.len() as u64;
    let received = transfer.received;
    drop(incoming);

    state.update_transfer(transfer_id, Some(received), None, None);
    state.emit_snapshot(app);
    Ok(())
}

pub async fn complete_incoming_file(
    app: &AppHandle,
    state: &AppState,
    device_id: Uuid,
    transfer_id: Uuid,
    expected_sha256: String,
) -> AppResult<()> {
    if state.is_canceled_incoming(device_id, transfer_id) {
        return Ok(());
    }
    let transfer = state
        .inner
        .incoming
        .lock()
        .await
        .remove(&(device_id, transfer_id))
        .ok_or_else(|| AppError::Protocol("收到未知文件的完成消息".into()))?;
    let IncomingTransfer {
        final_path,
        temporary_path,
        mut file,
        hasher,
        received,
        expected,
    } = transfer;

    if let Err(error) = file.flush().await {
        drop(file);
        let _ = fs::remove_file(&temporary_path).await;
        release_reserved_destination(state, &final_path).await;
        state.update_transfer(
            transfer_id,
            Some(received),
            Some(TransferState::Failed),
            Some(format!("写入接收文件失败：{error}")),
        );
        state.emit_snapshot(app);
        return Err(AppError::Io(error));
    }
    drop(file);
    let actual_hash = format!("{:x}", hasher.finalize());
    if received != expected || actual_hash != expected_sha256 {
        let _ = fs::remove_file(&temporary_path).await;
        release_reserved_destination(state, &final_path).await;
        state.update_transfer(
            transfer_id,
            Some(received),
            Some(TransferState::Failed),
            Some("文件校验失败，临时文件已删除".into()),
        );
        state.emit_snapshot(app);
        let detail = "文件大小或 SHA-256 校验不一致".to_string();
        let _ = state
            .send_frame_to(
                device_id,
                Frame::new(Message::TransferAck {
                    transfer_id,
                    accepted: false,
                    detail: detail.clone(),
                }),
            )
            .await;
        return Err(AppError::Protocol(detail));
    }

    if let Err(error) = fs::rename(&temporary_path, &final_path).await {
        let _ = fs::remove_file(&temporary_path).await;
        release_reserved_destination(state, &final_path).await;
        state.update_transfer(
            transfer_id,
            Some(received),
            Some(TransferState::Failed),
            Some(format!("保存文件失败：{error}")),
        );
        state.emit_snapshot(app);
        let _ = state
            .send_frame_to(
                device_id,
                Frame::new(Message::TransferAck {
                    transfer_id,
                    accepted: false,
                    detail: format!("保存文件失败：{error}"),
                }),
            )
            .await;
        return Err(AppError::Io(error));
    }
    release_reserved_destination(state, &final_path).await;
    state.update_transfer(
        transfer_id,
        Some(received),
        Some(TransferState::Completed),
        Some(format!("已保存到 {}", final_path.display())),
    );
    state.emit_snapshot(app);
    state
        .send_frame_to(
            device_id,
            Frame::new(Message::TransferAck {
                transfer_id,
                accepted: true,
                detail: "接收并校验完成".into(),
            }),
        )
        .await?;
    state.emit_notice(
        app,
        NoticeLevel::Success,
        format!("已接收 {}", final_path.display()),
    );
    Ok(())
}

pub async fn cancel_transfer(
    app: &AppHandle,
    state: &AppState,
    device_id: Uuid,
    transfer_id: Uuid,
    reason: String,
) -> AppResult<TransferCancelResult> {
    let result = cancel_local_transfer(app, state, device_id, transfer_id, &reason).await?;
    if result == TransferCancelResult::Canceled {
        if state.supports_capability(device_id, CAPABILITY_TRANSFER_CANCEL) {
            state.begin_cancel_ack_wait(device_id, transfer_id);
            if let Err(error) = state
                .send_frame_to(
                    device_id,
                    Frame::new(Message::TransferCancel {
                        transfer_id,
                        reason,
                    }),
                )
                .await
            {
                state.finish_cancel_ack_wait(device_id, transfer_id);
                return Err(error);
            }
            let timeout_state = state.clone();
            let timeout_app = app.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(5)).await;
                if timeout_state.finish_cancel_ack_wait(device_id, transfer_id) {
                    timeout_state.update_transfer(
                        transfer_id,
                        None,
                        Some(TransferState::Canceled),
                        Some("本机已停止，对方未确认".into()),
                    );
                    timeout_state.emit_snapshot(&timeout_app);
                }
            });
        } else {
            state.try_send_frame_to(
                device_id,
                Frame::new(Message::Disconnect {
                    reason: "对方版本不支持单任务取消，已断开连接".into(),
                }),
            )?;
            state.disconnect_peer(device_id);
        }
    }
    Ok(result)
}

pub async fn accept_transfer_cancel(
    app: &AppHandle,
    state: &AppState,
    device_id: Uuid,
    transfer_id: Uuid,
    reason: String,
) -> AppResult<()> {
    let result = cancel_local_transfer(app, state, device_id, transfer_id, &reason).await?;
    state
        .send_frame_to(
            device_id,
            Frame::new(Message::TransferCancelAck {
                transfer_id,
                result,
                detail: match result {
                    TransferCancelResult::Canceled => "任务已取消".into(),
                    TransferCancelResult::AlreadyCompleted => "任务已经完成，无法撤回".into(),
                    TransferCancelResult::NotFound => "未找到该传输任务".into(),
                },
            }),
        )
        .await
}

async fn cancel_local_transfer(
    app: &AppHandle,
    state: &AppState,
    device_id: Uuid,
    transfer_id: Uuid,
    reason: &str,
) -> AppResult<TransferCancelResult> {
    if let Some(transfer) = state.transfer(device_id, transfer_id)
        && transfer.is_terminal()
    {
        return Ok(if transfer.state == TransferState::Completed {
            TransferCancelResult::AlreadyCompleted
        } else {
            TransferCancelResult::Canceled
        });
    }

    let mut found = state.request_transfer_cancellation(device_id, transfer_id);
    let incoming = state
        .inner
        .incoming
        .lock()
        .await
        .remove(&(device_id, transfer_id));
    if let Some(incoming) = incoming {
        found = true;
        let temporary_path = incoming.temporary_path.clone();
        let final_path = incoming.final_path.clone();
        drop(incoming.file);
        let _ = fs::remove_file(&temporary_path).await;
        release_reserved_destination(state, &final_path).await;
        state.mark_canceled_incoming(device_id, transfer_id);
    }
    if !found && state.transfer(device_id, transfer_id).is_none() {
        return Ok(TransferCancelResult::NotFound);
    }

    state.update_transfer(
        transfer_id,
        None,
        Some(TransferState::Canceled),
        Some(if reason.is_empty() {
            "任务已取消".into()
        } else {
            format!("任务已取消：{reason}")
        }),
    );
    state.finish_transfer_cancellation(device_id, transfer_id);
    state.emit_snapshot(app);
    Ok(TransferCancelResult::Canceled)
}

pub async fn send_share_roots(
    state: &AppState,
    device_id: Uuid,
    request_id: Option<Uuid>,
) -> AppResult<()> {
    let roots = state
        .inner
        .local_shares
        .lock()
        .expect("共享目录锁损坏")
        .iter()
        .map(|share| SharedRoot {
            id: share.id,
            name: share.name.clone(),
        })
        .collect::<Vec<_>>();
    state
        .send_frame_to(
            device_id,
            Frame::with_payload(
                Message::ShareRoots {
                    request_id,
                    revision: Uuid::new_v4(),
                },
                serde_json::to_vec(&roots)?,
            ),
        )
        .await
}

pub fn accept_share_roots(
    state: &AppState,
    device_id: Uuid,
    revision: Uuid,
    payload: &[u8],
) -> AppResult<()> {
    let roots: Vec<SharedRoot> = serde_json::from_slice(payload)?;
    if roots.len() > 64 {
        return Err(AppError::Protocol("远端共享根目录数量超过限制".into()));
    }
    state.set_remote_roots(device_id, revision, roots);
    Ok(())
}

pub async fn send_directory_page(
    state: &AppState,
    device_id: Uuid,
    request_id: Uuid,
    share_id: Uuid,
    relative_path: String,
    offset: u32,
) -> AppResult<()> {
    let entries = list_direct_children(state, share_id, &relative_path).await?;
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(entries.len());
    let end = start.saturating_add(DIRECTORY_PAGE_SIZE).min(entries.len());
    let next_offset = (end < entries.len()).then(|| u32::try_from(end).unwrap_or(u32::MAX));
    state
        .send_frame_to(
            device_id,
            Frame::with_payload(
                Message::DirectoryListResponse {
                    request_id,
                    share_id,
                    relative_path,
                    offset,
                    next_offset,
                },
                serde_json::to_vec(&entries[start..end])?,
            ),
        )
        .await
}

pub async fn request_directory_page(
    state: &AppState,
    device_id: Uuid,
    share_id: Uuid,
    relative_path: String,
    offset: u32,
) -> AppResult<DirectoryPageView> {
    if !state.supports_capability(device_id, CAPABILITY_LAZY_DIRECTORY) {
        return Err(AppError::Protocol(
            "对方版本不支持按需目录浏览，请升级 NearWeave".into(),
        ));
    }
    validate_relative_path(&relative_path)?;
    if let Some(page) = state.cached_remote_directory(device_id, share_id, &relative_path, offset) {
        return Ok(page);
    }
    let request_id = Uuid::new_v4();
    let receiver = state.begin_directory_request(device_id, request_id).await;
    if let Err(error) = state
        .send_frame_to(
            device_id,
            Frame::new(Message::DirectoryListRequest {
                request_id,
                share_id,
                relative_path,
                offset,
            }),
        )
        .await
    {
        state.cancel_directory_request(device_id, request_id).await;
        return Err(error);
    }
    match tokio::time::timeout(Duration::from_secs(10), receiver).await {
        Ok(result) => result.map_err(|_| AppError::NotConnected)?,
        Err(_) => {
            state.cancel_directory_request(device_id, request_id).await;
            Err(AppError::Protocol("读取远端目录超时".into()))
        }
    }
}

pub async fn accept_directory_page(
    state: &AppState,
    device_id: Uuid,
    meta: DirectoryPageMeta,
    payload: &[u8],
) -> AppResult<()> {
    let DirectoryPageMeta {
        request_id,
        share_id,
        relative_path,
        offset,
        next_offset,
    } = meta;
    let entries: Vec<RemoteEntry> = serde_json::from_slice(payload)?;
    if entries.len() > DIRECTORY_PAGE_SIZE {
        return Err(AppError::Protocol("远端目录分页条目超过限制".into()));
    }
    let page = DirectoryPageView {
        device_id,
        share_id,
        relative_path,
        offset,
        next_offset,
        entries,
    };
    state.cache_remote_directory(page.clone());
    state
        .resolve_directory_request(device_id, request_id, Ok(page))
        .await;
    Ok(())
}

async fn list_direct_children(
    state: &AppState,
    share_id: Uuid,
    relative_path: &str,
) -> AppResult<Vec<RemoteEntry>> {
    validate_relative_path(relative_path)?;
    let share = state
        .inner
        .local_shares
        .lock()
        .expect("共享目录锁损坏")
        .iter()
        .find(|share| share.id == share_id)
        .cloned()
        .ok_or_else(|| AppError::InvalidInput("请求的共享目录不存在".into()))?;
    let root = fs::canonicalize(&share.path).await?;
    let directory = if relative_path.is_empty() {
        root.clone()
    } else {
        let requested = root.join(relative_path);
        if fs::symlink_metadata(&requested)
            .await?
            .file_type()
            .is_symlink()
        {
            return Err(AppError::InvalidInput("不允许浏览符号链接目录".into()));
        }
        fs::canonicalize(requested).await?
    };
    if !directory.starts_with(&root) || !fs::metadata(&directory).await?.is_dir() {
        return Err(AppError::InvalidInput("请求的共享子目录不安全".into()));
    }

    let mut reader = fs::read_dir(&directory).await?;
    let mut entries = Vec::new();
    while let Some(entry) = reader.next_entry().await? {
        let file_type = entry.file_type().await?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let relative = path
            .strip_prefix(&root)
            .map_err(|_| AppError::InvalidInput("共享目录索引越界".into()))?;
        let metadata = entry.metadata().await?;
        entries.push(RemoteEntry {
            share_id,
            share_name: share.name.clone(),
            relative_path: path_to_protocol(relative),
            name: entry.file_name().to_string_lossy().into_owned(),
            size: if metadata.is_file() {
                metadata.len()
            } else {
                0
            },
            is_directory: metadata.is_dir(),
        });
    }
    entries.sort_by(|left, right| {
        right
            .is_directory
            .cmp(&left.is_directory)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    Ok(entries)
}

fn validate_relative_path(relative_path: &str) -> AppResult<()> {
    let path = Path::new(relative_path);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(AppError::InvalidInput("共享目录相对路径不安全".into()));
    }
    Ok(())
}

fn resolve_shared_file(
    state: &AppState,
    share_id: Uuid,
    relative_path: &str,
) -> AppResult<(PathBuf, String)> {
    let shares = state.inner.local_shares.lock().expect("共享目录锁损坏");
    let share = shares
        .iter()
        .find(|value| value.id == share_id)
        .ok_or_else(|| AppError::InvalidInput("请求的共享目录不存在".into()))?;
    let relative = Path::new(relative_path);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(AppError::InvalidInput("共享文件路径不安全".into()));
    }

    let root = share.path.canonicalize()?;
    let requested = root.join(relative);
    if requested.symlink_metadata()?.file_type().is_symlink() {
        return Err(AppError::InvalidInput("不允许下载符号链接文件".into()));
    }
    let candidate = requested.canonicalize()?;
    if !candidate.starts_with(&root) || !candidate.is_file() {
        return Err(AppError::InvalidInput(
            "共享文件不存在或超出授权目录".into(),
        ));
    }
    Ok((candidate, format!("{}/{}", share.name, relative_path)))
}

fn path_to_protocol(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn sanitize_file_name(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "未命名文件".into());
    let mut cleaned: String = base
        .chars()
        .map(|character| {
            if character.is_control() || r#"<>:"/\|?*"#.contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect();
    cleaned = cleaned.trim().trim_end_matches(['.', ' ']).to_string();
    if cleaned.is_empty() {
        cleaned = "未命名文件".into();
    }

    let stem = cleaned
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );
    if reserved {
        cleaned.insert(0, '_');
    }
    cleaned
}

async fn reserve_unique_destination(state: &AppState, name: &str) -> PathBuf {
    let directory = &state.inner.receive_directory;
    let mut reserved = state.inner.reserved_destinations.lock().await;
    let requested = directory.join(name);
    if !reserved.contains(&requested) && !fs::try_exists(&requested).await.unwrap_or(true) {
        reserved.insert(requested.clone());
        return requested;
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map(|value| value.to_string_lossy())
        .unwrap_or_else(|| "文件".into());
    let extension = path.extension().map(|value| value.to_string_lossy());

    for index in 1..10_000 {
        let candidate_name = match &extension {
            Some(extension) => format!("{stem} ({index}).{extension}"),
            None => format!("{stem} ({index})"),
        };
        let candidate = directory.join(candidate_name);
        if !reserved.contains(&candidate) && !fs::try_exists(&candidate).await.unwrap_or(true) {
            reserved.insert(candidate.clone());
            return candidate;
        }
    }
    let candidate = directory.join(format!("{}-{name}", Uuid::new_v4()));
    reserved.insert(candidate.clone());
    candidate
}

async fn release_reserved_destination(state: &AppState, path: &Path) {
    state.inner.reserved_destinations.lock().await.remove(path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{identity::DeviceIdentity, models::LocalShare, state::AppState};

    #[test]
    fn sanitizes_remote_file_names() {
        assert_eq!(sanitize_file_name("../a:b?.txt"), "a_b_.txt");
        assert_eq!(sanitize_file_name("CON"), "_CON");
        assert_eq!(sanitize_file_name(".."), "未命名文件");
    }

    #[test]
    fn shared_file_request_cannot_escape_authorized_root() {
        let temporary = tempfile::tempdir().expect("应创建测试目录");
        let root = temporary.path().join("shared");
        std::fs::create_dir(&root).expect("应创建共享目录");
        std::fs::write(root.join("allowed.txt"), b"allowed").expect("应创建共享文件");
        std::fs::write(temporary.path().join("outside.txt"), b"outside").expect("应创建目录外文件");

        let state = AppState::new(crate::state::AppStateConfig {
            device_id: Uuid::nil(),
            device_name: "test".into(),
            receive_directory: temporary.path().join("received"),
            legacy_receive_directory: None,
            settings_path: temporary.path().join("settings.json"),
            trust_path: temporary.path().join("trusted-devices.json"),
            identity: DeviceIdentity {
                private_key: vec![1; 32],
                public_key: vec![2; 32],
            },
            trusted_devices: Vec::new(),
            clipboard_enabled: false,
            autostart_enabled: false,
            lan_enabled: false,
            lan_setup_decided: true,
        });
        let share_id = Uuid::new_v4();
        state
            .inner
            .local_shares
            .lock()
            .expect("共享目录锁损坏")
            .push(LocalShare {
                id: share_id,
                name: "shared".into(),
                path: root,
            });

        assert!(resolve_shared_file(&state, share_id, "allowed.txt").is_ok());
        assert!(resolve_shared_file(&state, share_id, "../outside.txt").is_err());
    }

    #[test]
    fn lazy_directory_listing_returns_only_direct_children() {
        let temporary = tempfile::tempdir().expect("应创建测试目录");
        let root = temporary.path().join("shared");
        std::fs::create_dir_all(root.join("child")).expect("应创建子目录");
        std::fs::write(root.join("root.txt"), b"root").expect("应创建根文件");
        std::fs::write(root.join("child").join("nested.txt"), b"nested").expect("应创建孙级文件");
        let state = AppState::new(crate::state::AppStateConfig {
            device_id: Uuid::nil(),
            device_name: "test".into(),
            receive_directory: temporary.path().join("received"),
            legacy_receive_directory: None,
            settings_path: temporary.path().join("settings.json"),
            trust_path: temporary.path().join("trusted-devices.json"),
            identity: DeviceIdentity {
                private_key: vec![1; 32],
                public_key: vec![2; 32],
            },
            trusted_devices: Vec::new(),
            clipboard_enabled: false,
            autostart_enabled: false,
            lan_enabled: false,
            lan_setup_decided: true,
        });
        let share_id = Uuid::new_v4();
        state
            .inner
            .local_shares
            .lock()
            .expect("共享目录锁损坏")
            .push(LocalShare {
                id: share_id,
                name: "shared".into(),
                path: root,
            });

        let entries = tauri::async_runtime::block_on(list_direct_children(&state, share_id, ""))
            .expect("应列出根目录");
        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_directory);
        assert_eq!(entries[0].relative_path, "child");
        assert_eq!(entries[1].relative_path, "root.txt");
        assert!(!entries.iter().any(|entry| entry.name == "nested.txt"));
    }

    #[test]
    fn concurrent_same_name_receives_reserve_different_paths() {
        let temporary = tempfile::tempdir().expect("应创建测试目录");
        let state = AppState::new(crate::state::AppStateConfig {
            device_id: Uuid::nil(),
            device_name: "test".into(),
            receive_directory: temporary.path().join("received"),
            legacy_receive_directory: None,
            settings_path: temporary.path().join("settings.json"),
            trust_path: temporary.path().join("trusted-devices.json"),
            identity: DeviceIdentity {
                private_key: vec![1; 32],
                public_key: vec![2; 32],
            },
            trusted_devices: Vec::new(),
            clipboard_enabled: false,
            autostart_enabled: false,
            lan_enabled: false,
            lan_setup_decided: true,
        });

        tauri::async_runtime::block_on(async {
            let first = reserve_unique_destination(&state, "同名.txt").await;
            let second = reserve_unique_destination(&state, "同名.txt").await;
            assert_ne!(first, second);
            release_reserved_destination(&state, &first).await;
            release_reserved_destination(&state, &second).await;
        });
    }
}
