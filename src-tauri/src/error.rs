use thiserror::Error;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("尚未连接另一台设备")]
    NotConnected,
    #[error("传输已取消")]
    TransferCanceled,
    #[error("输入无效：{0}")]
    InvalidInput(String),
    #[error("协议错误：{0}")]
    Protocol(String),
    #[error("安全校验失败：{0}")]
    Security(String),
    #[cfg_attr(target_os = "windows", allow(dead_code))]
    #[error("当前平台暂不支持：{0}")]
    Unsupported(String),
    #[error("蓝牙操作失败：{0}")]
    Bluetooth(String),
    #[error("文件操作失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("数据序列化失败：{0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(target_os = "windows")]
impl From<windows::core::Error> for AppError {
    fn from(value: windows::core::Error) -> Self {
        Self::Bluetooth(value.to_string())
    }
}
