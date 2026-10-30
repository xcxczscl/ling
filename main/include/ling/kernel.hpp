// 内核骨架：装载表 + 路由表 + 装载/卸载。S1 只做进程内轨道，不启用依赖与热更新。
// 语义依据 docs/ling/04-详细设计/01-内核四服务.md 与 02-插件装载与接口.md。
#pragma once

#include <cstdint>
#include <filesystem>
#include <map>
#include <memory>
#include <string>
#include <vector>

#include "ling/manifest.hpp"
#include "ling/plugin_api.h"
#include "ling/router.hpp"

namespace ling {

enum class PluginState { kInstalled, kActivated, kFailed, kUninstalled };

const char* state_name(PluginState state);

struct LoadedPlugin {
    Manifest manifest;
    void* module = nullptr;        // HMODULE / dlopen 句柄
    void* handle = nullptr;        // out_plugin，插件自己的不透明指针
    PluginState state = PluginState::kInstalled;
    std::uint64_t generation = 0;  // 每次装载递增，用于旧回调拦截
    std::string last_error;
};

// 插件上下文：宿主交给插件的那块数据，所有回调都从它反查归属。
struct PluginContext {
    class Kernel* kernel = nullptr;
    std::string plugin_id;
    std::uint64_t generation = 0;
    std::string data_dir;  // 必须长期存活，data_dir() 返回它的 c_str()
};

class Kernel {
public:
    explicit Kernel(std::filesystem::path data_root);
    ~Kernel();

    Kernel(const Kernel&) = delete;
    Kernel& operator=(const Kernel&) = delete;

    const std::filesystem::path& data_root() const { return data_root_; }
    std::filesystem::path plugins_dir() const { return data_root_ / "plugins"; }

    // 安装：校验来源目录的清单，整目录复制到 plugins/<id>/。不建符号链接。
    std::string install(const std::filesystem::path& source_dir);

    // 卸载：注销全部方法、卸载动态库、删除安装工件，保留 data/ 除非 purge。
    std::string uninstall(const std::string& plugin_id, bool purge);

    // 扫描 plugins/ 下所有已安装插件并激活。返回失败数量。
    std::size_t load_all();

    const std::vector<LoadedPlugin>& plugins() const { return plugins_; }
    const Router& router() const { return router_; }

    CallOutcome call(const std::string& method, const std::uint8_t* args, std::size_t len);

    // 为插件分配上下文，宿主函数表 context_alloc 的实现。
    void* create_context(const std::string& plugin_id, std::uint64_t generation);

    // 宿主函数表的实现入口（由 kernel.cpp 里的自由函数调用）。
    std::string on_register(PluginContext* ctx, const std::string& name, void* fn,
                            std::uint32_t requires_mask);
    std::string on_unregister(PluginContext* ctx, const std::string& name);

private:
    LoadedPlugin* find(const std::string& plugin_id);
    std::string activate(LoadedPlugin& plugin);
    void deactivate(LoadedPlugin& plugin);
    ling_host_api make_host_api();

    std::filesystem::path data_root_;
    Router router_;
    std::vector<LoadedPlugin> plugins_;
    std::map<std::string, std::unique_ptr<PluginContext>> contexts_;
    std::uint64_t next_generation_ = 1;
    ling_host_api host_api_{};
};

// 供 plugin_host.cpp 使用的内部函数。
std::uint32_t capability_mask_of(const Manifest& manifest);
std::string host_version_string();

}  // namespace ling
