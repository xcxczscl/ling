//! 数据同步（M3）：局域网身份、配对握手、blob 传输、断点续传、按需策略。
//!
//! 协议冻结于 `docs/03` §5.2。无 async/tokio：`std::net` + 线程。

pub mod base32;
pub mod client;
pub mod error;
pub mod http;
pub mod identity;
pub mod op_client;
pub mod pairing;
pub mod policy;
pub mod protocol;
pub mod server;

pub use error::TransferError;
pub use identity::{fingerprint_of, Identity};
pub use protocol::{DEFAULT_PORT, PAIRING_TTL_SECS, PARALLEL, PROTO, TS_SKEW_SECS};
pub use server::{PairingState, ServerCatalog, ServerConfig, ServerHandle};
