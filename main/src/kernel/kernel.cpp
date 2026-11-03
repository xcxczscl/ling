#include "ling/kernel.hpp"

#include <algorithm>
#include <cctype>
#include <chrono>
#include <cstdlib>
#include <ctime>
#include <fstream>
#include <cstring>
#include <string>
#include <vector>
#include <memory>
#include <random>
#include <system_error>
#include <utility>

#include "ling/admin.hpp"
#include "ling/depgraph.hpp"
#include "ling/errors.hpp"
#include "ling/gate.hpp"
#include "ling/log.hpp"
#include "ling/sys.hpp"
#include "ling/version.hpp"

#include <nlohmann/json.hpp>

namespace ling {
namespace {

Kernel* g_kernel = nullptr;  // 单进程单宿主，见 03 §5.1 的进程模型

// 界面进程的起始端口。真正绑上哪个由界面自己定（占用就顺延）并写回 registry.json
// 的 ui_port，宿主只给一个起点（docs/ling/builtin-plugins/core-ui/04-详细设计/05 §4.1）。
constexpr std::uint16_t kUiStartPort = 8010;

// 目标路径必须严格落在 root 之内。按路径分量比较，避免 "/plugins-x" 被当成
// "/plugins" 的前缀；恰好等于 root 本身也不算「之内」。
bool path_within(const std::filesystem::path& root, const std::filesystem::path& target) {
    auto root_it = root.begin();
    auto target_it = target.begin();
    for (; root_it != root.end(); ++root_it, ++target_it) {
        if (target_it == target.end() || *root_it != *target_it) {
            return false;
        }
    }
    return target_it != target.end();
}

// Windows 命令行参数加引号：外层包引号，参数里的双引号转义，末尾连续的
// 反斜杠翻倍。带空格或中文的路径靠它才能原样传到子进程。
std::string quote_command_arg(const std::string& text) {
    std::string out = "\"";
    std::size_t backslashes = 0;
    for (const char ch : text) {
        if (ch == '\\') {
            ++backslashes;
            continue;
        }
        if (ch == '"') {
            out.append(backslashes * 2 + 1, '\\');
        } else {
            out.append(backslashes, '\\');
        }
        backslashes = 0;
        out.push_back(ch);
    }
    out.append(backslashes * 2, '\\');
    out.push_back('"');
    return out;
}

// 常驻配额（plugin-support/01 §8.7）：Python 侧车最多 1 个声明 restart="always"
// 的常驻实例，第 2 个装载被拒，message 说明当前常驻的是哪个插件。
std::string resident_quota_error(const std::vector<LoadedPlugin>& plugins,
                                 const Manifest& manifest, const std::string& exclude_id) {
    if (!manifest.sidecar.present || manifest.sidecar.restart != "always" ||
        manifest.sidecar.runtime != "python") {
        return {};
    }
    for (const auto& existing : plugins) {
        if (existing.manifest.id == exclude_id || existing.state != PluginState::kActivated) {
            continue;
        }
        if (existing.manifest.sidecar.present && existing.manifest.sidecar.restart == "always" &&
            existing.manifest.sidecar.runtime == "python") {
            return std::string(err::kResidentLimit) + " 常驻 Python 侧车已有 " +
                   existing.manifest.id;
        }
    }
    return {};
}

// ---------- 宿主函数表实现 ----------

void* host_context_alloc(const char* plugin_id, std::uint64_t generation) {
    if (g_kernel == nullptr || plugin_id == nullptr) {
        return nullptr;
    }
    return g_kernel->create_context(plugin_id, generation);
}

int32_t host_register_method(void* ctx, const char* name, void* fn, void* undo_fn,
                             uint32_t requires_mask) {
    auto* context = static_cast<PluginContext*>(ctx);
    if (context == nullptr || name == nullptr) {
        return 1;
    }
    (void)undo_fn;  // S2 起进账本
    const std::string code = context->kernel->on_register(context, name, fn, requires_mask);
    return code.empty() ? 0 : 2;
}

int32_t host_unregister_method(void* ctx, const char* name) {
    auto* context = static_cast<PluginContext*>(ctx);
    if (context == nullptr || name == nullptr) {
        return 1;
    }
    const std::string code = context->kernel->on_unregister(context, name);
    return code.empty() ? 0 : 2;
}

int32_t host_subscribe(void* ctx, const char* pattern, void* fn, void* undo_fn,
                       uint64_t* out_sub_id) {
    auto* context = static_cast<PluginContext*>(ctx);
    if (context == nullptr || pattern == nullptr) {
        return 1;
    }
    (void)undo_fn;  // 撤销动作由宿主账本按 sub_id 生成，插件不必自带
    const std::string code =
        context->kernel->on_subscribe(context, pattern, fn, out_sub_id);
    return code.empty() ? 0 : 2;
}

int32_t host_unsubscribe(void* ctx, uint64_t sub_id) {
    auto* context = static_cast<PluginContext*>(ctx);
    if (context == nullptr) {
        return 1;
    }
    const std::string code = context->kernel->on_unsubscribe(context, sub_id);
    return code.empty() ? 0 : 2;
}

int32_t host_call(void* ctx, const char* method, const uint8_t* args, size_t args_len,
                  uint8_t** out, size_t* out_len) {
    auto* context = static_cast<PluginContext*>(ctx);
    if (context == nullptr || method == nullptr) {
        return 1;
    }

    std::string rewritten_args;
    if (rewrite_caller_plugin(method, context->plugin_id, args, args_len, &rewritten_args)) {
        args = reinterpret_cast<const uint8_t*>(rewritten_args.data());
        args_len = rewritten_args.size();
    }

    const CallOutcome outcome = context->kernel->call(method, args, args_len);
    if (!outcome.ok()) {
        return 2;
    }
    if (out != nullptr && out_len != nullptr) {
        if (outcome.data.empty()) {
            *out = nullptr;
            *out_len = 0;
        } else {
            auto* buffer = static_cast<uint8_t*>(std::malloc(outcome.data.size()));
            if (buffer == nullptr) {
                return 4;
            }
            std::memcpy(buffer, outcome.data.data(), outcome.data.size());
            *out = buffer;
            *out_len = outcome.data.size();
        }
    }
    return 0;
}

int32_t host_emit(void* ctx, const char* topic, const uint8_t* payload, size_t len) {
    auto* context = static_cast<PluginContext*>(ctx);
    if (context == nullptr || topic == nullptr) {
        return 1;
    }
    const EmitStats stats = context->kernel->emit_from(context->plugin_id, topic, payload, len);
    // 越权、超限这类「一条都没发出去」的返回错误码；个别订阅者失败不影响返回值。
    return stats.code.empty() ? 0 : 2;
}

int32_t host_write_config(void* ctx, const char* key, const uint8_t* value, size_t len) {
    auto* context = static_cast<PluginContext*>(ctx);
    if (context == nullptr || key == nullptr) {
        return 1;
    }
    const std::string code = context->kernel->write_config(context->plugin_id, key, value, len);
    return code.empty() ? 0 : 2;
}

const char* host_data_dir(void* ctx) {
    auto* context = static_cast<PluginContext*>(ctx);
    if (context == nullptr) {
        return nullptr;
    }
    return context->data_dir.c_str();
}

void host_free_buffer(void* ctx, uint8_t* p, size_t len) {
    (void)ctx;
    (void)len;
    std::free(p);
}

}  // namespace

// 身份敏感方法把 plugin 字段拼进命名空间或文件名，不能信调用方自报。
// 进程内插件调用没有独立的调用者字段，只能由宿主用真实上下文编号覆盖。
// 管理端口走的是 handle_kernel，不经过这里，仍保留显式指定 plugin 的管理能力。
bool rewrite_caller_plugin(const char* method, const std::string& caller, const uint8_t* args,
                           size_t args_len, std::string* rewritten) {
    const bool identity_sensitive = std::strcmp(method, "core-settings.write") == 0 ||
                                    std::strcmp(method, "core-log.write") == 0;
    if (!identity_sensitive || args == nullptr || args_len == 0) {
        return false;
    }

    nlohmann::json parsed;
    try {
        parsed = nlohmann::json::parse(args, args + args_len, nullptr, false);
    } catch (...) {
        log_warn("identity", std::string("插件 ") + caller + " 调用 " + method +
                                " 的参数解析失败，未做身份改写");
        return false;
    }
    if (parsed.is_discarded() || !parsed.is_object()) {
        log_warn("identity", std::string("插件 ") + caller + " 调用 " + method +
                                " 的参数不是 JSON 对象，未做身份改写");
        return false;
    }

    std::string reported;
    if (parsed.contains("plugin")) {
        reported = parsed["plugin"].is_string() ? parsed["plugin"].get<std::string>()
                                                : std::string("(非字符串)");
    } else {
        reported = "(未填)";
    }
    if (reported == caller) {
        return false;
    }

    // 自报值会进内核日志，先清掉换行并截断，避免一条日志被拆成多行或刷屏。
    std::string shown = reported;
    for (char& ch : shown) {
        if (std::iscntrl(static_cast<unsigned char>(ch)) != 0) {
            ch = ' ';
        }
    }
    if (shown.size() > 64) {
        shown = shown.substr(0, 64) + "...";
    }

    try {
        parsed["plugin"] = caller;
        *rewritten = parsed.dump();
    } catch (...) {
        log_warn("identity", std::string("插件 ") + caller + " 调用 " + method +
                                " 的参数序列化失败，未做身份改写");
        return false;
    }
    log_warn("identity", std::string("插件 ") + caller + " 调用 " + method + " 时自报 plugin=" +
                            shown + "，已改为真实编号");
    return true;
}

const char* state_name(PluginState state) {
    switch (state) {
        case PluginState::kInstalled:
            return "installed";
        case PluginState::kActivated:
            return "activated";
        case PluginState::kFailed:
            return "failed";
    }
    return "installed";
}

std::string host_version_string() { return version_string(); }

ling_host_api Kernel::make_host_api() {
    ling_host_api api{};
    api.struct_size = sizeof(ling_host_api);
    api.context_alloc = &host_context_alloc;
    api.register_method = &host_register_method;
    api.unregister_method = &host_unregister_method;
    api.subscribe = &host_subscribe;
    api.unsubscribe = &host_unsubscribe;
    api.call = &host_call;
    api.emit = &host_emit;
    api.write_config = &host_write_config;
    api.data_dir = &host_data_dir;
    api.free_buffer = &host_free_buffer;
    return api;
}

Kernel::Kernel(std::filesystem::path data_root) : data_root_(std::move(data_root)) {
    std::error_code ec;
    // 数据根一律落成绝对路径再往下传。命令行给的常是相对路径（run/x），而侧车的
    // 工作目录被设成插件自己的目录，相对路径传到子进程里就换了基准点
    // （docs/ling/builtin-plugins/core-ui/04-详细设计/05-进程形态与启动.md §3）。
    const auto absolute_root = std::filesystem::absolute(data_root_, ec);
    if (!ec) {
        const auto canonical_root = std::filesystem::weakly_canonical(absolute_root, ec);
        data_root_ = ec ? absolute_root.lexically_normal() : canonical_root;
        ec.clear();
    }
    std::filesystem::create_directories(plugins_dir(), ec);
    log_init(data_root_ / "logs");
    load_registry();
    host_api_ = make_host_api();
    g_kernel = this;
    log_info("kernel", "数据根 " + data_root_.string());
}

Kernel::~Kernel() {
    for (auto& plugin : plugins_) {
        if (plugin.state == PluginState::kActivated) {
            deactivate(plugin);
        }
    }
    g_kernel = nullptr;
}

LoadedPlugin* Kernel::find(const std::string& plugin_id) {
    for (auto& plugin : plugins_) {
        if (plugin.manifest.id == plugin_id) {
            return &plugin;
        }
    }
    return nullptr;
}

void Kernel::load_registry() {
    const auto path = data_root_ / "registry.json";
    std::ifstream input(path);
    if (!input) {
        return;
    }
    try {
        nlohmann::json document;
        input >> document;
        if (document.contains("disabled") && document["disabled"].is_array()) {
            for (const auto& item : document["disabled"]) {
                if (item.is_string()) {
                    disabled_.push_back(item.get<std::string>());
                }
            }
        }
        if (document.contains("removed_builtins") && document["removed_builtins"].is_array()) {
            for (const auto& item : document["removed_builtins"]) {
                if (item.is_string()) {
                    removed_builtins_.push_back(item.get<std::string>());
                }
            }
        }
    } catch (const std::exception& error) {
        // 配置坏了不阻塞启动：记一条 warn，按「没有停用记录」继续。
        log_warn("registry", std::string("registry.json 解析失败，按空配置继续：") + error.what());
    }
}

void Kernel::save_registry() const {
    nlohmann::json document;
    document["schema_version"] = 1;
    document["disabled"] = disabled_;
    document["removed_builtins"] = removed_builtins_;
    if (admin_port_ != 0) {
        document["port"] = admin_port_;
    }

    const auto path = data_root_ / "registry.json";
    // ui_port 由界面进程写进来，内核只转发不持有。重写时先读回旧值，否则每次
    // 插件状态变化都会把这个字段抹掉，界面地址就找不到了。
    {
        std::ifstream existing(path);
        if (existing) {
            const auto previous = nlohmann::json::parse(existing, nullptr, false);
            if (!previous.is_discarded() && previous.contains("ui_port")) {
                document["ui_port"] = previous["ui_port"];
            }
        }
    }

    std::ofstream output(path, std::ios::trunc);
    if (!output) {
        log_error("registry", "无法写入 " + path.string());
        return;
    }
    output << document.dump(2) << "\n";
}

void Kernel::clear_stale_ui_port() {
    const auto path = data_root_ / "registry.json";
    std::ifstream input(path);
    if (!input) {
        return;
    }
    auto document = nlohmann::json::parse(input, nullptr, false);
    input.close();
    if (document.is_discarded() || !document.is_object() || !document.contains("ui_port")) {
        return;
    }
    document.erase("ui_port");
    std::ofstream output(path, std::ios::trunc);
    if (output) {
        output << document.dump(2) << "\n";
    }
}

void Kernel::note_admin_port(std::uint16_t port) {
    admin_port_ = port;
    save_registry();
}

bool Kernel::is_disabled(const std::string& plugin_id) const {
    return std::find(disabled_.begin(), disabled_.end(), plugin_id) != disabled_.end();
}

std::string Kernel::set_enabled(const std::string& plugin_id, bool enabled) {
    const auto dir = plugins_dir() / plugin_id;
    if (!std::filesystem::exists(dir / "plugin.toml")) {
        return std::string(err::kPluginMissing) + " " + plugin_id;
    }

    const auto it = std::find(disabled_.begin(), disabled_.end(), plugin_id);
    if (enabled) {
        if (it != disabled_.end()) {
            disabled_.erase(it);
        }
    } else if (it == disabled_.end()) {
        disabled_.push_back(plugin_id);
    }
    save_registry();
    log_info("registry", plugin_id + (enabled ? " 已启用" : " 已停用"));
    return {};
}

std::string Kernel::install(const std::filesystem::path& source_dir) {
    const ManifestResult parsed = load_manifest(source_dir, host_version_string());
    if (!parsed.ok()) {
        log_error("loader", "安装失败 " + source_dir.string() + "：" + parsed.code + " " +
                                parsed.detail);
        return parsed.code + " " + parsed.detail;
    }

    const auto target = plugins_dir() / parsed.manifest->id;
    std::error_code ec;
    // 常驻配额是「装载」的校验（清单第 12 步），但装包时就拒掉更符合预期：
    // 否则装进去以后永远启不来，用户还得自己排查。
    const std::string quota =
        resident_quota_error(plugins_, *parsed.manifest, parsed.manifest->id);
    if (!quota.empty()) {
        log_warn("loader", "拒绝安装 " + parsed.manifest->id + "：" + quota);
        return quota;
    }
    // 判定口径见 04/02 §2.1：plugin.toml 存在才算已安装。
    // 只剩 data/ 的目录算未安装，可以重装并沿用里面的数据。
    if (std::filesystem::exists(target / "plugin.toml", ec)) {
        return std::string(err::kPluginExists) + " 已存在 " + target.string() + "，先卸载再装";
    }
    const bool target_existed = std::filesystem::exists(target, ec);

    // 安装途中有任何一步失败都要把半成品目录删掉，否则只留一半文件的目录会被
    // 「plugin.toml 存在即已安装」的口径当成装好了，重装又会撞 E_PLUGIN_EXISTS。
    // 但重装前就存在的 data/ 是用户的旧数据，回滚时不能一起删。
    auto rollback = [&target, target_existed](const std::string& code, const std::string& detail) {
        std::error_code cleanup_ec;
        if (!target_existed) {
            std::filesystem::remove_all(target, cleanup_ec);
        } else {
            std::filesystem::directory_iterator iterator(target, cleanup_ec);
            while (!cleanup_ec && iterator != std::filesystem::directory_iterator()) {
                if (iterator->path().filename() != "data") {
                    std::filesystem::remove_all(iterator->path(), cleanup_ec);
                }
                iterator.increment(cleanup_ec);
            }
        }
        if (cleanup_ec) {
            log_warn("loader", "安装回滚失败，残留目录 " + target.string() + "：" +
                                   cleanup_ec.message());
        }
        return code + " " + detail;
    };

    std::filesystem::create_directories(target, ec);
    if (ec) {
        return rollback(err::kIo, "无法创建目录 " + target.string() + "：" + ec.message());
    }
    // 逐个拷贝顶层条目，跳过 data/，避免把旧数据覆盖掉。
    std::filesystem::directory_iterator iterator(source_dir, ec);
    if (ec) {
        return rollback(err::kIo, "无法读取源目录 " + source_dir.string() + "：" + ec.message());
    }
    for (const auto& entry : iterator) {
        if (entry.path().filename() == "data") {
            continue;
        }
        std::filesystem::copy(entry.path(), target / entry.path().filename(),
                              std::filesystem::copy_options::recursive |
                                  std::filesystem::copy_options::overwrite_existing,
                              ec);
        if (ec) {
            return rollback(err::kIo, "复制失败：" + ec.message());
        }
    }

    std::filesystem::create_directories(target / "data", ec);
    if (ec) {
        return rollback(err::kIo, "无法创建数据目录：" + ec.message());
    }
    log_info("loader", "已安装 " + parsed.manifest->id + " " + parsed.manifest->version);
    return {};
}

std::string Kernel::uninstall(const std::string& plugin_id, bool purge) {
    // 编号会直接拼进路径并交给 remove_all：先按清单同一套规则校验，再确认目标
    // 确实落在 plugins/ 之内，两层都过了才动手。
    if (!valid_plugin_id(plugin_id)) {
        return std::string(err::kPluginMissing) + " 编号不合法：" + plugin_id;
    }
    std::error_code path_ec;
    const auto plugins_root = std::filesystem::weakly_canonical(plugins_dir(), path_ec);
    const auto dir = std::filesystem::weakly_canonical(plugins_dir() / plugin_id, path_ec);
    if (path_ec) {
        return std::string(err::kIo) + " 无法解析插件目录：" + path_ec.message();
    }
    if (!path_within(plugins_root, dir)) {
        return std::string(err::kPluginMissing) + " 编号不合法：" + plugin_id;
    }

    // 先把目录看一遍再动内存：目录不存在或读不了时直接返回，别出现「内存里已卸载、
    // 磁盘却没清」这种回不去的中间状态。迭代器也先建好，删除期间继续用。
    std::error_code ec;
    const bool dir_exists = std::filesystem::exists(dir, ec);
    if (ec) {
        return std::string(err::kIo) + " 无法检查 " + dir.string() + "：" + ec.message();
    }
    if (!dir_exists) {
        return std::string(err::kPluginMissing) + " " + plugin_id;
    }
    std::filesystem::directory_iterator iterator(dir, ec);
    if (ec) {
        return std::string(err::kIo) + " 无法读取 " + dir.string() + "：" + ec.message();
    }

    LoadedPlugin* plugin = find(plugin_id);
    if (plugin != nullptr) {
        const bool was_activated = plugin->state == PluginState::kActivated;
        if (was_activated) {
            const UndoReport report = plugin->ledger.undo_all();
            if (report.failed > 0) {
                // 撤销失败就不动文件：留下现场比删干净更有用。
                std::string detail = std::string(err::kUninstallIncomplete) + " 有 " +
                                     std::to_string(report.failed) + " 条效应撤销失败：";
                for (const auto& item : report.failed_items) {
                    // 逐条标出失败类型，方便调用方按码分类；整体仍返回 E_UNINSTALL_INCOMPLETE。
                    detail += std::string(err::kUndoFailed) + " " + item + " ";
                }
                log_error("loader", plugin_id + " " + detail);
                return detail;
            }
            // 侧车插件在装完/卸载时也要把自己的进程和宿主上下文收掉，不能只
            // 卸动态库：否则进程还在跑，上下文表里也留着旧指针。
            if (plugin->sidecar) {
                if (plugin->process != nullptr) {
                    plugin->process->terminate(2000);
                    plugin->process.reset();
                }
                plugin->sidecar = false;
                plugin->sidecar_methods.clear();
                plugin->sidecar_subs.clear();
                const std::string prefix = plugin_id + "/";
                for (auto it = sidecar_method_ctx_.begin(); it != sidecar_method_ctx_.end();) {
                    if (it->first.compare(0, prefix.size(), prefix) == 0) {
                        it = sidecar_method_ctx_.erase(it);
                    } else {
                        ++it;
                    }
                }
                sidecar_sub_ctx_.erase(plugin_id);
            }
            const auto removed = router_.unregister_all(plugin_id);
            if (!removed.empty()) {
                log_warn("loader", plugin_id + " 账本外残留方法 " +
                                       std::to_string(removed.size()) + " 个，已强制清空");
            }
            const auto patterns = events_.unsubscribe_all(plugin_id);
            if (!patterns.empty()) {
                log_warn("loader", plugin_id + " 账本外残留订阅 " +
                                       std::to_string(patterns.size()) + " 条，已强制清空");
            }
            if (plugin->module != nullptr) {
                sys::unload_library(plugin->module);
                plugin->module = nullptr;
            }
            plugin->handle = nullptr;
            plugin->state = PluginState::kInstalled;
            // 卸载只对外报一条 uninstalled：状态迁移不拆成 deactivated 加 uninstalled。
            publish_state(*plugin, "uninstalled");
        }
        plugins_.erase(plugins_.begin() + (plugin - plugins_.data()));
        contexts_.erase(plugin_id);
    }

    while (iterator != std::filesystem::directory_iterator()) {
        const auto name = iterator->path().filename().string();
        if (!(name == "data" && !purge)) {
            std::error_code remove_ec;
            std::filesystem::remove_all(iterator->path(), remove_ec);
            if (remove_ec) {
                return std::string(err::kIo) + " 删除失败 " + iterator->path().string() + "：" +
                       remove_ec.message();
            }
        }
        iterator.increment(ec);
        if (ec) {
            return std::string(err::kIo) + " 遍历失败 " + dir.string() + "：" + ec.message();
        }
    }
    // --purge 连目录本身一起删掉；不带 purge 时目录要留着装 data/。
    if (purge) {
        std::error_code remove_ec;
        std::filesystem::remove_all(dir, remove_ec);
        if (remove_ec) {
            return std::string(err::kIo) + " 删除失败 " + dir.string() + "：" + remove_ec.message();
        }
    }

    log_info("loader", std::string("已卸载 ") + plugin_id + (purge ? "（含数据）" : "（保留数据）"));

    // 用户主动卸掉的内置插件不再自动装回（04/07 §8.3 第三条）。
    if (plugin_id.rfind("core-", 0) == 0 &&
        std::find(removed_builtins_.begin(), removed_builtins_.end(), plugin_id) ==
            removed_builtins_.end()) {
        removed_builtins_.push_back(plugin_id);
        save_registry();
    }
    return {};
}

std::size_t Kernel::install_builtins(const std::filesystem::path& program_dir) {
    program_dir_ = program_dir;
    static const char* const kBuiltins[] = {"core-settings", "core-plugin-manager", "core-log",
                                            "core-job", "core-ui"};
    std::size_t installed = 0;
    for (const char* id : kBuiltins) {
        std::error_code ec;
        if (std::filesystem::exists(plugins_dir() / id / "plugin.toml", ec)) {
            continue;  // 已安装
        }
        if (std::find(removed_builtins_.begin(), removed_builtins_.end(), id) !=
            removed_builtins_.end()) {
            log_info("builtin", std::string(id) + " 曾被用户卸载，不再自动安装");
            continue;
        }
        const auto source = program_dir / "plugins" / id;
        if (!std::filesystem::exists(source / "plugin.toml", ec)) {
            log_warn("builtin", std::string("找不到内置插件产物 ") + source.string());
            continue;
        }
        const std::string code = install(source);
        if (!code.empty()) {
            log_warn("builtin", std::string("安装 ") + id + " 失败：" + code);
            continue;
        }
        ++installed;
    }
    if (installed > 0) {
        log_info("builtin", "预装内置插件 " + std::to_string(installed) + " 个");
    }
    return installed;
}

std::size_t Kernel::load_all() {
    std::size_t failures = 0;
    std::error_code ec;
    if (!std::filesystem::exists(plugins_dir(), ec)) {
        return 0;
    }

    std::vector<std::filesystem::path> dirs;
    for (const auto& entry : std::filesystem::directory_iterator(plugins_dir(), ec)) {
        if (entry.is_directory()) {
            dirs.push_back(entry.path());
        }
    }
    std::sort(dirs.begin(), dirs.end());

    for (const auto& dir : dirs) {
        if (!std::filesystem::exists(dir / "plugin.toml")) {
            continue;  // 只剩 data/ 的目录算未安装
        }
        if (is_disabled(dir.filename().string())) {
            log_info("loader", dir.filename().string() + " 已停用，跳过装载");
            continue;
        }
        const ManifestResult parsed = load_manifest(dir, host_version_string());
        if (!parsed.ok()) {
            log_error("loader", dir.filename().string() + " 清单不合法：" + parsed.code + " " +
                                    parsed.detail);
            ++failures;
            continue;
        }

        LoadedPlugin plugin;
        plugin.manifest = *parsed.manifest;
        plugin.generation = next_generation_++;
        plugins_.push_back(std::move(plugin));

        const std::string code = activate(plugins_.back());
        if (!code.empty()) {
            plugins_.back().state = PluginState::kFailed;
            plugins_.back().last_error = code;
            log_error("loader", plugins_.back().manifest.id + " 激活失败：" + code);
            publish_state(plugins_.back(), "failed", code);
            ++failures;
        } else {
            publish_state(plugins_.back(), "activated");
        }
    }

    // 全部装载完再调和：前面装载时依赖方可能排在前面，那时被依赖方还没就位。
    reconcile();
    return failures;
}

// 从磁盘重建一次「现在的样子」：装了哪些、哪些在跑、哪些装着但用不了。
// 调和与预览都要它，所以单独拎出来——两边各写一份的话，预览说的和实际做的迟早对不上。
DepGraph Kernel::current_graph(std::map<std::string, bool>* active_out) {
    std::vector<InstalledEntry> installed;
    std::map<std::string, bool> active;
    // 装着但用不了的：被停用的、启动失败的。依赖它们的插件要一起停（04/04 §5.1）。
    std::set<std::string> unavailable(reloading_.begin(), reloading_.end());

    // 只看装载表会漏掉停用与未装载的插件，导致「先装依赖方」时算不出它其实已经装好。
    std::error_code ec;
    for (const auto& entry : std::filesystem::directory_iterator(plugins_dir(), ec)) {
        if (!entry.is_directory()) {
            continue;
        }
        const auto dir = entry.path();
        if (!std::filesystem::exists(dir / "plugin.toml", ec)) {
            continue;  // 只剩 data/ 的目录算未安装
        }
        const ManifestResult parsed = load_manifest(dir, host_version_string());
        if (!parsed.ok()) {
            log_warn("loader", dir.filename().string() + " 清单非法，不参与调和：" +
                                   parsed.detail);
            continue;
        }
        InstalledEntry item;
        item.id = parsed.manifest->id;
        item.version = parsed.manifest->version;
        item.requires = parsed.manifest->requires_plugins;
        const std::string id = item.id;
        installed.push_back(std::move(item));

        const LoadedPlugin* plugin = find(id);
        active[id] = plugin != nullptr && plugin->state == PluginState::kActivated;

        if (is_disabled(id)) {
            unavailable.insert(id);
            // 停用是用户主动做的，算「已达期望状态」：别再试着起它，也不给它记原因。
            active[id] = true;
        } else if (plugin != nullptr && plugin->state == PluginState::kFailed) {
            unavailable.insert(id);
            active[id] = true;  // 失败的重试留给人决定（install/enable），不在这里循环重试
        }
    }

    if (active_out != nullptr) {
        *active_out = active;
    }
    return build_graph(installed, unavailable);
}

void Kernel::watch_tick() {
    pump_sidecars();

    const auto now_for_restart = std::chrono::steady_clock::now();
    for (auto& plugin : plugins_) {
        if (!plugin.restart_pending || now_for_restart < plugin.restart_at) {
            continue;
        }
        plugin.restart_pending = false;
        log_info("sidecar", plugin.manifest.id + " 重启（第 " +
                               std::to_string(plugin.restart_attempts) + " 次）");
        const std::string code = activate(plugin);
        if (code.empty()) {
            plugin.state = PluginState::kActivated;
            plugin.last_error.clear();
            plugin.restart_attempts = 0;
            publish_state(plugin, "activated");
        } else {
            plugin.state = PluginState::kFailed;
            plugin.last_error = code;
            publish_state(plugin, "failed", code);
        }
    }

    std::map<std::string, std::uint64_t> current;
    std::error_code ec;
    std::filesystem::directory_iterator iterator(plugins_dir(), ec);
    if (ec) {
        // 目录暂时读不到（权限、盘符断开）时本轮跳过，不要把异常抛进主循环。
        log_warn("watch", "扫描插件目录失败：" + ec.message());
        return;
    }
    for (const auto& entry : iterator) {
        std::error_code entry_ec;
        if (!entry.is_directory(entry_ec)) {
            if (entry_ec) {
                log_warn("watch", "读取插件目录项失败：" + entry_ec.message());
            }
            continue;
        }
        // 用清单的指纹（修改时间 + 大小）判断有没有被改过。改依赖、改版本、换产物
        // 都会碰这个文件，所以一个指纹就够。取指纹走平台层：标准库的
        // last_write_time 在 MinGW 上会溢出到秒，200 毫秒内的改动看不出来。
        const std::uint64_t stamp = sys::file_stamp(entry.path() / "plugin.toml");
        if (stamp == 0) {
            continue;  // 没有清单：只剩 data/ 的目录算未安装
        }
        current[entry.path().filename().string()] = stamp;
    }

    const auto now = std::chrono::steady_clock::now();
    if (current != watch_stamps_) {
        watch_stamps_ = current;
        watch_dirty_ = true;
        watch_last_change_ = now;
        return;  // 刚变过，等安静下来再算
    }
    if (!watch_dirty_) {
        return;
    }
    if (std::chrono::duration_cast<std::chrono::milliseconds>(now - watch_last_change_) <
        std::chrono::milliseconds(200)) {
        return;  // 还在合并窗口里：连续几次改动只算一次
    }

    watch_dirty_ = false;
    log_info("loader", "插件目录有变化，重新调和");
    reconcile();
}

std::string Kernel::doctor_report() {
    // 四项检查，只报告不修复（LING-REQ-076）。每项一行结论加必要的明细，
    // 全部在核心里算，不调任何插件方法——被修的插件不能是诊断的依据。
    nlohmann::json sections = nlohmann::json::array();
    bool all_ok = true;

    // 一、依赖图有没有绕成环
    {
        const DepGraph graph = current_graph(nullptr);
        const std::vector<std::string> cycle = find_cycle(graph);
        nlohmann::json section;
        section["key"] = "deps";
        section["title"] = "依赖图";
        section["ok"] = cycle.empty();
        section["details"] = nlohmann::json::array();
        if (cycle.empty()) {
            section["summary"] = "无环";
        } else {
            all_ok = false;
            // find_cycle 返回的环路首尾相同，直接按顺序拼就行，别再补尾巴。
            std::string path;
            for (std::size_t i = 0; i < cycle.size(); ++i) {
                if (i > 0) {
                    path += " → ";
                }
                path += cycle[i];
            }
            section["summary"] = "有环：" + path;
            section["details"].push_back(path);
        }
        sections.push_back(section);
    }

    // 二、有没有撤销没撤净的条目
    {
        nlohmann::json section;
        section["key"] = "undo";
        section["title"] = "未撤销条目";
        section["details"] = nlohmann::json::array();
        if (pending_undo_.empty()) {
            section["ok"] = true;
            section["summary"] = "没有残留";
        } else {
            all_ok = false;
            std::size_t total = 0;
            for (const auto& item : pending_undo_) {
                total += item.second;
                section["details"].push_back(item.first + " 有 " + std::to_string(item.second) +
                                             " 条改动没能退回去");
            }
            section["summary"] = std::to_string(pending_undo_.size()) + " 个插件共 " +
                                 std::to_string(total) + " 条没退回去";
        }
        sections.push_back(section);
    }

    // 三、端口还在听吗，口令文件在不在、权限够不够紧
    {
        nlohmann::json section;
        section["key"] = "port";
        section["title"] = "端口与口令文件";
        section["details"] = nlohmann::json::array();
        bool ok = true;

        if (admin_port_ == 0) {
            ok = false;
            section["details"].push_back("管理端口没在监听");
        }

        const auto session = data_root_ / "session.json";
        std::error_code ec;
        if (!std::filesystem::exists(session, ec)) {
            ok = false;
            section["details"].push_back("session.json 不存在：" + session.string());
        } else {
            // 这里的分平台不是系统调用差异，而是「安全策略还没在 Windows 上落地」：
            // 属应用层策略，不属于 ling::sys 平台抽象层（那边只收口系统调用差异）。
#ifdef _WIN32
            // Windows 上还没收紧到「仅当前用户」的 ACL（docs/log/031 记过这条不足）。
            // 与其假装检查过，不如把实际情况说出来。
            section["details"].push_back(
                "session.json 用的是系统默认权限，还没收紧到仅当前用户");
#else
            const auto mode = std::filesystem::status(session, ec).permissions();
            const bool tight =
                (mode & (std::filesystem::perms::group_all | std::filesystem::perms::others_all)) ==
                std::filesystem::perms::none;
            if (!tight) {
                ok = false;
                section["details"].push_back("session.json 对同组和其他用户可读，应当收紧到 0600");
            }
#endif
        }

        section["ok"] = ok;
        section["summary"] = "端口 " + std::to_string(admin_port_) +
                             (ok ? " 在监听" : " 有问题") + "，口令文件" +
                             (std::filesystem::exists(session, ec) ? "在" : "缺失");
        sections.push_back(section);
    }

    // 四、有没有人试过越权注册或越权广播
    {
        nlohmann::json section;
        section["key"] = "violations";
        section["title"] = "越权记录";
        section["details"] = nlohmann::json::array();

        // 取最新的一份内核日志，而不是「今天」那一份：跨零点时今天这份可能还没建。
        std::error_code log_ec;
        std::filesystem::path log_path;
        for (const auto& entry : std::filesystem::directory_iterator(data_root_ / "logs", log_ec)) {
            if (entry.path().extension() != ".log") {
                continue;
            }
            if (log_path.empty() || entry.path().filename().string() >
                                        log_path.filename().string()) {
                log_path = entry.path();
            }
        }
        std::vector<std::string> hits;
        std::ifstream input(log_path);
        std::string line;
        while (std::getline(input, line)) {
            if (line.find(err::kNamespaceViolation) != std::string::npos) {
                hits.push_back(line);
            }
        }
        section["ok"] = hits.empty();
        if (hits.empty()) {
            section["summary"] = "无";
        } else {
            all_ok = false;
            section["summary"] = "今天有 " + std::to_string(hits.size()) + " 条";
            const std::size_t begin = hits.size() > 10 ? hits.size() - 10 : 0;
            for (std::size_t i = begin; i < hits.size(); ++i) {
                section["details"].push_back(hits[i]);
            }
        }
        sections.push_back(section);
    }

    nlohmann::json report;
    report["ok"] = all_ok;
    report["sections"] = sections;
    return report.dump();
}

std::string Kernel::plan_json() {
    std::map<std::string, bool> active;
    const DepGraph graph = current_graph(&active);

    nlohmann::json result;
    result["steps"] = nlohmann::json::array();
    result["blocked"] = nlohmann::json::array();

    const auto cycle = find_cycle(graph);
    if (!cycle.empty()) {
        result["ok"] = false;
        result["code"] = err::kDependencyCycle;
        std::string text;
        for (std::size_t i = 0; i < cycle.size(); ++i) {
            text += (i == 0 ? "" : " → ");
            text += cycle[i];
        }
        result["message"] = "依赖成环：" + text;
        return result.dump();
    }

    const auto plan = compute_plan(graph, active);
    std::uint32_t index = 0;
    for (const auto& step : plan) {
        result["steps"].push_back({{"index", ++index},
                                   {"action", step.activate ? "activate" : "deactivate"},
                                   {"plugin", step.plugin_id}});
    }
    // 装着但起不来的，一起列出来：用户看「为什么它没跑」时不用再去翻日志。
    std::set<std::string> reported;
    for (const auto& item : graph.unmet) {
        if (reported.count(item.plugin_id) != 0) {
            continue;
        }
        reported.insert(item.plugin_id);
        result["blocked"].push_back(
            {{"plugin", item.plugin_id}, {"reason", graph.unmet_text(item.plugin_id)}});
    }
    result["ok"] = true;
    return result.dump();
}

std::string Kernel::reconcile() {
    std::map<std::string, bool> active;
    const DepGraph graph = current_graph(&active);

    const auto cycle = find_cycle(graph);
    if (!cycle.empty()) {
        // 成环时一步都不走：半截计划比不动更难排查（04/04 §5.4）。
        std::string text = std::string(err::kDependencyCycle) + " 依赖成环：";
        for (std::size_t i = 0; i < cycle.size(); ++i) {
            text += (i == 0 ? "" : " → ");
            text += cycle[i];
        }
        log_error("loader", text);
        return text;
    }

    for (const auto& item : graph.unmet) {
        log_warn("loader", item.plugin_id + " 的依赖不满足：" + item.reason);
    }
    // 未满足的原因先全部记下：一直没激活过的插件不会走「停用」那段，
    // 不记的话列表里只显示「已停用」，用户看不出为什么。
    for (const auto& item : active) {
        const std::string text = graph.unmet_text(item.first);
        if (text.empty()) {
            blocked_.erase(item.first);
        } else {
            blocked_[item.first] = text;
        }
    }

    const auto plan = compute_plan(graph, active);
    if (plan.empty()) {
        return {};  // 全都对得上：不产生任何步骤，也不广播（04/04 §5.1）
    }

    const std::uint64_t plan_id = next_plan_id_++;
    std::uint32_t index = 0;
    const auto announce = [&](const PlanStep& step, bool ok, const std::string& error) {
        nlohmann::json payload;
        payload["plan_id"] = plan_id;
        payload["step"] = ++index;
        payload["step_total"] = plan.size();
        payload["action"] = step.activate ? "activate" : "deactivate";
        payload["plugin"] = step.plugin_id;
        payload["ok"] = ok;
        if (!ok && !error.empty()) {
            payload["error"] = error;
        }
        log_info("plan", "计划 " + std::to_string(plan_id) + " 第 " +
                              std::to_string(index) + "/" + std::to_string(plan.size()) +
                              " 步：" + (step.activate ? "启用 " : "停用 ") + step.plugin_id +
                              (ok ? " 成功" : " 失败 " + error));
        emit_kernel("kernel.plan.step", payload.dump());
    };

    for (const auto& step : plan) {
        if (!step.activate) {
            const std::string reason = graph.unmet_text(step.plugin_id);
            const std::string code = unload_one(step.plugin_id);
            if (!code.empty()) {
                log_warn("loader", "调和时停用失败 " + step.plugin_id + "：" + code);
                announce(step, false, code);
                continue;
            }
            announce(step, true, {});
            blocked_[step.plugin_id] = reason;
            log_info("loader", step.plugin_id + " 因依赖不满足停用：" + reason);
            const LoadedPlugin* plugin = find(step.plugin_id);
            if (plugin != nullptr) {
                publish_state(*plugin, "deactivated", reason);
            }
            continue;
        }

        blocked_.erase(step.plugin_id);
        const std::string code = load_one(step.plugin_id);
        if (!code.empty()) {
            log_warn("loader", "调和时激活失败 " + step.plugin_id + "：" + code);
            announce(step, false, code);
        } else {
            log_info("loader", "依赖满足，激活 " + step.plugin_id);
            announce(step, true, {});
        }
    }
    return {};
}

std::string Kernel::load_one(const std::string& plugin_id) {
    // 编号会拼进路径：先按清单规则校验，避免 ../ 把插件装到 plugins/ 之外。
    if (!valid_plugin_id(plugin_id)) {
        return std::string(err::kPluginMissing) + " 编号不合法：" + plugin_id;
    }
    if (LoadedPlugin* existing = find(plugin_id); existing != nullptr) {
        if (existing->state == PluginState::kActivated) {
            return std::string(err::kPluginExists) + " " + plugin_id + " 已在运行";
        }
        // 上次激活失败的实例还留在表里：不先清掉的话，新版本会被这个旧实例顶掉，
        // reload 之后表里看到的仍是旧的失败实例。
        deactivate(*existing);
        plugins_.erase(plugins_.begin() + (existing - plugins_.data()));
        contexts_.erase(plugin_id);
    }
    const auto dir = plugins_dir() / plugin_id;
    std::error_code ec;
    if (!std::filesystem::exists(dir / "plugin.toml", ec)) {
        return std::string(err::kPluginMissing) + " " + plugin_id;
    }
    if (is_disabled(plugin_id)) {
        return std::string(err::kPluginMissing) + " " + plugin_id + " 已停用";
    }
    const ManifestResult parsed = load_manifest(dir, host_version_string());
    if (!parsed.ok()) {
        return parsed.code + " " + parsed.detail;
    }
    // 清单第 12 步的常驻配额校验。放在真正装载之前，超限时不产生失败实例。
    const std::string quota = resident_quota_error(plugins_, *parsed.manifest, plugin_id);
    if (!quota.empty()) {
        log_warn("loader", plugin_id + " 未装载：" + quota);
        return quota;
    }

    LoadedPlugin plugin;
    plugin.manifest = *parsed.manifest;
    plugin.generation = next_generation_++;
    plugins_.push_back(std::move(plugin));

    const std::string code = activate(plugins_.back());
    if (!code.empty()) {
        plugins_.back().state = PluginState::kFailed;
        plugins_.back().last_error = code;
        log_error("loader", plugins_.back().manifest.id + " 激活失败：" + code);
        publish_state(plugins_.back(), "failed", code);
        return code;
    }
    publish_state(plugins_.back(), "activated");
    return {};
}

std::string Kernel::unload_one(const std::string& plugin_id) {
    LoadedPlugin* plugin = find(plugin_id);
    if (plugin == nullptr || plugin->state != PluginState::kActivated) {
        return std::string(err::kPluginMissing) + " " + plugin_id + " 未激活";
    }
    deactivate(*plugin);
    plugins_.erase(plugins_.begin() + (plugin - plugins_.data()));
    contexts_.erase(plugin_id);
    return {};
}

namespace {

// 把最近 40 行读成 JSON 数组。日志按行追加，末尾没有换行也照收。
nlohmann::json tail_lines(const std::filesystem::path& path, std::size_t limit) {
    nlohmann::json lines = nlohmann::json::array();
    std::ifstream input(path);
    if (!input) {
        return lines;
    }
    std::vector<std::string> collected;
    std::string line;
    while (std::getline(input, line)) {
        collected.push_back(line);
    }
    const std::size_t begin = collected.size() > limit ? collected.size() - limit : 0;
    for (std::size_t i = begin; i < collected.size(); ++i) {
        lines.push_back(collected[i]);
    }
    return lines;
}

// 管理端口经 kernel.call 能代理的插件方法。按前缀放开，只覆盖「管理动作」这一类：
// 配置读写、定时任务、插件管理器的只读查询。业务插件的方法一律不放。
bool admin_proxyable(const std::string& method) {
    static const char* const kPrefixes[] = {"core-settings.", "core-job.",
                                            "core-plugin-manager."};
    for (const char* prefix : kPrefixes) {
        if (method.rfind(prefix, 0) == 0) {
            return true;
        }
    }
    return false;
}

// 内核方法返回的字符串形如 "E_XXX 说明"，拆成错误码与说明两段。
void fill_error(Kernel::KernelReply& reply, const std::string& text) {
    const auto space = text.find(' ');
    if (space == std::string::npos) {
        reply.code = text;
        reply.message = text;
        return;
    }
    reply.code = text.substr(0, space);
    reply.message = text.substr(space + 1);
}

std::string iso8601_now() {
    const auto now = std::chrono::system_clock::now();
    const std::time_t stamp = std::chrono::system_clock::to_time_t(now);
    const std::tm local = sys::local_time(&stamp);
    char buffer[32] = {};
    std::strftime(buffer, sizeof(buffer), "%Y-%m-%dT%H:%M:%S", &local);
    return buffer;
}

}  // namespace

Kernel::KernelReply Kernel::handle_kernel(const std::string& method,
                                          const std::string& args_json) {
    KernelReply reply;

    nlohmann::json args = nlohmann::json::object();
    if (!args_json.empty()) {
        try {
            args = nlohmann::json::parse(args_json);
        } catch (const std::exception&) {
            reply.code = err::kProtocol;
            reply.message = "args 不是合法 JSON";
            return reply;
        }
        if (!args.is_object()) {
            reply.code = err::kProtocol;
            reply.message = "args 必须是对象";
            return reply;
        }
    }

    const auto string_arg = [&args](const char* key) {
        if (args.contains(key) && args[key].is_string()) {
            return args[key].get<std::string>();
        }
        return std::string();
    };

    // kernel.call：管理端口的受限代理。
    //
    // 为什么需要它：配置与定时任务的方法是插件方法（core-settings.write、core-job.schedule），
    // 而管理端口只放行 kernel.*，于是界面与命令行都改不了配置、建不了任务。
    // 放开的范围按前缀白名单收窄，只给「管理动作」这一类的内置插件开，业务插件不受影响。
    // 完整说明见 docs/ling/04-详细设计/06-管理端口与命令行.md 3.5。
    if (method == "kernel.call") {
        const std::string inner = string_arg("method");
        if (!admin_proxyable(inner)) {
            reply.code = err::kNamespaceViolation;
            reply.message = "管理端口不能调用 " + inner;
            return reply;
        }
        std::string payload = "{}";
        if (args.contains("args")) {
            if (!args["args"].is_object()) {
                reply.code = err::kProtocol;
                reply.message = "args.args 必须是对象";
                return reply;
            }
            payload = args["args"].dump();
        }
        const CallOutcome outcome =
            call(inner, reinterpret_cast<const std::uint8_t*>(payload.data()), payload.size());
        if (!outcome.ok()) {
            reply.code = outcome.code;
            reply.message = "调用 " + inner + " 失败";
            if (!outcome.detail.empty()) {
                reply.message += "：" + outcome.detail;
            } else if (!outcome.code.empty()) {
                reply.message += "（" + outcome.code + "）";
            }
            return reply;
        }
        reply.result = std::string(outcome.data.begin(), outcome.data.end());
        return reply;
    }

    // kernel.panel_call：core-ui 代某个插件的面板调用它自己的方法。
    //
    // 与 kernel.call 分开的理由：那条是「管理动作」的窄白名单，只管三个内置插件；
    // 面板要调的是业务插件的方法，白名单拦不住也不该放开。这条改成按前缀校验——
    // 只能调 "<plugin>.*"，而 plugin 由 core-ui 按消息来源填，面板指定不了。
    // 权限链因此是：面板只能调自己 → 外壳校验一次 → 宿主在这里再校验一次。
    if (method == "kernel.panel_call") {
        const std::string plugin = string_arg("plugin");
        const std::string inner = string_arg("method");
        if (plugin.empty() || !valid_plugin_id(plugin) || inner.empty()) {
            reply.code = err::kProtocol;
            reply.message = "panel_call 需要 plugin 与 method 两个字段";
            return reply;
        }
        if (inner.rfind(plugin + ".", 0) != 0) {
            reply.code = err::kUiForbidden;
            reply.message = "面板不能调用 " + inner;
            return reply;
        }
        // 插件当前必须可用：停用后它的面板即使还开着，也调不动任何方法。
        const LoadedPlugin* target = find(plugin);
        if (target == nullptr || target->state != PluginState::kActivated) {
            reply.code = err::kUiForbidden;
            reply.message = "插件 " + plugin + " 当前没有运行";
            return reply;
        }
        std::string payload = "{}";
        if (args.contains("params") && !args["params"].is_null()) {
            if (!args["params"].is_object()) {
                reply.code = err::kProtocol;
                reply.message = "params 必须是对象";
                return reply;
            }
            payload = args["params"].dump();
        }
        const CallOutcome outcome =
            call(inner, reinterpret_cast<const std::uint8_t*>(payload.data()), payload.size());
        if (!outcome.ok()) {
            reply.code = outcome.code;
            reply.message = "调用 " + inner + " 失败";
            if (!outcome.detail.empty()) {
                reply.message += "：" + outcome.detail;
            } else if (!outcome.code.empty()) {
                reply.message += "（" + outcome.code + "）";
            }
            return reply;
        }
        reply.result = std::string(outcome.data.begin(), outcome.data.end());
        return reply;
    }

    if (method == "kernel.plugins") {
        nlohmann::json list = nlohmann::json::array();
        for (const auto& plugin : plugins_) {
            nlohmann::json item;
            item["id"] = plugin.manifest.id;
            item["version"] = plugin.manifest.version;
            item["state"] = state_name(plugin.state);
            // 看清单而不是运行时状态：插件停用之后进程没了，但它仍然是侧车插件。
            item["track"] = plugin.manifest.sidecar.present ? "sidecar" : "process";
            if (!plugin.last_error.empty()) {
                item["reason"] = plugin.last_error;
            }
            list.push_back(item);
        }
        // 已停用但还装着的插件不在装载表里，单独扫一遍目录补上。
        std::error_code ec;
        for (const auto& entry : std::filesystem::directory_iterator(plugins_dir(), ec)) {
            if (!entry.is_directory()) {
                continue;
            }
            const std::string id = entry.path().filename().string();
            if (find(id) != nullptr) {
                continue;
            }
            if (!std::filesystem::exists(entry.path() / "plugin.toml", ec)) {
                continue;
            }
            const ManifestResult parsed = load_manifest(entry.path(), host_version_string());
            nlohmann::json item;
            item["id"] = id;
            item["version"] = parsed.ok() ? parsed.manifest->version : std::string();
            item["state"] = "deactivated";
            item["track"] = parsed.ok() && parsed.manifest->sidecar.present ? "sidecar"
                                                                            : "process";
            // 因依赖不满足停用的，把原因带上：否则用户只看到「已停用」，不知道为什么。
            const auto blocked = blocked_.find(id);
            if (blocked != blocked_.end()) {
                item["reason"] = blocked->second;
            }
            list.push_back(item);
        }
        nlohmann::json result;
        result["plugins"] = list;
        reply.result = result.dump();
        return reply;
    }

    if (method == "kernel.doctor") {
        reply.result = doctor_report();
        return reply;
    }

    if (method == "kernel.plan") {
        // 只看不做：界面与命令行先用它告诉用户「点下去会发生什么」。
        reply.result = plan_json();
        return reply;
    }

    if (method == "kernel.reload" || method == "kernel.restart") {
        const std::string id = string_arg("id");
        if (id.empty()) {
            reply.code = err::kPluginMissing;
            reply.message = "缺少 id";
            return reply;
        }
        if (!valid_plugin_id(id)) {
            reply.code = err::kPluginMissing;
            reply.message = "编号不合法：" + id;
            return reply;
        }
        const auto dir = plugins_dir() / id;
        std::error_code dir_ec;
        if (!std::filesystem::exists(dir / "plugin.toml", dir_ec)) {
            reply.code = err::kPluginMissing;
            reply.message = "找不到这个插件：" + id;
            return reply;
        }
        const ManifestResult before = load_manifest(dir, host_version_string());
        if (!before.ok()) {
            // 新版本清单本身就不合法，不能先把旧的卸掉再发现装不回去。
            reply.code = err::kReloadFailed;
            reply.message = "新版本的清单有问题，旧版本还在用：" + before.detail;
            return reply;
        }
        // load_manifest 读的是磁盘上的清单，也就是「新版本」；「上一个版本」要趁
        // 卸载前从内存里取，否则 unload_one 之后就丢了。
        const std::string new_version = before.manifest->version;
        std::string previous_version;
        if (const LoadedPlugin* current = find(id); current != nullptr) {
            previous_version = current->manifest.version;
        }

        // 让依赖它的先停：先置为不可用，调和一次，再换产物。
        reloading_.insert(id);
        reconcile();
        const LoadedPlugin* reloading_plugin = find(id);
        const bool was_activated =
            reloading_plugin != nullptr && reloading_plugin->state == PluginState::kActivated;
        const std::string unload_code = unload_one(id);
        if (was_activated && !unload_code.empty()) {
            // 旧版本停不下来就报失败：不能装作换成功，否则界面以为已是新版本。
            reloading_.erase(id);
            reconcile();
            reply.code = err::kReloadFailed;
            reply.message = "停掉旧版本失败：" + unload_code;
            return reply;
        }
        const std::string load_code = load_one(id);
        reloading_.erase(id);
        if (!load_code.empty()) {
            reply.code = err::kReloadFailed;
            reply.message = "换版本失败，插件已停下：" + load_code;
            reconcile();
            return reply;
        }
        reconcile();

        nlohmann::json result;
        result["id"] = id;
        result["version"] = new_version;
        result["previous_version"] = previous_version;
        const LoadedPlugin* plugin = find(id);
        result["state"] = plugin != nullptr && plugin->state == PluginState::kActivated
                              ? "activated"
                              : "deactivated";
        const auto blocked = blocked_.find(id);
        if (blocked != blocked_.end()) {
            result["reason"] = blocked->second;
        }
        reply.result = result.dump();
        return reply;
    }

    if (method == "kernel.install") {
        const std::string path = string_arg("path");
        if (path.empty()) {
            reply.code = err::kIo;
            reply.message = "缺少 path";
            return reply;
        }
        // path 来自管理端口 JSON，是 UTF-8；Windows 上不能让它隐式当 ANSI 路径用。
        const std::filesystem::path source = std::filesystem::u8path(path);
        const std::string code = install(source);
        if (!code.empty()) {
            fill_error(reply, code);
            return reply;
        }
        // 安装完不直接装载，先调和一次：新装的插件可能依赖别人，也可能让别的
        // 插件从「等依赖」变成「可以起」。顺序与原因都由调和统一决定（04/04 §5）。
        const ManifestResult parsed = load_manifest(source, host_version_string());
        const std::string id = parsed.ok() ? parsed.manifest->id : std::string();
        nlohmann::json result;
        result["id"] = id;
        result["version"] = parsed.ok() ? parsed.manifest->version : std::string();
        if (!id.empty()) {
            reconcile();
        }
        const LoadedPlugin* plugin = id.empty() ? nullptr : find(id);
        const bool activated = plugin != nullptr && plugin->state == PluginState::kActivated;
        result["state"] = activated ? "activated" : "deactivated";
        if (!activated) {
            const auto blocked = blocked_.find(id);
            if (blocked != blocked_.end()) {
                result["reason"] = blocked->second;
            }
        }
        reply.result = result.dump();
        return reply;
    }

    if (method == "kernel.uninstall") {
        const std::string id = string_arg("id");
        if (id.empty()) {
            reply.code = err::kPluginMissing;
            reply.message = "缺少 id";
            return reply;
        }
        const bool purge = args.contains("purge") && args["purge"].is_boolean() && args["purge"];
        const std::string code = uninstall(id, purge);
        if (!code.empty()) {
            fill_error(reply, code);
            return reply;
        }
        blocked_.erase(id);
        // 卸掉被依赖的插件后，依赖它的那些要停下来，这里统一处理。
        reconcile();
        reply.result = "{\"id\":\"" + id + "\"}";
        return reply;
    }

    if (method == "kernel.enable" || method == "kernel.disable") {
        const std::string id = string_arg("id");
        if (id.empty()) {
            reply.code = err::kPluginMissing;
            reply.message = "缺少 id";
            return reply;
        }
        const bool enabled = method == "kernel.enable";
        const std::string code = set_enabled(id, enabled);
        if (!code.empty()) {
            fill_error(reply, code);
            return reply;
        }
        if (enabled) {
            load_one(id);
        } else {
            unload_one(id);
        }
        // 停用一个插件，依赖它的要跟着停；启用同理，依赖它的可能因此能起来。
        reconcile();

        // 回真实状态：启用一个依赖没满足的插件，调和会把它再停下。
        // 只说「已启用」会让用户以为在跑，然后在调它的时候撞上 E_PLUGIN_MISSING。
        nlohmann::json result;
        result["id"] = id;
        const LoadedPlugin* plugin = find(id);
        const bool activated = plugin != nullptr && plugin->state == PluginState::kActivated;
        result["state"] = activated ? "activated" : "deactivated";
        if (!activated) {
            const auto blocked = blocked_.find(id);
            if (blocked != blocked_.end()) {
                result["reason"] = blocked->second;
            }
        }
        reply.result = result.dump();
        return reply;
    }

    if (method == "kernel.logs") {
        const std::string id = string_arg("plugin");
        if (id.empty()) {
            reply.code = err::kPluginMissing;
            reply.message = "缺少 plugin";
            return reply;
        }
        if (!valid_plugin_id(id)) {
            reply.code = err::kPluginMissing;
            reply.message = "插件编号不合法：" + id;
            return reply;
        }
        const auto dir = plugins_dir() / id / "data";
        std::error_code ec;
        if (!std::filesystem::exists(dir, ec)) {
            reply.code = err::kPluginMissing;
            reply.message = "插件 " + id + " 没有数据目录";
            return reply;
        }
        // 三个可选参数给界面用：条数、级别过滤、上翻偏移。命令行只给 plugin，
        // 这时行为与以前一样（40 条、不过滤）。参数交给 core-log 之前先夹住，
        // 别让界面要一万条。
        // 先取 int64 再夹取：3e9 这种值直接 get<int>() 会抛 out_of_range，
        // 一个越界的界面参数不该让整个管理请求崩掉。
        const auto clamp_arg = [&args](const char* key, std::int64_t fallback, std::int64_t low,
                                       std::int64_t high) {
            if (!args.contains(key)) {
                return fallback;
            }
            const auto& node = args[key];
            std::int64_t value = fallback;
            if (node.is_number_unsigned()) {
                const std::uint64_t raw = node.get<std::uint64_t>();
                value = raw > static_cast<std::uint64_t>(high) ? high
                                                              : static_cast<std::int64_t>(raw);
            } else if (node.is_number_integer()) {
                value = node.get<std::int64_t>();
            } else {
                return fallback;
            }
            return std::clamp(value, low, high);
        };
        const int count = static_cast<int>(clamp_arg("lines", 40, 1, 1000));
        const int before = static_cast<int>(clamp_arg("before", 0, 0, 10000));
        const std::string level = string_arg("level");
        // core-log 在时优先问它（04/06 §5.5 的第一条路径），不在时退回读 data/ 下的 .log。
        if (find("core-log") != nullptr) {
            // 有级别过滤走 query（逐行筛），否则走 tail（只读末尾几行）。
            const std::string target = level.empty() ? "core-log.tail" : "core-log.query";
            if (router_.contains(target)) {
                nlohmann::json request;
                request["plugin"] = id;
                if (level.empty()) {
                    request["n"] = count + before;
                } else {
                    request["level"] = level;
                }
                const std::string text = request.dump();
                const CallOutcome outcome = call(
                    target, reinterpret_cast<const std::uint8_t*>(text.data()), text.size());
                if (outcome.ok()) {
                    const std::string tail_text(outcome.data.begin(), outcome.data.end());
                    auto parsed = nlohmann::json::parse(tail_text, nullptr, false);
                    // core-log 里没有这个插件的记录时退回 data/，不然「日志为空」会被当成真的没有。
                    if (!parsed.is_discarded() && parsed.contains("lines") &&
                        parsed["lines"].is_array() && !parsed["lines"].empty()) {
                        auto& lines = parsed["lines"];
                        // query 返回的是整份日志，界面只要末尾 count 条，再往前翻 before 条。
                        const std::size_t keep = static_cast<std::size_t>(count + before);
                        if (lines.size() > keep) {
                            lines.erase(lines.begin(),
                                        lines.end() - static_cast<std::ptrdiff_t>(keep));
                        }
                        if (before > 0) {
                            const auto drop = static_cast<std::size_t>(before);
                            if (lines.size() > drop) {
                                lines.erase(lines.end() - static_cast<std::ptrdiff_t>(drop),
                                            lines.end());
                            } else {
                                lines = nlohmann::json::array();
                            }
                        }
                        reply.result = parsed.dump();
                        return reply;
                    }
                }
            }
        }
        // 取数据目录下最新的 .log 文件；没有就用空列表，不报错。
        std::filesystem::path latest;
        std::filesystem::directory_iterator iterator(dir, ec);
        if (ec) {
            reply.code = err::kIo;
            reply.message = "无法读取日志目录：" + ec.message();
            return reply;
        }
        while (iterator != std::filesystem::directory_iterator()) {
            const auto& entry = *iterator;
            if (entry.is_regular_file() && entry.path().extension() == ".log") {
                // 每次取时间都用独立的 error_code：共用一个失败状态会让
                // 「某次读取失败」把后面的比较全带偏。
                std::error_code entry_ec;
                const auto stamp = std::filesystem::last_write_time(entry, entry_ec);
                if (!entry_ec) {
                    bool newer = latest.empty();
                    if (!newer) {
                        std::error_code latest_ec;
                        const auto latest_stamp =
                            std::filesystem::last_write_time(latest, latest_ec);
                        newer = latest_ec || stamp > latest_stamp;
                    }
                    if (newer) {
                        latest = entry.path();
                    }
                }
            }
            iterator.increment(ec);
            if (ec) {
                reply.code = err::kIo;
                reply.message = "遍历日志目录失败：" + ec.message();
                return reply;
            }
        }
        nlohmann::json result;
        result["plugin"] = id;
        result["path"] = latest.empty() ? std::string() : latest.string();
        auto lines = latest.empty()
                         ? nlohmann::json::array()
                         : tail_lines(latest, static_cast<std::size_t>(count + before));
        if (before > 0) {
            const auto drop = static_cast<std::size_t>(before);
            if (lines.size() > drop) {
                lines.erase(lines.end() - static_cast<std::ptrdiff_t>(drop), lines.end());
            } else {
                lines = nlohmann::json::array();
            }
        }
        result["lines"] = lines;
        reply.result = result.dump();
        return reply;
    }

    if (method == "kernel.shutdown") {
        shutdown_requested_ = true;
        reply.shutdown = true;
        reply.result = "{\"stopping\":true}";
        log_info("kernel", "收到 kernel.shutdown");
        return reply;
    }

    reply.code = err::kMethodMissing;
    reply.message = "没有这个方法：" + method;
    return reply;
}

namespace {

// 侧车方法的代理：路由表里存的是这个函数，调用时转成一次 invoke 并等结果。
int32_t sidecar_method_thunk(void* handler_ctx, const uint8_t* args, size_t args_len,
                             uint8_t** out, size_t* out_len) {
    auto* method = static_cast<SidecarMethodCtx*>(handler_ctx);
    if (method == nullptr || method->kernel == nullptr) {
        return 3;
    }
    const std::string text(reinterpret_cast<const char*>(args), args_len);
    const CallOutcome outcome =
        method->kernel->invoke_sidecar(method->plugin_id, method->method, text);
    if (!outcome.ok()) {
        return 3;  // 侧车没答上来：调用方按「方法失败」处理
    }
    if (!outcome.data.empty()) {
        auto* buffer = static_cast<uint8_t*>(std::malloc(outcome.data.size()));
        if (buffer == nullptr) {
            return 1;
        }
        std::memcpy(buffer, outcome.data.data(), outcome.data.size());
        *out = buffer;
        *out_len = outcome.data.size();
    } else {
        *out = nullptr;
        *out_len = 0;
    }
    return 0;
}

// 侧车事件订阅的代理：把事件写成一帧发过去。
int32_t sidecar_event_thunk(void* handler_ctx, const char* topic, const uint8_t* payload,
                            size_t len) {
    auto* sub = static_cast<SidecarMethodCtx*>(handler_ctx);
    if (sub == nullptr || sub->kernel == nullptr) {
        return 3;
    }
    const std::string text(reinterpret_cast<const char*>(payload), len);
    return sub->kernel->deliver_to_sidecar(sub->plugin_id, topic == nullptr ? "" : topic, text);
}

std::string make_sidecar_token() {
    std::random_device device;
    const char* digits = "0123456789abcdef";
    std::string hex;
    hex.reserve(64);
    for (int i = 0; i < 32; ++i) {
        const auto byte = static_cast<unsigned>(device() & 0xFFu);
        hex.push_back(digits[byte >> 4]);
        hex.push_back(digits[byte & 0x0Fu]);
    }
    return hex;
}

}  // namespace

std::filesystem::path Kernel::python_interpreter() const {
    // 优先级：显式指定 → 随包分发的运行时 → 系统 PATH。
    const std::string custom = sys::read_env("LING_PYTHON");
    if (!custom.empty()) {
        return std::filesystem::path(custom);
    }
    if (!program_dir_.empty()) {
        // 随包 Python 运行时的目录布局因平台而异（Windows 是 python.exe，Linux 是
        // bin/python3），这是「打包布局」策略，不是系统调用差异，故不放进 ling::sys。
#ifdef _WIN32
        const auto bundled = program_dir_ / "runtime" / "python" / "python.exe";
#else
        const auto bundled = program_dir_ / "runtime" / "python" / "bin" / "python3";
#endif
        std::error_code ec;
        if (std::filesystem::exists(bundled, ec)) {
            return bundled;
        }
    }
    return std::filesystem::path("python");
}

std::string Kernel::activate_sidecar(LoadedPlugin& plugin) {
    const auto dir = plugins_dir() / plugin.manifest.id;
    const auto data_dir = dir / "data";
    std::error_code ec;
    std::filesystem::create_directories(data_dir, ec);
    if (ec) {
        return std::string(err::kIo) + " 无法创建数据目录 " + data_dir.string() + "：" +
               ec.message();
    }

    // 两种运行时走同一套握手与帧，差别只在「用什么起这个进程」。
    std::filesystem::path exe;
    std::string exe_args;
    std::filesystem::path sdk_dir;
    if (plugin.manifest.sidecar.runtime == "python") {
        exe = python_interpreter();
        if (exe.empty()) {
            return std::string(err::kInternal) +
                   " 找不到 Python 解释器：装一个 Python 3，或用环境变量 LING_PYTHON 指定路径";
        }
        const auto script = dir / plugin.manifest.sidecar.entry;
        exe_args = quote_command_arg(script.string());
        if (!program_dir_.empty()) {
            sdk_dir = program_dir_ / "python";
        }
        log_info("sidecar", plugin.manifest.id + " 用 " + exe.string() + " 跑 " +
                                plugin.manifest.sidecar.entry);
    } else {
        exe = dir / plugin.manifest.sidecar.executable;
    }

    if (plugin.process != nullptr) {
        plugin.process.reset();
    }
    plugin.sidecar = true;
    plugin.sidecar_token = make_sidecar_token();
    plugin.process = std::make_unique<Sidecar>();
    // 界面进程要替浏览器去连管理端口，那枚口令是会话口令（session.json 里那枚），
    // 跟侧车握手口令是两回事，不能拿这一枚顶替。只给 core-ui。
    // 数据根与起始端口一起给：界面不自己推断数据根在哪（core-ui/04-01 §2.1）。
    UiEnv ui_env;
    if (plugin.manifest.id == "core-ui") {
        std::string token_error;
        if (!admin::read_token(data_root_, &ui_env.token, &token_error)) {
            ui_env.token.clear();
        }
        ui_env.root = data_root_.string();
        ui_env.port = std::to_string(kUiStartPort);
    }
    const std::string start_error =
        plugin.process->start(exe, exe_args, dir, plugin.manifest.id, plugin.sidecar_token,
                              data_dir.string(), sdk_dir.string(), ui_env);
    if (!start_error.empty()) {
        plugin.process.reset();
        plugin.sidecar = false;
        return std::string(err::kInternal) + " " + start_error;
    }

    // 握手：先等 hello，校验口令与插件名，再发 activate（04/02 §8.4）。
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(5);
    bool greeted = false;
    while (std::chrono::steady_clock::now() < deadline) {
        sidecar::Frame frame;
        bool closed = false;
        std::string error;
        if (!plugin.process->read(&frame, 100, &closed, &error)) {
            if (closed) {
                plugin.process.reset();
                plugin.sidecar = false;
                return std::string(err::kInternal) + " 侧车启动后立刻退出";
            }
            continue;
        }
        if (frame.op != "hello") {
            log_warn("sidecar", plugin.manifest.id + " 握手阶段收到 " + frame.op + "，已断开");
            plugin.process->terminate(1000);
            plugin.process.reset();
            plugin.sidecar = false;
            return std::string(err::kProtocol) + " 握手第一帧必须是 hello";
        }
        const std::string token =
            frame.fields.contains("token") && frame.fields["token"].is_string()
                ? frame.fields["token"].get<std::string>()
                : std::string();
        const std::string who =
            frame.fields.contains("plugin_id") && frame.fields["plugin_id"].is_string()
                ? frame.fields["plugin_id"].get<std::string>()
                : std::string();
        if (token != plugin.sidecar_token) {
            log_warn("sidecar", plugin.manifest.id + " 口令不对，已断开");
            plugin.process->terminate(1000);
            plugin.process.reset();
            plugin.sidecar = false;
            return std::string(err::kAuth) + " 侧车口令不匹配";
        }
        if (who != plugin.manifest.id) {
            plugin.process->terminate(1000);
            plugin.process.reset();
            plugin.sidecar = false;
            return std::string(err::kProtocol) + " 侧车自报的插件名是 " + who;
        }
        greeted = true;
        break;
    }
    if (!greeted) {
        plugin.process->terminate(500);
        plugin.process.reset();
        plugin.sidecar = false;
        return std::string(err::kTimeout) + " 侧车 5 秒内没有握手";
    }

    sidecar::Frame activate;
    activate.op = "activate";
    activate.fields = {{"plugin_id", plugin.manifest.id},
                       {"generation", plugin.generation},
                       {"data_dir", data_dir.string()}};
    std::string send_error;
    if (!plugin.process->send(activate, &send_error)) {
        plugin.process->terminate(500);
        plugin.process.reset();
        plugin.sidecar = false;
        return std::string(err::kIo) + " 发 activate 失败：" + send_error;
    }

    plugin.state = PluginState::kActivated;
    plugin.last_used_at = std::chrono::steady_clock::now();
    log_info("loader", plugin.manifest.id + " 侧车已启动（pid " +
                           std::to_string(plugin.process->pid()) + "）");
    return {};
}

LoadedPlugin* Kernel::find_sidecar(const std::string& plugin_id) {
    LoadedPlugin* plugin = find(plugin_id);
    if (plugin == nullptr || !plugin->sidecar || plugin->process == nullptr) {
        return nullptr;
    }
    return plugin;
}

bool Kernel::handle_sidecar_frame(LoadedPlugin& plugin, const sidecar::Frame& frame) {
    if (frame.op == "register_method") {
        const std::string name = frame.fields.contains("name") && frame.fields["name"].is_string()
                                     ? frame.fields["name"].get<std::string>()
                                     : std::string();
        if (name.empty()) {
            return true;
        }
        const std::string key = plugin.manifest.id + "/" + name;
        // 先建新上下文、注册成功后再落表：注册失败（例如重名）时旧上下文还在，
        // 路由里的 handler_ctx 也就不会被换成一个马上要被删掉的指针。
        auto ctx = std::make_unique<SidecarMethodCtx>();
        ctx->kernel = this;
        ctx->plugin_id = plugin.manifest.id;
        ctx->method = name;
        const std::string code = router_.register_method(
            plugin.manifest.id, name, reinterpret_cast<void*>(&sidecar_method_thunk), ctx.get(), 0);
        if (!code.empty()) {
            log_warn("sidecar", plugin.manifest.id + " 登记 " + name + " 失败：" + code);
            return true;
        }
        sidecar_method_ctx_[key] = std::move(ctx);
        plugin.sidecar_methods.push_back(name);
        log_info("sidecar", plugin.manifest.id + " 登记方法 " + name);
        return true;
    }

    if (frame.op == "subscribe") {
        const std::string pattern =
            frame.fields.contains("pattern") && frame.fields["pattern"].is_string()
                ? frame.fields["pattern"].get<std::string>()
                : std::string();
        if (pattern.empty()) {
            return true;
        }
        auto& ctx = sidecar_sub_ctx_[plugin.manifest.id];
        if (ctx == nullptr) {
            ctx = std::make_unique<SidecarMethodCtx>();
            ctx->kernel = this;
            ctx->plugin_id = plugin.manifest.id;
        }
        std::uint64_t sub_id = 0;
        const std::string code = events_.subscribe(
            plugin.manifest.id, pattern, reinterpret_cast<void*>(&sidecar_event_thunk),
            ctx.get(), &sub_id);
        if (!code.empty()) {
            log_warn("sidecar", plugin.manifest.id + " 订阅 " + pattern + " 失败：" + code);
            return true;
        }
        plugin.sidecar_subs.push_back(sub_id);
        return true;
    }

    if (frame.op == "emit") {
        const std::string topic = frame.fields.contains("topic") && frame.fields["topic"].is_string()
                                      ? frame.fields["topic"].get<std::string>()
                                      : std::string();
        std::string payload;
        if (frame.fields.contains("payload")) {
            payload = frame.fields["payload"].is_string()
                          ? frame.fields["payload"].get<std::string>()
                          : frame.fields["payload"].dump();
        }
        if (!topic.empty()) {
            emit_from(plugin.manifest.id, topic,
                      reinterpret_cast<const std::uint8_t*>(payload.data()), payload.size());
        }
        return true;
    }

    if (frame.op == "call") {
        // 侧车要调别人的方法：宿主代它调，结果按同一个 id 回去。
        const std::string method = frame.fields.contains("method") &&
                                           frame.fields["method"].is_string()
                                       ? frame.fields["method"].get<std::string>()
                                       : std::string();
        std::string args = frame.fields.contains("args") ? frame.fields["args"].dump() : "{}";
        std::string rewritten_args;
        if (rewrite_caller_plugin(method.c_str(), plugin.manifest.id,
                                  reinterpret_cast<const std::uint8_t*>(args.data()), args.size(),
                                  &rewritten_args)) {
            args = std::move(rewritten_args);
        }
        const CallOutcome outcome =
            call(method, reinterpret_cast<const std::uint8_t*>(args.data()), args.size());
        std::string error;
        sidecar::Frame out;
        out.id = frame.id;
        out.op = outcome.ok() ? "result" : "error";
        if (outcome.ok()) {
            // 方法可以返回 JSON，也可以返回一段纯文本（例如 demo-a 的 pong）。
            // 解析不出来的按字符串带回去——直接塞 discarded 会让序列化抛异常。
            nlohmann::json value = nullptr;
            if (!outcome.data.empty()) {
                const std::string text(outcome.data.begin(), outcome.data.end());
                value = nlohmann::json::parse(text, nullptr, false);
                if (value.is_discarded()) {
                    value = text;
                }
            }
            out.fields = nlohmann::json{{"result", value}};
        } else {
            out.fields = nlohmann::json{{"error",
                                         {{"code", outcome.code},
                                          {"message", "宿主代调用失败"}}}};
        }
        plugin.process->send(out, &error);
        return true;
    }

    if (frame.op == "error") {
        log_warn("sidecar", plugin.manifest.id + " 报错：" + frame.fields.dump());
        return true;
    }

    if (frame.op == "deactivated") {
        return true;  // 停用流程在等它，这里不做事
    }

    // 走到这说明 op 不在侧车合法集合里。侧车协议里 op 是封闭枚举，未知值要么是
    // 版本不匹配、要么是坏帧，记一条 warn 方便排查，不静默吞掉。
    log_warn("sidecar", plugin.manifest.id + " 发来未知 op：" + frame.op);
    return true;
}

void Kernel::reap_idle_sidecars() {
    const auto now = std::chrono::steady_clock::now();
    for (auto& plugin : plugins_) {
        if (!plugin.sidecar || plugin.process == nullptr) {
            continue;
        }
        // 常驻的（restart = "always"）不因空闲退出；0 表示按需但永不自动退出。
        if (plugin.manifest.sidecar.restart == "always" ||
            plugin.manifest.sidecar.idle_timeout <= 0) {
            continue;
        }
        const auto idle = std::chrono::duration_cast<std::chrono::seconds>(now - plugin.last_used_at);
        if (idle < std::chrono::seconds(plugin.manifest.sidecar.idle_timeout)) {
            continue;
        }

        // 先发 shutdown 让它自己退，体面收尾；不退就杀掉。
        sidecar::Frame bye;
        bye.op = "shutdown";
        bye.fields = nlohmann::json::object();
        std::string error;
        plugin.process->send(bye, &error);
        plugin.process->terminate(1000);
        plugin.process.reset();
        plugin.sidecar = false;
        // 路由与订阅故意保留：调用方下次调它要能进到 invoke_sidecar，
        // 由那里重新拉起进程。清掉的话调用方只会得到「没有这个方法」。
        log_info("sidecar", plugin.manifest.id + " 空闲 " + std::to_string(idle.count()) +
                                " 秒，已退出（下次调用时再起）");
    }
}

void Kernel::pump_sidecars() {
    reap_idle_sidecars();
    for (auto& plugin : plugins_) {
        if (!plugin.sidecar || plugin.process == nullptr) {
            continue;
        }
        if (!plugin.process->alive()) {
            const int code = plugin.process->exit_code();
            sidecar_lost(plugin, "侧车退出（代码 " + std::to_string(code) + "）");
            continue;
        }
        // 一轮最多收 16 帧，避免一个话多的侧车把整轮循环占住。
        for (int i = 0; i < 16; ++i) {
            sidecar::Frame frame;
            bool closed = false;
            std::string error;
            if (!plugin.process->read(&frame, 0, &closed, &error)) {
                if (closed) {
                    sidecar_lost(plugin, "侧车断开");
                }
                break;
            }
            handle_sidecar_frame(plugin, frame);
        }
    }
}

void Kernel::sidecar_lost(LoadedPlugin& plugin, const std::string& reason) {
    const std::string id = plugin.manifest.id;
    router_.unregister_all(id);
    events_.unsubscribe_all(id);
    // 方法/订阅都没了，它们各自的宿主上下文也要一起清：否则 crash 后按
    // restart 策略重起并重新注册时，旧上下文一直留在表里。
    const std::string prefix = id + "/";
    for (auto it = sidecar_method_ctx_.begin(); it != sidecar_method_ctx_.end();) {
        if (it->first.compare(0, prefix.size(), prefix) == 0) {
            it = sidecar_method_ctx_.erase(it);
        } else {
            ++it;
        }
    }
    sidecar_sub_ctx_.erase(id);
    plugin.sidecar_methods.clear();
    plugin.sidecar_subs.clear();
    const int code = plugin.process != nullptr ? plugin.process->exit_code() : -1;
    plugin.process.reset();
    plugin.sidecar = false;

    publish_state(plugin, "crashed", reason);
    log_warn("sidecar", id + " " + reason);

    const std::string policy = plugin.manifest.sidecar.restart;
    const bool normal_exit = code == 0;
    bool restart = policy == "always" || (policy == "on-crash" && !normal_exit);
    if (policy == "never") {
        restart = false;
    }
    if (restart && plugin.restart_attempts >= 3) {
        // 连续三次起不来就别再试了：反复重启一个坏插件只会刷日志（§8.5）。
        restart = false;
        log_error("sidecar", id + " 连续 3 次起不来，停止重启");
    }
    if (restart) {
        plugin.restart_pending = true;
        plugin.restart_at = std::chrono::steady_clock::now() + std::chrono::seconds(2);
        ++plugin.restart_attempts;
        plugin.state = PluginState::kFailed;
        plugin.last_error = reason + "，2 秒后重启";
        return;
    }

    plugin.state = PluginState::kFailed;
    plugin.last_error = reason;
}

CallOutcome Kernel::invoke_sidecar(const std::string& plugin_id, const std::string& method,
                                  const std::string& args_json) {
    CallOutcome outcome;
    LoadedPlugin* plugin = find(plugin_id);
    if (plugin == nullptr || !plugin->manifest.sidecar.present) {
        outcome.code = err::kPluginMissing;
        outcome.detail = "这个插件不是侧车插件";
        return outcome;
    }
    if (plugin->state != PluginState::kActivated) {
        outcome.code = err::kPluginMissing;
        // 「装着但没在跑」和「没装」要分开说，否则用户会去重装一个本来就装着的插件。
        outcome.detail = blocked_.count(plugin_id) > 0 ? "插件现在没在跑：" + blocked_[plugin_id]
                                                      : "插件现在没在跑";
        return outcome;
    }

    // 按需侧车：空闲退出之后，第一次调用要把它重新拉起来（04/02 §8.7）。
    // 重新拉起会再走一遍握手，侧车会重新登记方法与订阅，所以先把旧的清掉，
    // 否则同一批名字会被当成重复注册。
    if (plugin->process == nullptr || !plugin->process->alive()) {
        router_.unregister_all(plugin_id);
        events_.unsubscribe_all(plugin_id);
        plugin->sidecar_methods.clear();
        plugin->sidecar_subs.clear();
        plugin->sidecar = false;
        const std::string start_code = activate_sidecar(*plugin);
        if (!start_code.empty()) {
            outcome.code = err::kInternal;
            outcome.detail = "重新拉起侧车失败：" + start_code;
            return outcome;
        }
        log_info("sidecar", plugin_id + " 按需拉起（上次是空闲退出的）");
    }
    plugin->last_used_at = std::chrono::steady_clock::now();

    const int call_id = static_cast<int>(next_sidecar_call_id_++);
    nlohmann::json call_args = nlohmann::json::parse(args_json, nullptr, false);
    if (call_args.is_discarded()) {
        call_args = nlohmann::json::object();  // 空参就是空对象，别把 discarded 塞进帧里
    }
    sidecar::Frame invoke;
    invoke.id = call_id;
    invoke.op = "invoke";
    invoke.fields = {{"method", method}, {"args", call_args}};
    std::string error;
    if (!plugin->process->send(invoke, &error)) {
        outcome.code = err::kIo;
        outcome.detail = "发 invoke 失败：" + error;
        return outcome;
    }

    log_info("sidecar", "invoke #" + std::to_string(call_id) + " " + method);
    const auto started = std::chrono::steady_clock::now();
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(5);
    while (std::chrono::steady_clock::now() < deadline) {
        sidecar::Frame frame;
        bool closed = false;
        if (!plugin->process->read(&frame, 50, &closed, &error)) {
            if (closed) {
                outcome.code = err::kInternal;
                outcome.detail = "侧车在调用过程中退出";
                return outcome;
            }
            if (!error.empty()) {
                // 解析失败也要说出来，否则一帧坏数据会让调用方干等到超时。
                log_warn("sidecar", "无法解析侧车的帧：" + error);
            }
            continue;
        }
        if (frame.id == call_id && frame.op == "result") {
            log_info("sidecar", "invoke #" + std::to_string(call_id) + " 完成，用时 " +
                                    std::to_string(std::chrono::duration_cast<std::chrono::milliseconds>(
                                                       std::chrono::steady_clock::now() - started)
                                                       .count()) +
                                    " 毫秒");
            if (frame.fields.contains("result")) {
                const std::string text = frame.fields["result"].is_string()
                                             ? frame.fields["result"].get<std::string>()
                                             : frame.fields["result"].dump();
                outcome.data.assign(text.begin(), text.end());
            }
            return outcome;
        }
        if (frame.id == call_id && frame.op == "error") {
            // 错误帧的 code / message 都可能缺失或类型不对，先判类型再取，
            // 不做 get<std::string>()，免得坏帧把异常抛到调用路径上。
            if (frame.fields.contains("error") && frame.fields["error"].is_object() &&
                frame.fields["error"].contains("code") &&
                frame.fields["error"]["code"].is_string()) {
                outcome.code = frame.fields["error"]["code"].get<std::string>();
            } else {
                outcome.code = err::kInternal;
            }
            if (frame.fields.contains("error") && frame.fields["error"].is_object() &&
                frame.fields["error"].contains("message") &&
                frame.fields["error"]["message"].is_string()) {
                outcome.detail = frame.fields["error"]["message"].get<std::string>();
            }
            return outcome;
        }
        // 等待期间来的别的帧照常处理（登记、事件、别人的调用）。
        handle_sidecar_frame(*plugin, frame);
    }

    outcome.code = err::kTimeout;
    outcome.detail = "侧车 5 秒没有应答";
    return outcome;
}

int32_t Kernel::deliver_to_sidecar(const std::string& plugin_id, const std::string& topic,
                                  const std::string& payload) {
    LoadedPlugin* plugin = find_sidecar(plugin_id);
    if (plugin == nullptr) {
        return 3;
    }
    sidecar::Frame event;
    event.op = "event";
    event.fields = {{"topic", topic}, {"payload", payload}};
    std::string error;
    return plugin->process->send(event, &error) ? 0 : 3;
}

std::string Kernel::activate(LoadedPlugin& plugin) {
    if (plugin.manifest.sidecar.present) {
        return activate_sidecar(plugin);
    }

    const auto path = plugins_dir() / plugin.manifest.id / plugin.manifest.artifact;
    plugin.module = sys::load_library(path);
    if (plugin.module == nullptr) {
        return std::string(err::kIo) + " 无法装载 " + path.string();
    }

    auto* entry = reinterpret_cast<ling_plugin_entry_fn>(sys::find_symbol(plugin.module,
                                                                    "ling_plugin_entry"));
    if (entry == nullptr) {
        sys::unload_library(plugin.module);
        plugin.module = nullptr;
        return std::string(err::kManifestInvalid) + " 缺少 ling_plugin_entry";
    }

    // 入口即激活点：插件在 entry 里登记方法，所以状态要先置为可注册。
    plugin.state = PluginState::kActivated;

    // entry 一旦跑过，就可能已经登记了方法和订阅。后面任何校验失败都必须先把
    // 这些注册撤干净再卸库，否则路由和事件表里留着指向已卸载 DLL 的函数指针。
    auto rollback_activation = [&plugin, this]() {
        const UndoReport report = plugin.ledger.undo_all();
        if (report.failed > 0) {
            log_error("loader", plugin.manifest.id + " 激活回滚有 " +
                                   std::to_string(report.failed) + " 条效应撤销失败");
        }
        router_.unregister_all(plugin.manifest.id);
        events_.unsubscribe_all(plugin.manifest.id);
        sys::unload_library(plugin.module);
        plugin.module = nullptr;
        plugin.handle = nullptr;
        plugin.state = PluginState::kInstalled;
    };

    ling_plugin_descriptor desc{};
    void* handle = nullptr;
    if (entry(&host_api_, &desc, &handle) != 0) {
        rollback_activation();
        return std::string(err::kInternal) + " 入口返回非 0";
    }

    if (desc.abi_version != LING_ABI_VERSION) {
        rollback_activation();
        return std::string(err::kApiMismatch) + " ABI 版本 " +
               std::to_string(desc.abi_version) + " 与宿主 " + std::to_string(LING_ABI_VERSION) +
               " 不同";
    }
    // 接口主版本、插件版本也要和清单一致：只有 ABI 对齐、版本却是另一份产物的
    // 插件，装进来后行为对不上，排查起来很难。
    if (desc.api == nullptr || plugin.manifest.api != desc.api) {
        rollback_activation();
        return std::string(err::kApiMismatch) + " 代码里的 api " +
               (desc.api != nullptr ? std::string(desc.api) : std::string("(空)")) + " 与清单 " +
               plugin.manifest.api + " 不一致";
    }
    if (desc.plugin_version == nullptr || plugin.manifest.version != desc.plugin_version) {
        rollback_activation();
        const std::string code_version =
            desc.plugin_version != nullptr ? std::string(desc.plugin_version) : std::string("(空)");
        return std::string(err::kApiMismatch) + " 代码里的 plugin_version " + code_version +
               " 与清单 " + plugin.manifest.version + " 不一致";
    }
    if (desc.plugin_id == nullptr || plugin.manifest.id != desc.plugin_id) {
        rollback_activation();
        return std::string(err::kManifestInvalid) + " 代码里的 plugin_id 与清单不一致";
    }

    plugin.handle = handle;
    log_info("loader", plugin.manifest.id + " 已激活，注册方法 " +
                           std::to_string(router_.size()) + " 个（累计）");
    return {};
}

void Kernel::deactivate(LoadedPlugin& plugin) {
    // 对外状态在撤销开始前就变成 deactivated，撤销过程中的回调看到的是最终态。
    publish_state(plugin, "deactivated");

    if (plugin.sidecar) {
        // 先请它自己清理（关文件、退订），给它 2 秒；不走就强杀（04/02 §8.3）。
        if (plugin.process != nullptr) {
            sidecar::Frame frame;
            frame.op = "deactivate";
            std::string error;
            plugin.process->send(frame, &error);
            const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(2);
            while (std::chrono::steady_clock::now() < deadline) {
                sidecar::Frame reply;
                bool closed = false;
                if (!plugin.process->read(&reply, 100, &closed, &error)) {
                    if (closed) {
                        break;
                    }
                    continue;
                }
                if (reply.op == "deactivated") {
                    break;
                }
            }
            plugin.process->terminate(2000);
            plugin.process.reset();
        }
        plugin.sidecar = false;
        plugin.sidecar_methods.clear();
        plugin.sidecar_subs.clear();
        router_.unregister_all(plugin.manifest.id);
        events_.unsubscribe_all(plugin.manifest.id);
        // 侧车停掉后，它注册方法与订阅时创建的宿主上下文也要按前缀清掉。
        const std::string prefix = plugin.manifest.id + "/";
        for (auto it = sidecar_method_ctx_.begin(); it != sidecar_method_ctx_.end();) {
            if (it->first.compare(0, prefix.size(), prefix) == 0) {
                it = sidecar_method_ctx_.erase(it);
            } else {
                ++it;
            }
        }
        sidecar_sub_ctx_.erase(plugin.manifest.id);
        plugin.handle = nullptr;
        plugin.state = PluginState::kInstalled;
        return;
    }
    const UndoReport report = plugin.ledger.undo_all();
    if (report.attempted > 0) {
        log_info("loader", plugin.manifest.id + " 撤销效应 " + std::to_string(report.attempted) +
                               " 条，失败 " + std::to_string(report.failed) + " 条");
    }
    // 撤销失败的条目留在磁盘上的痕迹没法自动清。记下来给 doctor 看，
    // 否则这类问题只在当次日志里出现一次，事后查不到。
    if (report.failed > 0) {
        pending_undo_[plugin.manifest.id] = report.failed;
    } else {
        pending_undo_.erase(plugin.manifest.id);
    }
    // 账本撤销完还有残留方法（例如插件绕过 Context 直接注册的），兜底清掉。
    const auto removed = router_.unregister_all(plugin.manifest.id);
    if (!removed.empty()) {
        log_warn("loader", plugin.manifest.id + " 账本外残留方法 " +
                               std::to_string(removed.size()) + " 个，已强制清空");
    }
    const auto patterns = events_.unsubscribe_all(plugin.manifest.id);
    if (!patterns.empty()) {
        log_warn("loader", plugin.manifest.id + " 账本外残留订阅 " +
                               std::to_string(patterns.size()) + " 条，已强制清空");
    }
    if (plugin.module != nullptr) {
        sys::unload_library(plugin.module);
        plugin.module = nullptr;
    }
    plugin.handle = nullptr;
    plugin.state = PluginState::kInstalled;
}

void* Kernel::create_context(const std::string& plugin_id, std::uint64_t generation) {
    auto context = std::make_unique<PluginContext>();
    context->kernel = this;
    context->plugin_id = plugin_id;
    context->generation = generation;
    context->data_dir = (plugins_dir() / plugin_id / "data").string();

    std::error_code ec;
    std::filesystem::create_directories(context->data_dir, ec);
    if (ec) {
        // 数据目录建不出来时不能把上下文交出去：插件拿到一个写不进数据根的
        // data_dir，后面所有读写都会以更难查的方式失败。
        log_error("loader", plugin_id + " 的数据目录建不出来：" + ec.message());
        return nullptr;
    }

    void* raw = context.get();
    contexts_[plugin_id] = std::move(context);
    return raw;
}

std::string Kernel::on_register(PluginContext* ctx, const std::string& name, void* fn,
                                std::uint32_t requires_mask) {
    auto* plugin = find(ctx->plugin_id);
    if (plugin == nullptr || plugin->state != PluginState::kActivated) {
        return err::kPluginMissing;
    }
    // 第 5 条：方法声明的能力必须是清单的子集（能力门禁，见 gate.hpp）。
    const std::uint32_t declared = capability_mask_of(plugin->manifest);
    if (!capability_allowed(requires_mask, declared)) {
        return err::kCapabilityDenied;
    }
    const std::string code = router_.register_method(ctx->plugin_id, name, fn, ctx, requires_mask);
    if (!code.empty()) {
        return code;
    }

    // 记账：撤销动作是注销这个方法。捕获所需值，避免悬挂引用。
    const std::string method_name = name;
    const std::string plugin_id = ctx->plugin_id;
    Effect effect;
    effect.kind = EffectKind::kMethodRegistered;
    effect.detail = method_name;
    effect.undo = [this, plugin_id, method_name]() {
        return router_.unregister_method(plugin_id, method_name).empty();
    };
    if (plugin != nullptr && !plugin->ledger.append(std::move(effect))) {
        router_.unregister_method(ctx->plugin_id, name);
        return err::kLedgerFull;
    }
    return {};
}

std::string Kernel::on_unregister(PluginContext* ctx, const std::string& name) {
    return router_.unregister_method(ctx->plugin_id, name);
}

std::string Kernel::on_subscribe(PluginContext* ctx, const std::string& pattern, void* fn,
                                 std::uint64_t* out_sub_id) {
    auto* plugin = find(ctx->plugin_id);
    if (plugin == nullptr || plugin->state != PluginState::kActivated) {
        return err::kPluginMissing;
    }

    std::uint64_t sub_id = 0;
    // 与方法回调一致：回调的第一个参数是宿主的上下文句柄，插件把它当作不透明值原样带回即可。
    const std::string code = events_.subscribe(ctx->plugin_id, pattern, fn, ctx, &sub_id);
    if (!code.empty()) {
        return code;
    }

    const std::string plugin_id = ctx->plugin_id;
    Effect effect;
    effect.kind = EffectKind::kSubscribed;
    effect.detail = pattern;
    effect.undo = [this, plugin_id, sub_id]() {
        return events_.unsubscribe(plugin_id, sub_id).empty();
    };
    if (!plugin->ledger.append(std::move(effect))) {
        events_.unsubscribe(plugin_id, sub_id);
        return err::kLedgerFull;
    }

    if (out_sub_id != nullptr) {
        *out_sub_id = sub_id;
    }
    log_info("event", plugin_id + " 订阅 " + pattern + "（sub_id=" + std::to_string(sub_id) + "）");
    return {};
}

std::string Kernel::on_unsubscribe(PluginContext* ctx, std::uint64_t sub_id) {
    return events_.unsubscribe(ctx->plugin_id, sub_id);
}

std::string Kernel::apply_config(const std::string& plugin_id, const std::string& key,
                                 const std::string& value_json) {
    nlohmann::json request;
    request["plugin"] = plugin_id;
    request["key"] = key;
    request["value"] = nlohmann::json::parse(value_json, nullptr, false);
    if (request["value"].is_discarded()) {
        request["value"] = value_json;  // 不是合法 JSON 就当字符串存
    }

    const std::string text = request.dump();
    const CallOutcome outcome = call("core-settings.write",
                                     reinterpret_cast<const std::uint8_t*>(text.data()),
                                     text.size());
    if (!outcome.ok()) {
        // core-settings 没装时给出明确错误码：配置是它提供的服务。
        if (outcome.code == err::kMethodMissing) {
            return err::kPluginMissing;
        }
        return outcome.code;
    }
    return {};
}

std::string Kernel::write_config(const std::string& plugin_id, const std::string& key,
                                 const std::uint8_t* value, std::size_t len) {
    const std::string prefix = plugin_id + ".";
    if (key.rfind(prefix, 0) != 0) {
        return err::kNamespaceViolation;
    }
    if (len > EventBus::kMaxPayloadBytes) {
        return err::kPayloadTooLarge;
    }
    LoadedPlugin* plugin = find(plugin_id);
    if (plugin == nullptr || plugin->state != PluginState::kActivated) {
        return err::kPluginMissing;
    }

    const std::string raw(reinterpret_cast<const char*>(value), len);

    // 先读旧值：撤销时按它写回，原本没有就删键。
    nlohmann::json get_request;
    get_request["key"] = key;
    const std::string get_text = get_request.dump();
    const CallOutcome previous = call("core-settings.get",
                                      reinterpret_cast<const std::uint8_t*>(get_text.data()),
                                      get_text.size());
    std::string old_json = "null";
    if (previous.ok()) {
        const auto parsed = nlohmann::json::parse(
            std::string(previous.data.begin(), previous.data.end()), nullptr, false);
        // core-settings 的回复必须有布尔 found；类型不对说明插件回复破损，
        // 这时候写下去等于用 null 覆盖旧值，撤销也还原不了。
        if (parsed.is_discarded() || !parsed.is_object() || !parsed.contains("found") ||
            !parsed["found"].is_boolean()) {
            log_error("ledger", plugin_id + " 读配置 " + key + " 的回复不符合协议");
            return err::kProtocol;
        }
        if (parsed["found"].get<bool>()) {
            if (!parsed.contains("value")) {
                log_error("ledger", plugin_id + " 读配置 " + key + " 的回复缺少 value");
                return err::kProtocol;
            }
            old_json = parsed["value"].dump();
        }
    } else if (previous.code == err::kMethodMissing) {
        return err::kPluginMissing;
    }

    const std::string code = apply_config(plugin_id, key, raw);
    if (!code.empty()) {
        return code;
    }

    Effect effect;
    effect.kind = EffectKind::kConfigWrite;
    effect.detail = key;
    effect.undo = [this, plugin_id, key, old_json]() {
        return apply_config(plugin_id, key, old_json).empty();
    };
    if (!plugin->ledger.append(std::move(effect))) {
        // 记账失败就把刚写的值也撤回去，别留下无主的配置。
        const std::string undo_code = apply_config(plugin_id, key, old_json);
        if (!undo_code.empty()) {
            // 回滚也失败：配置已改但账本里没有这条，doctor 必须能看到这处残留。
            ++pending_undo_[plugin_id];
            log_error("ledger", plugin_id + " 账本满后回滚 " + key + " 失败：" + undo_code);
        }
        return err::kLedgerFull;
    }
    return {};
}

EmitStats Kernel::emit_from(const std::string& plugin_id, const std::string& topic,
                            const std::uint8_t* payload, std::size_t len) {
    EmitStats stats;
    // 第 1 条：主题形状。
    if (!valid_topic_shape(topic)) {
        stats.code = err::kNamespaceViolation;
        log_warn("event", plugin_id + " 广播主题不合法：" + topic);
        return stats;
    }
    // 第 2 条：只能在自己的前缀下广播，kernel. 保留给内核。
    const std::string prefix = plugin_id + ".";
    if (topic.rfind(prefix, 0) != 0 || topic.rfind("kernel.", 0) == 0) {
        stats.code = err::kNamespaceViolation;
        log_warn("event", plugin_id + " 越权广播 " + topic + "，已丢弃");
        return stats;
    }
    // 第 3 条：载荷上限。
    if (len > EventBus::kMaxPayloadBytes) {
        stats.code = err::kPayloadTooLarge;
        log_warn("event", plugin_id + " 广播 " + topic + " 的载荷 " + std::to_string(len) +
                              " 字节，超过上限，已丢弃");
        return stats;
    }
    return events_.dispatch(topic, payload, len);
}

EmitStats Kernel::emit_kernel(const std::string& topic, const std::string& payload) {
    if (payload.size() > EventBus::kMaxPayloadBytes) {
        EmitStats stats;
        stats.code = err::kPayloadTooLarge;
        log_warn("event", "内核事件 " + topic + " 超限，已丢弃");
        return stats;
    }
    const auto* bytes = reinterpret_cast<const std::uint8_t*>(payload.data());
    return events_.dispatch(topic, bytes, payload.size());
}

void Kernel::publish_tick() {
    const auto now = std::chrono::system_clock::now();
    const auto epoch_ms =
        std::chrono::duration_cast<std::chrono::milliseconds>(now.time_since_epoch()).count();

    nlohmann::json payload;
    payload["now"] = iso8601_now();
    payload["epoch_ms"] = epoch_ms;
    // 心跳不写内核日志：每 200 毫秒一条会把日志刷满，排障时反而找不到东西。
    emit_kernel("kernel.tick", payload.dump());
}

void Kernel::publish_state(const LoadedPlugin& plugin, const char* state,
                           const std::string& reason) {
    nlohmann::json payload;
    payload["plugin_id"] = plugin.manifest.id;
    payload["state"] = state;
    payload["version"] = plugin.manifest.version;
    payload["generation"] = plugin.generation;
    if (!reason.empty()) {
        payload["reason"] = reason;
    }

    const EmitStats stats = emit_kernel("kernel.plugin.state", payload.dump());
    log_info("event", std::string("kernel.plugin.state ") + plugin.manifest.id + " " + state +
                           "（订阅者 " + std::to_string(stats.recipients) + "）");
}

CallOutcome Kernel::call(const std::string& method, const std::uint8_t* args,
                         std::size_t len) {
    CallOutcome outcome = router_.call(method, args, len);
    if (!outcome.ok()) {
        // 装着但没在跑（依赖不满足、被停用），原因写在 blocked_ 里。
        // 只回「方法不存在」会让调用方以为插件没装，白查半天。
        const auto dot = method.find('.');
        if (dot != std::string::npos) {
            const auto blocked = blocked_.find(method.substr(0, dot));
            if (blocked != blocked_.end()) {
                outcome.code = err::kPluginMissing;
                outcome.detail = "插件现在没在跑：" + blocked->second;
            }
        }
    }
    if (outcome.code == err::kMethodMissing) {
        // 二次查表只在失败路径：方法差一段，说明插件在但方法没注册。
        const auto dot = method.find('.');
        if (dot != std::string::npos) {
            const auto id = method.substr(0, dot);
            const auto* plugin = find(id);
            if (plugin == nullptr || plugin->state != PluginState::kActivated) {
                outcome.code = err::kPluginMissing;
            }
        }
    }
    return outcome;
}

}  // namespace ling
