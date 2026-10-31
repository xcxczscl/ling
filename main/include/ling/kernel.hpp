// 内核骨架：装载表 + 路由表 + 装载/卸载。S1 只做进程内轨道，不启用依赖与热更新。
// 语义依据 docs/ling/04-详细设计/01-内核四服务.md 与 02-插件装载与接口.md。
#pragma once

#include <chrono>
#include <cstdint>
#include <filesystem>
#include <map>
#include <set>
#include <memory>
#include <string>
#include <vector>

#include "ling/event.hpp"
#include "ling/depgraph.hpp"
#include "ling/sidecar_host.hpp"
#include "ling/ledger.hpp"
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
    Ledger ledger;                 // 本插件本代的效应，停用时倒序撤销

    // 侧车轨道：进程句柄、口令、以及宿主代它登记的东西。
    // 代登记的意思是「这个方法属于 X」，卸载时由宿主代为注销（04/02 §8.2）。
    bool sidecar = false;
    std::unique_ptr<Sidecar> process;
    std::string sidecar_token;
    std::vector<std::string> sidecar_methods;
    std::vector<std::uint64_t> sidecar_subs;
    std::uint32_t restart_attempts = 0;
    bool restart_pending = false;
    std::chrono::steady_clock::time_point restart_at{};
};

// 插件上下文：宿主交给插件的那块数据，所有回调都从它反查归属。
struct PluginContext {
    class Kernel* kernel = nullptr;
    std::string plugin_id;
    std::uint64_t generation = 0;
    std::string data_dir;  // 必须长期存活，data_dir() 返回它的 c_str()
};

// 侧车方法的代理上下文：路由表里存一个指针，调用时按 plugin_id + method 发 invoke。
struct SidecarMethodCtx {
    class Kernel* kernel = nullptr;
    std::string plugin_id;
    std::string method;
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

    // 卸载：倒序撤销账本、卸载动态库、删除安装工件，保留 data/ 除非 purge。
    // 撤销有失败时返回 E_UNINSTALL_INCOMPLETE，且不删除任何文件（留下残件让人处理）。
    std::string uninstall(const std::string& plugin_id, bool purge);

    // 启用/停用。停用的插件记进 registry.json 的 disabled 列表，下次启动不装载。
    std::string set_enabled(const std::string& plugin_id, bool enabled);
    bool is_disabled(const std::string& plugin_id) const;

    // 扫描 plugins/ 下所有已安装插件并激活。返回失败数量。
    std::size_t load_all();

    // 装载并激活一个已安装插件（管理端口的 install 用）。成功返回空。
    std::string load_one(const std::string& plugin_id);

    // 关掉一个已激活插件但保留安装工件（disable 用）。成功返回空。
    std::string unload_one(const std::string& plugin_id);

    // kernel.* 方法的分发入口。返回错误码为空表示成功，result 填 JSON 文本。
    struct KernelReply {
        std::string code;
        std::string message;
        std::string result;
        bool shutdown = false;
    };
    KernelReply handle_kernel(const std::string& method, const std::string& args_json);

    bool shutdown_requested() const { return shutdown_requested_; }

    // 管理端口启动后写进 registry.json，供命令行找端口。
    void note_admin_port(std::uint16_t port);

    // 主循环每轮调一次：看插件目录有没有变化（改清单、换产物），
    // 有就等安静 200 毫秒后重算一次。合并窗口见 04/04 §7.2。
    void watch_tick();

    // 通过侧车调用一个方法。宿主侧调用方阻塞等 result；超时按 30 秒。
    // 只给代理函数与内核内部用，不对外暴露成 kernel.* 方法。
    CallOutcome invoke_sidecar(const std::string& plugin_id, const std::string& method,
                               const std::string& args_json);
    // 把事件转发给订阅了它的侧车。返回 0 表示已投递。
    int32_t deliver_to_sidecar(const std::string& plugin_id, const std::string& topic,
                               const std::string& payload);

    // 算一遍「接下来会做什么」，但一步也不执行。给命令行与界面预览用。
    // 返回 JSON：{ok, steps:[{index,action,plugin}], blocked:[{plugin,reason}]}。
    std::string plan_json();

    // 按依赖图调和一次：把需求不满足的插件停用，把需求已满足的插件激活。
    // 每次安装、卸载、启用、停用之后都调一次；空闲时不做定时重算（04/04 §7）。
    // 返回空表示成功，否则是错误说明（成环等）。
    std::string reconcile();

    // 清掉 registry.json 里上一次留下的界面端口。
    // 界面被强杀时来不及自己清理，宿主要在下一次启动时把陈旧值抹掉，
    // 否则「界面地址」指向一个已经没人监听的端口。
    void clear_stale_ui_port();

    // 广播 kernel.tick。宿主主循环每轮调一次，供需要按时间做事的插件使用。
    // 为什么用它而不是让插件起线程：见 docs/ling/04-详细设计/01-内核四服务.md §6.4。
    void publish_tick();

    // 预装内置插件：从 <程序目录>/plugins/<编号> 复制到数据根，已装或用户主动卸过的跳过。
    // 返回实际安装的个数。
    std::size_t install_builtins(const std::filesystem::path& program_dir);

    const std::vector<LoadedPlugin>& plugins() const { return plugins_; }
    const Router& router() const { return router_; }
    const EventBus& events() const { return events_; }

    CallOutcome call(const std::string& method, const std::uint8_t* args, std::size_t len);

    // 插件发起的广播：主题必须以本插件编号加点开头，且不得占用 kernel. 前缀。
    EmitStats emit_from(const std::string& plugin_id, const std::string& topic,
                        const std::uint8_t* payload, std::size_t len);

    // 为插件分配上下文，宿主函数表 context_alloc 的实现。
    void* create_context(const std::string& plugin_id, std::uint64_t generation);

    // 宿主函数表的实现入口（由 kernel.cpp 里的自由函数调用）。
    std::string on_register(PluginContext* ctx, const std::string& name, void* fn,
                            std::uint32_t requires_mask);
    std::string on_unregister(PluginContext* ctx, const std::string& name);
    std::string on_subscribe(PluginContext* ctx, const std::string& pattern, void* fn,
                             std::uint64_t* out_sub_id);
    std::string on_unsubscribe(PluginContext* ctx, std::uint64_t sub_id);

    // 配置写入：先记旧值入账，再转给 core-settings 落盘（04/03 §6.2、04/07 §4.3）。
    std::string write_config(const std::string& plugin_id, const std::string& key,
                             const std::uint8_t* value, std::size_t len);

private:
    // 侧车：起进程并完成 hello 握手，然后发 activate。
    std::string activate_sidecar(LoadedPlugin& plugin);
    // 处理一帧来自侧车的消息。返回 false 表示这一帧让连接作废。
    bool handle_sidecar_frame(LoadedPlugin& plugin, const sidecar::Frame& frame);
    // 收帧：非阻塞轮询所有侧车。主循环每轮调一次。
    void pump_sidecars();
    // 侧车退出后的清场：注销它登记的方法与订阅，按 restart 决定要不要再来一次。
    void sidecar_lost(LoadedPlugin& plugin, const std::string& reason);

    // 当前状态的快照（装了哪些、哪些在跑、哪些用不了）与依赖图。
    DepGraph current_graph(std::map<std::string, bool>* active_out);

    LoadedPlugin* find(const std::string& plugin_id);
    std::string activate(LoadedPlugin& plugin);
    void deactivate(LoadedPlugin& plugin);
    ling_host_api make_host_api();

    // 广播 kernel.plugin.state。state 取对外五个取值之一。
    void publish_state(const LoadedPlugin& plugin, const char* state,
                       const std::string& reason = {});
    // 内核自身发出的广播，跳过插件前缀校验。
    EmitStats emit_kernel(const std::string& topic, const std::string& payload);
    // 把一次配置写入交给 core-settings。value_json 是 JSON 文本，"null" 表示删键。
    std::string apply_config(const std::string& plugin_id, const std::string& key,
                             const std::string& value_json);

    void load_registry();
    void save_registry() const;

    std::filesystem::path data_root_;
    std::vector<std::string> disabled_;
    // 用户主动卸掉的内置插件，升级时不再自动装回（04/07 §8.3）。
    std::vector<std::string> removed_builtins_;
    Router router_;
    EventBus events_;
    std::vector<LoadedPlugin> plugins_;
    // 依赖不满足导致停用的原因。停用的插件不在装载表里，原因得单独记。
    std::map<std::string, std::string> blocked_;
    // 调和计划编号，单调递增。每一步广播 kernel.plan.step 时带上它。
    std::uint64_t next_plan_id_ = 1;
    // 目录变化监视：插件清单的最后修改时间，用来判断要不要重算。
    std::map<std::string, std::uint64_t> watch_stamps_;
    bool watch_dirty_ = false;
    std::chrono::steady_clock::time_point watch_last_change_{};

    // 侧车方法的代理上下文。key 是「插件名/方法名」，值指过去，路由表存它的地址。
    std::map<std::string, std::unique_ptr<SidecarMethodCtx>> sidecar_method_ctx_;
    // 侧车事件订阅的上下文，key 是插件名。
    std::map<std::string, std::unique_ptr<SidecarMethodCtx>> sidecar_sub_ctx_;
    std::uint32_t next_sidecar_call_id_ = 1;

    // 找侧车实例；不在或没在跑返回 nullptr。
    LoadedPlugin* find_sidecar(const std::string& plugin_id);

    // 正在重载的插件。重载期间把它当成「不可用」，依赖它的先停下来，
    // 换完再回来——否则会出现「新版本还没就位，调用方已经在调」。
    std::set<std::string> reloading_;
    std::map<std::string, std::unique_ptr<PluginContext>> contexts_;
    std::uint64_t next_generation_ = 1;
    std::uint16_t admin_port_ = 0;
    bool shutdown_requested_ = false;
    ling_host_api host_api_{};
};

// 供 plugin_host.cpp 使用的内部函数。
std::uint32_t capability_mask_of(const Manifest& manifest);
std::string host_version_string();

}  // namespace ling
