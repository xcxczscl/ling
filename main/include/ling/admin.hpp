// 管理端口：只绑回环、jsonl 帧、首帧认证。
// 语义依据 docs/ling/04-详细设计/06-管理端口与命令行.md §2、§3、§6。
#pragma once

#include <cstdint>
#include <filesystem>
#include <memory>
#include <string>

namespace ling {

class Kernel;

// 一次管理调用的结果。
struct AdminReply {
    enum class Kind {
        kOk,          // 方法返回成功
        kUnreachable, // 宿主没起、端口没响应、找不到 registry.json（退出码 3）
        kAuth,        // 口令不对或没有口令文件（退出码 4）
        kRemote,      // 宿主返回了错误（退出码 4）
        kProtocol,    // 帧不合规、版本不匹配（退出码 4）
    };

    Kind kind = Kind::kUnreachable;
    std::string code;     // 错误码，如 E_PLUGIN_MISSING
    std::string message;  // 给人看的一句话
    std::string result;   // 成功时的 result 字段原文（JSON）
    bool ok() const { return kind == Kind::kOk; }
};

class AdminServer {
public:
    explicit AdminServer(Kernel* kernel);
    ~AdminServer();

    AdminServer(const AdminServer&) = delete;
    AdminServer& operator=(const AdminServer&) = delete;

    // 从 start_port 起在 127.0.0.1 上找第一个可绑定的端口，最多试到 last_port。
    // 成功后写 session.json。返回空表示成功，否则是给人看的错误说明。
    std::string start(std::uint16_t start_port, std::uint16_t last_port);

    std::uint16_t port() const;

    // 处理一轮事件，最多等 timeout_ms。返回 false 表示收到了 kernel.shutdown。
    bool poll(int timeout_ms);

    void stop();

private:
    struct Impl;
    std::unique_ptr<Impl> impl_;
};

namespace admin {

// 从 <数据根>/registry.json 的 port 字段读端口。
bool read_port(const std::filesystem::path& data_root, std::uint16_t* out_port, std::string* error);

// 读 <数据根>/session.json 的 token。
bool read_token(const std::filesystem::path& data_root, std::string* out_token, std::string* error);

// 一次完整的客户端调用：连端口、认证、发一帧、读一帧。
AdminReply call(const std::filesystem::path& data_root, const std::string& method,
                const std::string& args_json, int timeout_ms);

}  // namespace admin

}  // namespace ling
