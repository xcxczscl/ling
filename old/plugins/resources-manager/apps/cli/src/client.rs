//! 命令行错误分类：Usage → 退出码 2；Blocked → 3；Biz → 1。

pub enum CmdError {
    Usage(String),
    Blocked(String),
    Biz(String),
}
