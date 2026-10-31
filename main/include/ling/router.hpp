// 方法路由表。规则依据 docs/ling/04-详细设计/01-内核四服务.md §5.1 至 §5.3。
#pragma once

#include <cstdint>
#include <map>
#include <string>
#include <vector>

#include "ling/errors.hpp"

namespace ling {

struct RouteEntry {
    std::string plugin_id;
    void* handler = nullptr;    // ling_method_fn
    void* handler_ctx = nullptr;
    std::uint32_t requires_mask = 0;
};

struct CallOutcome {
    std::string code;            // 空表示成功
    std::vector<std::uint8_t> data;
    // 补充说明，通常是「为什么没跑起来」。只在给人看的路径上用，
    // 插件之间的调用只认 code。
    std::string detail;
    bool ok() const { return code.empty(); }
};

class Router {
public:
    // 第 2、3、4、6 条校验；第 1 条（插件状态）与第 5 条（能力子集）由调用方先做。
    std::string register_method(const std::string& plugin_id, const std::string& name, void* fn,
                                void* handler_ctx, std::uint32_t requires_mask);

    std::string unregister_method(const std::string& plugin_id, const std::string& name);

    // 批量注销某插件的全部方法，返回被注销的方法名（按字典序），供账本倒序撤销。
    std::vector<std::string> unregister_all(const std::string& plugin_id);

    CallOutcome call(const std::string& method, const std::uint8_t* args, std::size_t len) const;

    bool contains(const std::string& method) const;
    std::size_t size() const { return routes_.size(); }

private:
    std::map<std::string, RouteEntry> routes_;
};

// 纯函数：方法名合法性（第 4 条）。
bool valid_method_name(const std::string& name);

}  // namespace ling
