#include "ling/router.hpp"

#include <cctype>
#include <cstdlib>
#include <cstring>
#include <exception>
#include <utility>

#include "ling/plugin_api.h"

namespace ling {
namespace {

constexpr std::size_t kMaxMethodNameBytes = 128;
constexpr std::size_t kMaxPayloadBytes = 1024 * 1024;  // 1 MB，LING-REQ-097

}  // namespace

bool valid_method_name(const std::string& name) {
    if (name.empty() || name.size() > kMaxMethodNameBytes) {
        return false;
    }
    for (char ch : name) {
        const auto uch = static_cast<unsigned char>(ch);
        // 允许连字符：方法名以插件编号打头，而编号本身可以含 '-'（如 demo-a.ping）。
        // 04/01 §5.1 第 4 条的字符集已同步放宽，否则带连字符的插件永远注册不了方法。
        const bool ok = std::isalnum(uch) != 0 || ch == '_' || ch == '.' || ch == '-';
        if (!ok) {
            return false;
        }
    }
    return true;
}

std::string Router::register_method(const std::string& plugin_id, const std::string& name,
                                    void* fn, void* handler_ctx, std::uint32_t requires_mask) {
    // 第 2 条：处理函数非空。与「撤销函数为空」同码，见 04/01 §5.1 的理由。
    if (fn == nullptr) {
        return err::kLedgerInvalid;
    }
    // 第 3 条：命名空间前缀。
    const std::string prefix = plugin_id + ".";
    if (name.rfind(prefix, 0) != 0) {
        return err::kNamespaceViolation;
    }
    // 第 4 条：长度与字符集。
    if (!valid_method_name(name)) {
        return err::kManifestInvalid;
    }
    // 第 6 条：不冲突。
    if (routes_.find(name) != routes_.end()) {
        return err::kDuplicateMethod;
    }

    RouteEntry entry;
    entry.plugin_id = plugin_id;
    entry.handler = fn;
    entry.handler_ctx = handler_ctx;
    entry.requires_mask = requires_mask;
    routes_.emplace(name, std::move(entry));
    return {};
}

std::string Router::unregister_method(const std::string& plugin_id, const std::string& name) {
    const auto it = routes_.find(name);
    if (it == routes_.end()) {
        return err::kMethodMissing;
    }
    if (it->second.plugin_id != plugin_id) {
        return err::kCapabilityDenied;
    }
    routes_.erase(it);
    return {};
}

std::vector<std::string> Router::unregister_all(const std::string& plugin_id) {
    std::vector<std::string> removed;
    for (auto it = routes_.begin(); it != routes_.end();) {
        if (it->second.plugin_id == plugin_id) {
            removed.push_back(it->first);
            it = routes_.erase(it);
        } else {
            ++it;
        }
    }
    return removed;
}

bool Router::contains(const std::string& method) const {
    return routes_.find(method) != routes_.end();
}

CallOutcome Router::call(const std::string& method, const std::uint8_t* args,
                         std::size_t len) const {
    CallOutcome outcome;

    // 第 2 步：载荷上限检查在查表之前，避免为超限请求做无用查找。
    if (len > kMaxPayloadBytes) {
        outcome.code = err::kPayloadTooLarge;
        return outcome;
    }

    const auto it = routes_.find(method);
    if (it == routes_.end()) {
        outcome.code = err::kMethodMissing;
        return outcome;
    }

    const auto fn = reinterpret_cast<ling_method_fn>(it->second.handler);
    std::uint8_t* out = nullptr;
    std::size_t out_len = 0;

    int32_t code = 0;
    try {
        code = fn(it->second.handler_ctx, args, len, &out, &out_len);
    } catch (...) {
        // 插件越界抛异常属于违约，但不能让它穿过边界。
        outcome.code = err::kInternal;
        return outcome;
    }

    if (code != 0) {
        outcome.code = err::kInternal;
        return outcome;
    }
    if (out != nullptr && out_len > 0) {
        outcome.data.assign(out, out + out_len);
        // 进程内轨道两侧共用同一个 /MD 运行时，插件用 malloc 申请、宿主用 free 归还。
        std::free(out);
    }
    return outcome;
}

}  // namespace ling
