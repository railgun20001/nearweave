use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::{AppError, AppResult},
    models::TransferCancelResult,
};

pub const PROTOCOL_VERSION: u16 = 1;
pub const FRAME_PREFIX_SIZE: usize = 20;
pub const MAX_HEADER_SIZE: usize = 256 * 1024;
pub const MAX_PAYLOAD_SIZE: usize = 1024 * 1024;
pub const FILE_CHUNK_SIZE: usize = 48 * 1024;
pub const CAPABILITY_TRANSFER_CANCEL: &str = "transfer_cancel_v1";
pub const CAPABILITY_LAZY_DIRECTORY: &str = "lazy_directory_browse_v1";

const MAGIC: &[u8; 4] = b"NWV1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Message {
    Hello {
        protocol_version: u16,
        device_id: Uuid,
        device_name: String,
        capabilities: Vec<String>,
        #[serde(default)]
        network_offer: Option<NetworkOffer>,
    },
    NetworkHello {
        device_id: Uuid,
        session_id: Uuid,
        connection_id: Uuid,
    },
    Ping {
        nonce: Uuid,
    },
    Pong {
        nonce: Uuid,
    },
    Disconnect {
        reason: String,
    },
    ClipboardText {
        message_id: Uuid,
        sha256: String,
    },
    FileOffer {
        transfer_id: Uuid,
        name: String,
        size: u64,
        source: TransferSource,
    },
    FileChunk {
        transfer_id: Uuid,
        offset: u64,
    },
    FileComplete {
        transfer_id: Uuid,
        sha256: String,
    },
    TransferAck {
        transfer_id: Uuid,
        accepted: bool,
        detail: String,
    },
    TransferCancel {
        transfer_id: Uuid,
        reason: String,
    },
    TransferCancelAck {
        transfer_id: Uuid,
        result: TransferCancelResult,
        detail: String,
    },
    ShareFileRequest {
        request_id: Uuid,
        share_id: Uuid,
        relative_path: String,
    },
    ShareRootsRequest {
        request_id: Uuid,
    },
    ShareRoots {
        request_id: Option<Uuid>,
        revision: Uuid,
    },
    DirectoryListRequest {
        request_id: Uuid,
        share_id: Uuid,
        relative_path: String,
        offset: u32,
    },
    DirectoryListResponse {
        request_id: Uuid,
        share_id: Uuid,
        relative_path: String,
        offset: u32,
        next_offset: Option<u32>,
    },
    Error {
        request_id: Option<Uuid>,
        message: String,
    },
}

impl Message {
    pub fn prefers_network(&self) -> bool {
        !matches!(
            self,
            Self::Hello { .. }
                | Self::NetworkHello { .. }
                | Self::Ping { .. }
                | Self::Pong { .. }
                | Self::Disconnect { .. }
                | Self::TransferCancel { .. }
                | Self::TransferCancelAck { .. }
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkOffer {
    pub session_id: Uuid,
    pub key: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferSource {
    Direct,
    SharedDirectory,
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub message: Message,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(message: Message) -> Self {
        Self {
            message,
            payload: Vec::new(),
        }
    }

    pub fn with_payload(message: Message, payload: Vec<u8>) -> Self {
        Self { message, payload }
    }

    pub fn encode(&self) -> AppResult<Vec<u8>> {
        let header = serde_json::to_vec(&self.message)?;
        validate_lengths(header.len(), self.payload.len())?;

        let mut output = Vec::with_capacity(FRAME_PREFIX_SIZE + header.len() + self.payload.len());
        output.extend_from_slice(MAGIC);
        output.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
        output.extend_from_slice(&[0, 0]);
        output.extend_from_slice(&(header.len() as u32).to_be_bytes());
        output.extend_from_slice(&(self.payload.len() as u64).to_be_bytes());
        output.extend_from_slice(&header);
        output.extend_from_slice(&self.payload);
        Ok(output)
    }

    pub fn decode(bytes: &[u8]) -> AppResult<Self> {
        if bytes.len() < FRAME_PREFIX_SIZE {
            return Err(AppError::Protocol("消息头不完整".into()));
        }
        let (header_len, payload_len) = decode_prefix(&bytes[..FRAME_PREFIX_SIZE])?;
        let expected = FRAME_PREFIX_SIZE
            .checked_add(header_len)
            .and_then(|value| value.checked_add(payload_len))
            .ok_or_else(|| AppError::Protocol("消息长度溢出".into()))?;
        if bytes.len() != expected {
            return Err(AppError::Protocol(format!(
                "消息长度不匹配，期望 {expected} 字节，实际 {} 字节",
                bytes.len()
            )));
        }

        let header_end = FRAME_PREFIX_SIZE + header_len;
        let message = serde_json::from_slice(&bytes[FRAME_PREFIX_SIZE..header_end])?;
        Ok(Self {
            message,
            payload: bytes[header_end..].to_vec(),
        })
    }
}

pub fn decode_prefix(prefix: &[u8]) -> AppResult<(usize, usize)> {
    if prefix.len() != FRAME_PREFIX_SIZE {
        return Err(AppError::Protocol("固定消息头长度错误".into()));
    }
    if &prefix[..4] != MAGIC {
        return Err(AppError::Protocol("无法识别的消息标记".into()));
    }

    let version = u16::from_be_bytes([prefix[4], prefix[5]]);
    if version != PROTOCOL_VERSION {
        return Err(AppError::Protocol(format!("不支持的协议版本 {version}")));
    }

    let header_len = u32::from_be_bytes(prefix[8..12].try_into().expect("长度固定")) as usize;
    let payload_len_u64 = u64::from_be_bytes(prefix[12..20].try_into().expect("长度固定"));
    let payload_len = usize::try_from(payload_len_u64)
        .map_err(|_| AppError::Protocol("负载长度超出当前平台限制".into()))?;
    validate_lengths(header_len, payload_len)?;
    Ok((header_len, payload_len))
}

fn validate_lengths(header_len: usize, payload_len: usize) -> AppResult<()> {
    if header_len == 0 || header_len > MAX_HEADER_SIZE {
        return Err(AppError::Protocol(format!(
            "消息头长度 {header_len} 超出限制"
        )));
    }
    if payload_len > MAX_PAYLOAD_SIZE {
        return Err(AppError::Protocol(format!(
            "负载长度 {payload_len} 超出限制"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip_preserves_binary_payload() {
        let frame = Frame::with_payload(
            Message::ClipboardText {
                message_id: Uuid::nil(),
                sha256: "abc".into(),
            },
            vec![0, 1, 2, 255],
        );

        let decoded = Frame::decode(&frame.encode().expect("编码应成功")).expect("解码应成功");
        assert!(matches!(decoded.message, Message::ClipboardText { .. }));
        assert_eq!(decoded.payload, vec![0, 1, 2, 255]);
    }

    #[test]
    fn disconnect_frame_round_trip_preserves_reason() {
        let frame = Frame::new(Message::Disconnect {
            reason: "用户主动断开".into(),
        });

        let decoded =
            Frame::decode(&frame.encode().expect("断开消息应能编码")).expect("断开消息应能解码");

        match decoded.message {
            Message::Disconnect { reason } => assert_eq!(reason, "用户主动断开"),
            _ => panic!("应解码为断开消息"),
        }
    }

    #[test]
    fn bluetooth_only_hello_without_network_offer_is_valid() {
        let header = r#"{"kind":"hello","protocol_version":1,"device_id":"00000000-0000-0000-0000-000000000000","device_name":"测试设备","capabilities":["files"]}"#;
        let message: Message = serde_json::from_slice(header.as_bytes()).expect("Hello 应可解码");

        match message {
            Message::Hello { network_offer, .. } => assert!(network_offer.is_none()),
            _ => panic!("应解码为 Hello"),
        }
    }

    #[test]
    fn bulk_messages_prefer_network_but_control_messages_do_not() {
        assert!(
            Message::FileChunk {
                transfer_id: Uuid::nil(),
                offset: 0,
            }
            .prefers_network()
        );
        assert!(!Message::Ping { nonce: Uuid::nil() }.prefers_network());
        assert!(
            !Message::TransferCancel {
                transfer_id: Uuid::nil(),
                reason: "测试取消".into(),
            }
            .prefers_network()
        );
    }

    #[test]
    fn transfer_cancel_round_trip_preserves_result() {
        let transfer_id = Uuid::new_v4();
        let frame = Frame::new(Message::TransferCancelAck {
            transfer_id,
            result: TransferCancelResult::AlreadyCompleted,
            detail: "任务已经完成".into(),
        });

        let decoded =
            Frame::decode(&frame.encode().expect("取消确认应能编码")).expect("取消确认应能解码");
        assert!(matches!(
            decoded.message,
            Message::TransferCancelAck {
                transfer_id: decoded_id,
                result: TransferCancelResult::AlreadyCompleted,
                ..
            } if decoded_id == transfer_id
        ));
    }

    #[test]
    fn directory_page_round_trip_preserves_paging_fields() {
        let request_id = Uuid::new_v4();
        let share_id = Uuid::new_v4();
        let frame = Frame::new(Message::DirectoryListResponse {
            request_id,
            share_id,
            relative_path: "一级/二级".into(),
            offset: 200,
            next_offset: Some(400),
        });

        let decoded =
            Frame::decode(&frame.encode().expect("目录分页应能编码")).expect("目录分页应能解码");
        assert!(matches!(
            decoded.message,
            Message::DirectoryListResponse {
                request_id: decoded_request,
                share_id: decoded_share,
                offset: 200,
                next_offset: Some(400),
                ..
            } if decoded_request == request_id && decoded_share == share_id
        ));
    }

    #[test]
    fn invalid_magic_is_rejected() {
        let mut encoded = Frame::new(Message::ShareRootsRequest {
            request_id: Uuid::nil(),
        })
        .encode()
        .expect("编码应成功");
        encoded[0] = b'X';

        let error = Frame::decode(&encoded).expect_err("非法消息必须失败");
        assert!(error.to_string().contains("消息标记"));
    }

    #[test]
    fn encoded_frame_uses_nearweave_magic() {
        let encoded = Frame::new(Message::ShareRootsRequest {
            request_id: Uuid::nil(),
        })
        .encode()
        .expect("编码应成功");

        assert_eq!(&encoded[..4], b"NWV1");
    }

    #[test]
    fn oversized_payload_is_rejected_before_allocation() {
        let mut prefix = [0_u8; FRAME_PREFIX_SIZE];
        prefix[..4].copy_from_slice(MAGIC);
        prefix[4..6].copy_from_slice(&PROTOCOL_VERSION.to_be_bytes());
        prefix[8..12].copy_from_slice(&1_u32.to_be_bytes());
        prefix[12..20].copy_from_slice(&((MAX_PAYLOAD_SIZE as u64) + 1).to_be_bytes());

        assert!(decode_prefix(&prefix).is_err());
    }
}
