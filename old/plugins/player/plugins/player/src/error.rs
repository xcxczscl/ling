//! player P0 错误码（P 族，`docs/01` §8）：P100 通用参数；P1xx 内容/解码；
//! P2xx 串流；P3xx 运行/沙箱；P4xx 会话。与 rm 的 F、ling 的 E 不重叠。

use std::fmt;

/// player 业务错误：经 ling 方法路由放在 `error.code`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerError {
    pub code: &'static str,
    pub message: String,
}

impl PlayerError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }

    /// P100 通用参数错误（缺参/非法参数/未知页面；附录 B #22 登记入 §8）。
    pub fn bad_param(msg: impl Into<String>) -> Self {
        Self::new("P100", msg)
    }
    /// P101 内容不可用（rm F501/离线/丢失）。
    pub fn content_unavailable(msg: impl Into<String>) -> Self {
        Self::new("P101", msg)
    }
    /// P104 无可用解码器。
    pub fn no_decoder(msg: impl Into<String>) -> Self {
        Self::new("P104", msg)
    }
    /// P401 会话不存在。
    pub fn session_not_found(id: &str) -> Self {
        Self::new("P401", format!("会话不存在：{id}"))
    }
    /// P103 不支持的资产类型。
    pub fn unsupported_asset(msg: impl Into<String>) -> Self {
        Self::new("P103", msg)
    }
    /// P201 串流目标不可达（对端 lite 接收器未知/无响应）。
    pub fn target_unreachable(msg: impl Into<String>) -> Self {
        Self::new("P201", msg)
    }
    /// P202 串流被拒绝（接收器返回 `accepted:false`）。
    pub fn stream_rejected(msg: impl Into<String>) -> Self {
        Self::new("P202", msg)
    }
    /// P203 代理端口分配失败（7914 被占用且顺延失败）。
    pub fn proxy_port_failed(msg: impl Into<String>) -> Self {
        Self::new("P203", msg)
    }
    /// P301 沙箱启动失败（OS 隔离 API 调用失败；`docs/01` §8，P5）。
    pub fn sandbox_start_failed(msg: impl Into<String>) -> Self {
        Self::new("P301", msg)
    }
    /// P302 沙箱权限不足（试图访问未授权路径/清单非法；§8，P5）。
    pub fn sandbox_path_denied(msg: impl Into<String>) -> Self {
        Self::new("P302", msg)
    }
    /// P303 远程执行不可用（§8：对端未运行/未配对/无运行能力；P7）。
    pub fn remote_unavailable(msg: impl Into<String>) -> Self {
        Self::new("P303", msg)
    }
}

impl fmt::Display for PlayerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.code, self.message)
    }
}

impl From<PlayerError> for ling_sdk::CallError {
    fn from(e: PlayerError) -> Self {
        ling_sdk::CallError::new(e.code, e.message)
    }
}

/// 把 rm 侧 F 族错误翻译为 P 族（契约：player 对外只出 P 码）。
pub fn from_rm_err(e: &ling_sdk::CallError) -> PlayerError {
    match e.code() {
        "F501" => PlayerError::content_unavailable(e.message()),
        "F502" => PlayerError::new("P102", format!("流 token 失效：{}", e.message())),
        "F504" => PlayerError::new("P102", format!("流服务未启动：{}", e.message())),
        other => PlayerError::new("P101", format!("{other} {}", e.message())),
    }
}
