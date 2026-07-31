use std::{sync::mpsc, thread, time::Duration};

use arboard::Clipboard;
use sha2::{Digest, Sha256};
use tauri::AppHandle;
use uuid::Uuid;

use crate::{
    models::NoticeLevel,
    protocol::{Frame, Message},
    state::{AppState, ClipboardCommand},
};

pub const MAX_CLIPBOARD_TEXT_BYTES: usize = 512 * 1024;

pub fn start_clipboard_worker(app: AppHandle, state: AppState) {
    let (sender, receiver) = mpsc::channel();
    state.set_clipboard_command_sender(sender);

    #[cfg(target_os = "windows")]
    let monitor_shutdown = start_windows_monitor(app.clone(), state.clone());
    #[cfg(not(target_os = "windows"))]
    let monitor_shutdown = ();

    let _ = thread::Builder::new()
        .name("nearweave-clipboard".into())
        .spawn(move || clipboard_worker(app, state, receiver, monitor_shutdown));
}

#[cfg(target_os = "windows")]
fn start_windows_monitor(
    app: AppHandle,
    state: AppState,
) -> Option<clipboard_win::monitor::Shutdown> {
    let sender = state
        .inner
        .clipboard_commands
        .lock()
        .expect("剪贴板命令锁损坏")
        .as_ref()
        .cloned()
        .expect("剪贴板命令发送端应已初始化");
    let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
    let monitor_app = app.clone();
    let monitor_state = state.clone();

    let spawn_result = thread::Builder::new()
        .name("nearweave-clipboard-monitor".into())
        .spawn(move || {
            let mut monitor = match clipboard_win::Monitor::new() {
                Ok(monitor) => monitor,
                Err(error) => {
                    let message = format!("无法监听 Windows 剪贴板事件：{error}");
                    let _ = ready_sender.send(Err(message.clone()));
                    monitor_state.emit_notice(&monitor_app, NoticeLevel::Error, message);
                    return;
                }
            };
            let shutdown = monitor.shutdown_channel();
            if ready_sender.send(Ok(shutdown)).is_err() {
                return;
            }

            loop {
                match monitor.recv() {
                    Ok(true) => {
                        if sender
                            .send(ClipboardCommand::LocalClipboardChanged)
                            .is_err()
                        {
                            return;
                        }
                    }
                    Ok(false) => return,
                    Err(error) => {
                        monitor_state.emit_notice(
                            &monitor_app,
                            NoticeLevel::Error,
                            format!("Windows 剪贴板事件监听已停止：{error}"),
                        );
                        return;
                    }
                }
            }
        });
    if let Err(error) = spawn_result {
        state.emit_notice(
            &app,
            NoticeLevel::Error,
            format!("无法创建 Windows 剪贴板监听线程：{error}"),
        );
        return None;
    }

    match ready_receiver.recv_timeout(Duration::from_secs(2)) {
        Ok(Ok(shutdown)) => Some(shutdown),
        Ok(Err(_)) => None,
        Err(error) => {
            state.emit_notice(
                &app,
                NoticeLevel::Error,
                format!("等待 Windows 剪贴板监听器启动失败：{error}"),
            );
            None
        }
    }
}

fn clipboard_worker<T>(
    app: AppHandle,
    state: AppState,
    receiver: mpsc::Receiver<ClipboardCommand>,
    _monitor_shutdown: T,
) {
    let mut clipboard = match Clipboard::new() {
        Ok(clipboard) => clipboard,
        Err(error) => {
            state.emit_notice(
                &app,
                NoticeLevel::Error,
                format!("无法访问系统剪贴板：{error}"),
            );
            return;
        }
    };
    let mut last_sent_hash = String::new();

    while let Ok(command) = receiver.recv() {
        match command {
            ClipboardCommand::ApplyRemoteText(_device_id, text, hash) => {
                if let Err(error) = clipboard.set_text(text) {
                    state.emit_notice(&app, NoticeLevel::Error, format!("写入剪贴板失败：{error}"));
                } else {
                    last_sent_hash = hash;
                }
            }
            ClipboardCommand::LocalClipboardChanged => {
                sync_local_text(&app, &state, &mut clipboard, &mut last_sent_hash);
            }
            ClipboardCommand::SyncPeer(device_id) => {
                if *state
                    .inner
                    .clipboard_enabled
                    .lock()
                    .expect("剪贴板状态锁损坏")
                {
                    sync_peer_text(&app, &state, device_id, &mut clipboard, &mut last_sent_hash);
                }
            }
            ClipboardCommand::Stop => return,
        }
    }
}

fn sync_peer_text(
    app: &AppHandle,
    state: &AppState,
    device_id: Uuid,
    clipboard: &mut Clipboard,
    last_sent_hash: &mut String,
) {
    let Ok(text) = clipboard.get_text() else {
        return;
    };
    let bytes = text.as_bytes();
    if bytes.len() > MAX_CLIPBOARD_TEXT_BYTES {
        state.emit_notice(app, NoticeLevel::Info, "剪贴板文本超过 512 KiB，本次未同步");
        return;
    }
    let hash = hash_bytes(bytes);
    let frame = Frame::with_payload(
        Message::ClipboardText {
            message_id: Uuid::new_v4(),
            sha256: hash.clone(),
        },
        bytes.to_vec(),
    );
    if state.try_send_frame_to(device_id, frame).is_ok() {
        *last_sent_hash = hash;
    }
}

fn sync_local_text(
    app: &AppHandle,
    state: &AppState,
    clipboard: &mut Clipboard,
    last_sent_hash: &mut String,
) {
    let enabled = *state
        .inner
        .clipboard_enabled
        .lock()
        .expect("剪贴板状态锁损坏");
    if !enabled {
        return;
    }

    let Ok(text) = clipboard.get_text() else {
        return;
    };
    let bytes = text.as_bytes();
    let hash = hash_bytes(bytes);
    if hash == *last_sent_hash {
        return;
    }
    if bytes.len() > MAX_CLIPBOARD_TEXT_BYTES {
        *last_sent_hash = hash;
        state.emit_notice(app, NoticeLevel::Info, "剪贴板文本超过 512 KiB，本次未同步");
        return;
    }

    let frame = Frame::with_payload(
        Message::ClipboardText {
            message_id: Uuid::new_v4(),
            sha256: hash.clone(),
        },
        bytes.to_vec(),
    );
    if state.try_send_frame(frame).is_ok() {
        *last_sent_hash = hash;
    }
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
