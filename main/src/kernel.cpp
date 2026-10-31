#include "ling/kernel.hpp"

#include <algorithm>
#include <chrono>
#include <cstdlib>
#include <ctime>
#include <fstream>
#include <cstring>
#include <string>
#include <vector>
#include <memory>
#include <system_error>
#include <utility>

#include "ling/errors.hpp"
#include "ling/log.hpp"
#include "ling/version.hpp"

#include <nlohmann/json.hpp>

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#else
#include <dlfcn.h>
#endif

namespace ling {
namespace {

Kernel* g_kernel = nullptr;  // 单进程单宿主，见 03 §5.1 的进程模型

void* load_library(const std::filesystem::path& path) {
#ifdef _WIN32
    return reinterpret_cast<void*>(::LoadLibraryA(path.string().c_str()));
#else
    return ::dlopen(path.string().c_str(), RTLD_NOW | RTLD_LOCAL);
#endif
}

void* find_symbol(void* module, const char* name) {
#ifdef _WIN32
    return reinterpret_cast<void*>(
        ::GetProcAddress(reinterpret_cast<HMODULE>(module), name));
#else
    return ::dlsym(module, name);
#endif
}

void unload_library(void* module) {
#ifdef _WIN32
    ::FreeLibrary(reinterpret_cast<HMODULE>(module));
#else
    ::dlclose(module);
#endif
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

const char* state_name(PluginState state) {
    switch (state) {
        case PluginState::kInstalled:
            return "installed";
        case PluginState::kActivated:
            return "activated";
        case PluginState::kFailed:
            return "failed";
        case PluginState::kUninstalled:
            return "uninstalled";
    }
    return "installed";
}

std::uint32_t capability_mask_of(const Manifest& manifest) {
    std::uint32_t mask = 0;
    for (const auto& capability : manifest.capabilities) {
        if (capability == "fs") {
            mask |= kCapFs;
        } else if (capability == "network") {
            mask |= kCapNetwork;
        } else if (capability == "ui") {
            mask |= kCapUi;
        } else if (capability == "shell") {
            mask |= kCapShell;
        }
    }
    return mask;
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
    std::ofstream output(path, std::ios::trunc);
    if (!output) {
        log_error("registry", "无法写入 " + path.string());
        return;
    }
    output << document.dump(2) << "\n";
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
    // 判定口径见 04/02 §2.1：plugin.toml 存在才算已安装。
    // 只剩 data/ 的目录算未安装，可以重装并沿用里面的数据。
    if (std::filesystem::exists(target / "plugin.toml", ec)) {
        return std::string(err::kPluginExists) + " 已存在 " + target.string() + "，先卸载再装";
    }

    std::filesystem::create_directories(target, ec);
    // 逐个拷贝顶层条目，跳过 data/，避免把旧数据覆盖掉。
    for (const auto& entry : std::filesystem::directory_iterator(source_dir, ec)) {
        if (entry.path().filename() == "data") {
            continue;
        }
        std::filesystem::copy(entry.path(), target / entry.path().filename(),
                              std::filesystem::copy_options::recursive |
                                  std::filesystem::copy_options::overwrite_existing,
                              ec);
        if (ec) {
            return std::string(err::kIo) + " 复制失败：" + ec.message();
        }
    }

    std::filesystem::create_directories(target / "data", ec);
    log_info("loader", "已安装 " + parsed.manifest->id + " " + parsed.manifest->version);
    return {};
}

std::string Kernel::uninstall(const std::string& plugin_id, bool purge) {    LoadedPlugin* plugin = find(plugin_id);
    if (plugin != nullptr) {
        const bool was_activated = plugin->state == PluginState::kActivated;
        if (was_activated) {
            const UndoReport report = plugin->ledger.undo_all();
            if (report.failed > 0) {
                // 撤销失败就不动文件：留下现场比删干净更有用。
                std::string detail = std::string(err::kUninstallIncomplete) + " 有 " +
                                     std::to_string(report.failed) + " 条效应撤销失败：";
                for (const auto& item : report.failed_items) {
                    detail += item + " ";
                }
                log_error("loader", plugin_id + " " + detail);
                return detail;
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
                unload_library(plugin->module);
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

    const auto dir = plugins_dir() / plugin_id;
    std::error_code ec;
    if (!std::filesystem::exists(dir, ec)) {
        return std::string(err::kPluginMissing) + " " + plugin_id;
    }

    for (const auto& entry : std::filesystem::directory_iterator(dir, ec)) {
        const auto name = entry.path().filename().string();
        if (name == "data" && !purge) {
            continue;
        }
        std::filesystem::remove_all(entry.path(), ec);
    }
    // --purge 连目录本身一起删掉；不带 purge 时目录要留着装 data/。
    if (purge) {
        std::filesystem::remove_all(dir, ec);
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
    static const char* const kBuiltins[] = {"core-settings", "core-plugin-manager", "core-log",
                                            "core-job"};
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
    return failures;
}

std::string Kernel::load_one(const std::string& plugin_id) {
    if (find(plugin_id) != nullptr) {
        return {};  // 已装载：幂等
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
    std::tm local{};
#ifdef _WIN32
    localtime_s(&local, &stamp);
#else
    localtime_r(&stamp, &local);
#endif
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

    if (method == "kernel.plugins") {
        nlohmann::json list = nlohmann::json::array();
        for (const auto& plugin : plugins_) {
            nlohmann::json item;
            item["id"] = plugin.manifest.id;
            item["version"] = plugin.manifest.version;
            item["state"] = state_name(plugin.state);
            item["track"] = "process";
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
            item["track"] = "process";
            list.push_back(item);
        }
        nlohmann::json result;
        result["plugins"] = list;
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
        const std::string code = install(path);
        if (!code.empty()) {
            fill_error(reply, code);
            return reply;
        }
        // 安装完立刻装载并激活，命令行才能看到 activated。
        const ManifestResult parsed = load_manifest(path, host_version_string());
        const std::string id = parsed.ok() ? parsed.manifest->id : std::string();
        const std::string load_code = id.empty() ? std::string() : load_one(id);
        nlohmann::json result;
        result["id"] = id;
        result["version"] = parsed.ok() ? parsed.manifest->version : std::string();
        result["state"] = load_code.empty() ? "activated" : "failed";
        if (!load_code.empty()) {
            result["reason"] = load_code;
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
        reply.result = "{\"id\":\"" + id + "\"}";
        return reply;
    }

    if (method == "kernel.logs") {
        const std::string id = string_arg("plugin");
        if (id.empty()) {
            reply.code = err::kPluginMissing;
            reply.message = "缺少 plugin";
            return reply;
        }
        const auto dir = plugins_dir() / id / "data";
        std::error_code ec;
        if (!std::filesystem::exists(dir, ec)) {
            reply.code = err::kPluginMissing;
            reply.message = "插件 " + id + " 没有数据目录";
            return reply;
        }
        // core-log 在时优先问它（04/06 §5.5 的第一条路径），不在时退回读 data/ 下的 .log。
        if (find("core-log") != nullptr && router_.contains("core-log.tail")) {
            nlohmann::json request;
            request["plugin"] = id;
            request["n"] = 40;
            const std::string text = request.dump();
            const CallOutcome outcome = call("core-log.tail",
                                             reinterpret_cast<const std::uint8_t*>(text.data()),
                                             text.size());
            if (outcome.ok()) {
                const std::string tail_text(outcome.data.begin(), outcome.data.end());
                const auto parsed = nlohmann::json::parse(tail_text, nullptr, false);
                // core-log 里没有这个插件的记录时退回 data/，不然「日志为空」会被当成真的没有。
                if (!parsed.is_discarded() && parsed.contains("lines") &&
                    !parsed["lines"].empty()) {
                    reply.result = tail_text;
                    return reply;
                }
            }
        }
        // 取数据目录下最新的 .log 文件；没有就用空列表，不报错。
        std::filesystem::path latest;
        for (const auto& entry : std::filesystem::directory_iterator(dir, ec)) {
            if (!entry.is_regular_file()) {
                continue;
            }
            if (entry.path().extension() != ".log") {
                continue;
            }
            if (latest.empty() ||
                std::filesystem::last_write_time(entry, ec) >
                    std::filesystem::last_write_time(latest, ec)) {
                latest = entry.path();
            }
        }
        nlohmann::json result;
        result["plugin"] = id;
        result["path"] = latest.empty() ? std::string() : latest.string();
        result["lines"] = latest.empty() ? nlohmann::json::array() : tail_lines(latest, 40);
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

std::string Kernel::activate(LoadedPlugin& plugin) {    const auto path = plugins_dir() / plugin.manifest.id / plugin.manifest.artifact;
    plugin.module = load_library(path);
    if (plugin.module == nullptr) {
        return std::string(err::kIo) + " 无法装载 " + path.string();
    }

    auto* entry = reinterpret_cast<ling_plugin_entry_fn>(find_symbol(plugin.module,
                                                                    "ling_plugin_entry"));
    if (entry == nullptr) {
        unload_library(plugin.module);
        plugin.module = nullptr;
        return std::string(err::kManifestInvalid) + " 缺少 ling_plugin_entry";
    }

    // 入口即激活点：插件在 entry 里登记方法，所以状态要先置为可注册。
    plugin.state = PluginState::kActivated;

    ling_plugin_descriptor desc{};
    void* handle = nullptr;
    if (entry(&host_api_, &desc, &handle) != 0) {
        unload_library(plugin.module);
        plugin.module = nullptr;
        return std::string(err::kInternal) + " 入口返回非 0";
    }

    if (desc.abi_version != LING_ABI_VERSION) {
        unload_library(plugin.module);
        plugin.module = nullptr;
        return std::string(err::kApiMismatch) + " ABI 版本 " +
               std::to_string(desc.abi_version) + " 与宿主 " + std::to_string(LING_ABI_VERSION) +
               " 不同";
    }
    if (desc.plugin_id == nullptr || plugin.manifest.id != desc.plugin_id) {
        unload_library(plugin.module);
        plugin.module = nullptr;
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
    const UndoReport report = plugin.ledger.undo_all();
    if (report.attempted > 0) {
        log_info("loader", plugin.manifest.id + " 撤销效应 " + std::to_string(report.attempted) +
                               " 条，失败 " + std::to_string(report.failed) + " 条");
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
        unload_library(plugin.module);
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
    // 第 5 条：方法声明的能力必须是清单的子集。
    const std::uint32_t declared = capability_mask_of(plugin->manifest);
    if ((requires_mask & ~declared) != 0) {
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
        if (!parsed.is_discarded() && parsed.contains("found") && parsed["found"].get<bool>()) {
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
        apply_config(plugin_id, key, old_json);
        return err::kLedgerFull;
    }
    return {};
}

EmitStats Kernel::emit_from(const std::string& plugin_id, const std::string& topic,                            const std::uint8_t* payload, std::size_t len) {
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
