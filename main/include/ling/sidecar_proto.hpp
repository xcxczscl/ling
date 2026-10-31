// 侧车协议：一行一个 JSON 对象（jsonl）。
// 字段与握手规则见 docs/ling/04-详细设计/02-插件装载与接口.md §8。
//
// 这里只做「帧」这一层：拼、解析、校验。进程怎么起、管道怎么读、要不要重启，
// 都不在这一层。分开的理由和依赖图一样——协议规则的错（少字段、版本不符、超长）
// 全能用单测压住，不必真起一个进程。
#pragma once

#include <cstddef>
#include <string>

#include <nlohmann/json.hpp>

namespace ling {
namespace sidecar {

constexpr int kProtocolVersion = 1;
constexpr std::size_t kMaxFrameBytes = 1024 * 1024;  // 与控制面载荷上限一致（§8.6）

// 一帧。`fields` 是除 v/id/op 之外的字段原文，原样转发，避免每加一个字段就改结构体。
struct Frame {
    int id = 0;                     // 请求序号；没有就是 0
    std::string op;                 // 消息类型：hello / activate / invoke / …
    nlohmann::json fields = nlohmann::json::object();
};

// 解析一行（不含换行符）。失败时 error 给出能直接给人看的原因。
bool parse(const std::string& line, Frame* out, std::string* error);

// 组装一行（含结尾换行）。fields 里的 v/id/op 会被覆盖成本帧的值。
std::string encode(const Frame& frame);

// 两个最常用的应答：成功与失败。错误结构见 §5.3。
std::string encode_result(int id, const nlohmann::json& result);
std::string encode_error(int id, const std::string& code, const std::string& message);

// 侧车到宿主的七种 op 与宿主到侧车的五种 op（§8.2、§8.3）。
bool is_sidecar_op(const std::string& op);
bool is_host_op(const std::string& op);

}  // namespace sidecar
}  // namespace ling
