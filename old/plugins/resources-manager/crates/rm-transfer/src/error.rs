//! 错误类型：带稳定 F 族错误码（`docs/03` §2.7）。

use rm_proto::codes;

/// 传输/配对错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferError {
    pub code: String,
    pub message: String,
}

impl TransferError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }

    /// F401：配对码无效或过期。
    pub fn bad_code(msg: impl Into<String>) -> Self {
        Self::new(codes::F401, msg)
    }

    /// F402：指纹不匹配（换证书 / 防中间人）。
    pub fn fingerprint_mismatch(msg: impl Into<String>) -> Self {
        Self::new(codes::F402, msg)
    }

    /// F403：冷备份或策略禁止。
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::new(codes::F403, msg)
    }

    /// F404：对端不可达。
    pub fn unreachable(msg: impl Into<String>) -> Self {
        Self::new(codes::F404, msg)
    }

    /// F405：校验失败。
    pub fn corrupt(msg: impl Into<String>) -> Self {
        Self::new(codes::F405, msg)
    }

    pub fn io(msg: impl Into<String>) -> Self {
        Self::new("F301", msg)
    }

    pub fn proto(msg: impl Into<String>) -> Self {
        Self::new("F301", msg)
    }
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for TransferError {}

impl From<TransferError> for rm_proto::BizError {
    fn from(e: TransferError) -> Self {
        rm_proto::BizError::new(&e.code, e.message)
    }
}
