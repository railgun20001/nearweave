use tauri::AppHandle;
use uuid::Uuid;

use crate::{
    clipboard::{MAX_CLIPBOARD_TEXT_BYTES, hash_bytes},
    error::{AppError, AppResult},
    files,
    models::{NoticeLevel, TransferCancelResult, TransferState},
    protocol::{CAPABILITY_LAZY_DIRECTORY, Frame, Message, PROTOCOL_VERSION},
    state::{AppState, ClipboardCommand},
    transport,
};

pub async fn handle_frame(
    app: AppHandle,
    state: AppState,
    generation: Uuid,
    frame: Frame,
) -> AppResult<()> {
    state.record_activity(generation);
    match frame.message {
        Message::Hello {
            protocol_version,
            device_id,
            device_name,
            capabilities,
            network_offer,
            ..
        } => {
            if protocol_version != PROTOCOL_VERSION {
                return Err(AppError::Protocol(format!(
                    "对方协议版本 {protocol_version} 与本机不兼容"
                )));
            }
            if !state.update_peer_identity(generation, device_id, device_name.clone(), capabilities)
            {
                return Ok(());
            }
            if let Some(offer) = network_offer {
                transport::negotiate_network(&app, &state, generation, device_id, offer)?;
            }
            state.emit_snapshot(&app);
            state.emit_notice(&app, NoticeLevel::Success, format!("已连接 {device_name}"));
            state.send_clipboard_command(ClipboardCommand::SyncPeer(device_id));
            if state.supports_capability(device_id, CAPABILITY_LAZY_DIRECTORY) {
                let roots_state = state.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = files::send_share_roots(&roots_state, device_id, None).await;
                });
            }
        }
        Message::NetworkHello { .. } => {
            return Err(AppError::Protocol(
                "局域网握手消息不能出现在业务消息流中".into(),
            ));
        }
        Message::Ping { nonce } => {
            state
                .send_frame_for_generation(generation, Frame::new(Message::Pong { nonce }))
                .await?;
        }
        Message::Pong { .. } => state.record_pong(generation),
        Message::Disconnect { reason } => {
            if let Some(device_id) = state.peer_for_generation(generation) {
                state.cancel_reconnect_for_peer(device_id);
            }
            state.close_connection(
                generation,
                Some(if reason.is_empty() {
                    "对方主动断开连接".into()
                } else {
                    format!("对方主动断开连接：{reason}")
                }),
            );
        }
        Message::ClipboardText { sha256, .. } => {
            let device_id = state
                .peer_for_generation(generation)
                .ok_or(AppError::NotConnected)?;
            let enabled = *state
                .inner
                .clipboard_enabled
                .lock()
                .expect("剪贴板状态锁损坏");
            if !enabled {
                return Ok(());
            }
            if frame.payload.len() > MAX_CLIPBOARD_TEXT_BYTES {
                return Err(AppError::Protocol("远端剪贴板文本超过大小限制".into()));
            }
            if hash_bytes(&frame.payload) != sha256 {
                return Err(AppError::Protocol("远端剪贴板内容校验失败".into()));
            }
            let text = String::from_utf8(frame.payload)
                .map_err(|_| AppError::Protocol("远端剪贴板不是有效 UTF-8 文本".into()))?;
            state
                .send_clipboard_command(ClipboardCommand::ApplyRemoteText(device_id, text, sha256));
            state.emit_notice(&app, NoticeLevel::Info, "已同步对方剪贴板文本");
        }
        Message::FileOffer {
            transfer_id,
            name,
            size,
            ..
        } => {
            let device_id = state
                .peer_for_generation(generation)
                .ok_or(AppError::NotConnected)?;
            if let Err(error) =
                files::accept_file_offer(&app, &state, device_id, transfer_id, name, size).await
            {
                let _ = state
                    .send_frame_to(
                        device_id,
                        Frame::new(Message::TransferAck {
                            transfer_id,
                            accepted: false,
                            detail: error.to_string(),
                        }),
                    )
                    .await;
                return Err(error);
            }
        }
        Message::FileChunk {
            transfer_id,
            offset,
        } => {
            let device_id = state
                .peer_for_generation(generation)
                .ok_or(AppError::NotConnected)?;
            files::accept_file_chunk(&app, &state, device_id, transfer_id, offset, frame.payload)
                .await?;
        }
        Message::FileComplete {
            transfer_id,
            sha256,
        } => {
            let device_id = state
                .peer_for_generation(generation)
                .ok_or(AppError::NotConnected)?;
            files::complete_incoming_file(&app, &state, device_id, transfer_id, sha256).await?;
        }
        Message::TransferAck {
            transfer_id,
            accepted,
            detail,
        } => {
            let device_id = state
                .peer_for_generation(generation)
                .ok_or(AppError::NotConnected)?;
            state.resolve_transfer_ack(device_id, transfer_id, accepted, detail.clone());
            state.update_transfer(
                transfer_id,
                None,
                Some(if accepted {
                    TransferState::Completed
                } else {
                    TransferState::Failed
                }),
                Some(detail),
            );
            state.emit_snapshot(&app);
        }
        Message::TransferCancel {
            transfer_id,
            reason,
        } => {
            let device_id = state
                .peer_for_generation(generation)
                .ok_or(AppError::NotConnected)?;
            files::accept_transfer_cancel(&app, &state, device_id, transfer_id, reason).await?;
        }
        Message::TransferCancelAck {
            transfer_id,
            result,
            detail,
        } => {
            let device_id = state
                .peer_for_generation(generation)
                .ok_or(AppError::NotConnected)?;
            state.finish_cancel_ack_wait(device_id, transfer_id);
            state.update_transfer(
                transfer_id,
                None,
                Some(match result {
                    TransferCancelResult::Canceled => TransferState::Canceled,
                    TransferCancelResult::AlreadyCompleted => TransferState::Completed,
                    TransferCancelResult::NotFound => TransferState::Canceled,
                }),
                Some(detail),
            );
            state.emit_snapshot(&app);
        }
        Message::ShareFileRequest {
            request_id,
            share_id,
            relative_path,
        } => {
            let device_id = state
                .peer_for_generation(generation)
                .ok_or(AppError::NotConnected)?;
            let app_clone = app.clone();
            let request_state = state.clone();
            tauri::async_runtime::spawn(async move {
                let _ = files::send_shared_file(
                    app_clone,
                    request_state,
                    device_id,
                    request_id,
                    share_id,
                    relative_path,
                )
                .await;
            });
        }
        Message::ShareRootsRequest { request_id } => {
            let device_id = state
                .peer_for_generation(generation)
                .ok_or(AppError::NotConnected)?;
            files::send_share_roots(&state, device_id, Some(request_id)).await?;
        }
        Message::ShareRoots { revision, .. } => {
            let device_id = state
                .peer_for_generation(generation)
                .ok_or(AppError::NotConnected)?;
            files::accept_share_roots(&state, device_id, revision, &frame.payload)?;
            state.emit_snapshot(&app);
        }
        Message::DirectoryListRequest {
            request_id,
            share_id,
            relative_path,
            offset,
        } => {
            let device_id = state
                .peer_for_generation(generation)
                .ok_or(AppError::NotConnected)?;
            if let Err(error) = files::send_directory_page(
                &state,
                device_id,
                request_id,
                share_id,
                relative_path,
                offset,
            )
            .await
            {
                state
                    .send_frame_to(
                        device_id,
                        Frame::new(Message::Error {
                            request_id: Some(request_id),
                            message: error.to_string(),
                        }),
                    )
                    .await?;
            }
        }
        Message::DirectoryListResponse {
            request_id,
            share_id,
            relative_path,
            offset,
            next_offset,
        } => {
            let device_id = state
                .peer_for_generation(generation)
                .ok_or(AppError::NotConnected)?;
            files::accept_directory_page(
                &state,
                device_id,
                files::DirectoryPageMeta {
                    request_id,
                    share_id,
                    relative_path,
                    offset,
                    next_offset,
                },
                &frame.payload,
            )
            .await?;
        }
        Message::Error {
            request_id,
            message,
        } => {
            if let (Some(device_id), Some(request_id)) =
                (state.peer_for_generation(generation), request_id)
            {
                state
                    .resolve_directory_request(
                        device_id,
                        request_id,
                        Err(AppError::Protocol(message.clone())),
                    )
                    .await;
            }
            state.emit_notice(&app, NoticeLevel::Error, message);
        }
    }
    Ok(())
}
