#include "ling/sidecar_proto.hpp"

#include <cstdint>
#include <limits>
#include <string>

#include "ling/errors.hpp"

namespace ling {
namespace sidecar {
namespace {

constexpr const char* kSidecarOps[] = {"hello", "register_method", "subscribe", "call",
                                       "emit",  "result",          "error",     "deactivated"};
constexpr const char* kHostOps[] = {"activate", "invoke", "event", "deactivate", "shutdown"};

template <std::size_t N>
bool contains(const char* const (&values)[N], const std::string& text) {
    for (const char* value : values) {
        if (text == value) {
            return true;
        }
    }
    return false;
}

}  // namespace

bool parse(const std::string& line, Frame* out, std::string* error) {
    if (line.size() > kMaxFrameBytes) {
        *error = "一帧超过 1 MB";
        return false;
    }
    if (line.empty()) {
        *error = "空帧";
        return false;
    }

    nlohmann::json document = nlohmann::json::parse(line, nullptr, false);
    if (document.is_discarded()) {
        *error = "帧不是合法 JSON";
        return false;
    }
    if (!document.is_object()) {
        *error = "帧必须是 JSON 对象";
        return false;
    }
    if (!document.contains("v") || !document["v"].is_number_integer()) {
        *error = "缺少协议版本 v";
        return false;
    }
    // v / id 都是外部输入：直接 get<int>() 遇到 3e9 这类值会抛 out_of_range，
    // 先按 int64 取再判断，坏帧只能得到「版本不匹配」，不该变成异常。
    std::int64_t version = 0;
    if (document["v"].is_number_unsigned()) {
        const std::uint64_t raw = document["v"].get<std::uint64_t>();
        version = raw > static_cast<std::uint64_t>(std::numeric_limits<std::int32_t>::max())
                      ? std::numeric_limits<std::int32_t>::max()
                      : static_cast<std::int64_t>(raw);
    } else if (document["v"].is_number_integer()) {
        version = document["v"].get<std::int64_t>();
    } else {
        *error = "协议版本 v 必须是整数";
        return false;
    }
    if (version != kProtocolVersion) {
        *error = "协议版本 " + std::to_string(version) + " 与宿主 " +
                 std::to_string(kProtocolVersion) + " 不同";
        return false;
    }
    if (!document.contains("op") || !document["op"].is_string()) {
        *error = "缺少 op";
        return false;
    }

    out->op = document["op"].get<std::string>();
    out->id = 0;
    if (document.contains("id")) {
        const auto& id_node = document["id"];
        if (id_node.is_number_unsigned()) {
            const std::uint64_t raw = id_node.get<std::uint64_t>();
            if (raw <= static_cast<std::uint64_t>(std::numeric_limits<int>::max())) {
                out->id = static_cast<int>(raw);
            }
        } else if (id_node.is_number_integer()) {
            const std::int64_t raw = id_node.get<std::int64_t>();
            if (raw >= std::numeric_limits<int>::min() &&
                raw <= std::numeric_limits<int>::max()) {
                out->id = static_cast<int>(raw);
            }
        }
    }
    out->fields = std::move(document);
    return true;
}

std::string encode(const Frame& frame) {
    nlohmann::json document = frame.fields.is_object() ? frame.fields : nlohmann::json::object();
    document["v"] = kProtocolVersion;
    document["op"] = frame.op;
    if (frame.id != 0) {
        document["id"] = frame.id;
    }
    return document.dump() + "\n";
}

std::string encode_result(int id, const nlohmann::json& result) {
    nlohmann::json document;
    document["v"] = kProtocolVersion;
    if (id != 0) {
        document["id"] = id;
    }
    document["op"] = "result";
    document["ok"] = true;
    document["result"] = result;
    return document.dump() + "\n";
}

std::string encode_error(int id, const std::string& code, const std::string& message) {
    nlohmann::json document;
    document["v"] = kProtocolVersion;
    if (id != 0) {
        document["id"] = id;
    }
    document["op"] = "error";
    document["ok"] = false;
    document["error"]["code"] = code;
    document["error"]["message"] = message;
    return document.dump() + "\n";
}

bool is_sidecar_op(const std::string& op) {
    return contains(kSidecarOps, op);
}

bool is_host_op(const std::string& op) {
    return contains(kHostOps, op);
}

}  // namespace sidecar
}  // namespace ling
