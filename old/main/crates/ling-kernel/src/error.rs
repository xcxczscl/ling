use std::fmt;

/// 内核级错误（安装/装载/管理操作）。`ecode` 返回协议错误码（`docs/04` §8），
/// 无对应码的错误（如 IO）返回 `None`。
#[derive(Debug)]
pub enum KernelError {
    Manifest { code: &'static str, message: String },
    AlreadyInstalled(String),
    AlreadyLoaded(String),
    NotFound(String),
    Unsupported(String),
    Load { message: String },
    Io { op: String, message: String },
    CapabilityState { message: String },
    DependencyCycle { path: Vec<String> },
    Reload { message: String },
}

impl KernelError {
    pub fn manifest(code: &'static str, message: impl Into<String>) -> Self {
        KernelError::Manifest {
            code,
            message: message.into(),
        }
    }

    pub fn load(message: impl Into<String>) -> Self {
        KernelError::Load {
            message: message.into(),
        }
    }

    pub fn io(op: impl Into<String>, message: impl Into<String>) -> Self {
        KernelError::Io {
            op: op.into(),
            message: message.into(),
        }
    }

    pub fn dependency_cycle(path: Vec<String>) -> Self {
        KernelError::DependencyCycle { path }
    }

    pub fn reload(message: impl Into<String>) -> Self {
        KernelError::Reload {
            message: message.into(),
        }
    }

    pub fn ecode(&self) -> Option<&'static str> {
        match self {
            KernelError::Manifest { code, .. } => Some(code),
            KernelError::Load { .. } => Some(ling_sdk::codes::E_ACTIVATE_FAILED),
            KernelError::DependencyCycle { .. } => Some(ling_sdk::codes::E_DEPENDENCY_CYCLE),
            KernelError::Reload { .. } => Some(ling_sdk::codes::E_RELOAD_FAILED),
            _ => None,
        }
    }
}

impl fmt::Display for KernelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KernelError::Manifest { code, message } => write!(f, "{code}: {message}"),
            KernelError::AlreadyInstalled(id) => write!(f, "插件已安装：{id}"),
            KernelError::AlreadyLoaded(id) => write!(f, "插件已装载：{id}"),
            KernelError::NotFound(id) => write!(f, "找不到插件：{id}"),
            KernelError::Unsupported(msg) => f.write_str(msg),
            KernelError::Load { message } => {
                write!(f, "{}: {message}", ling_sdk::codes::E_ACTIVATE_FAILED)
            }
            KernelError::Io { op, message } => write!(f, "{op}: {message}"),
            KernelError::CapabilityState { message } => f.write_str(message),
            KernelError::DependencyCycle { path } => write!(
                f,
                "{}: 形成环 {}",
                ling_sdk::codes::E_DEPENDENCY_CYCLE,
                path.join(" → ")
            ),
            KernelError::Reload { message } => {
                write!(f, "{}: {message}", ling_sdk::codes::E_RELOAD_FAILED)
            }
        }
    }
}

impl std::error::Error for KernelError {}
