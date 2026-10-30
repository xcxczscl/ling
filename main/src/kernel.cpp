#include "ling/kernel.hpp"

#include <algorithm>
#include <cstdlib>
#include <fstream>
#include <cstring>
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
    (void)ctx;
    (void)pattern;
    (void)fn;
    (void)undo_fn;
    (void)out_sub_id;
    return 3;  // 事件系统 S2 之后接入
}

int32_t host_unsubscribe(void* ctx, uint64_t sub_id) {
    (void)ctx;
    (void)sub_id;
    return 3;
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
    (void)ctx;
    (void)topic;
    (void)payload;
    (void)len;
    return 3;  // 事件系统 S2 之后接入
}

int32_t host_write_config(void* ctx, const char* key, const uint8_t* value, size_t len) {
    (void)ctx;
    (void)key;
    (void)value;
    (void)len;
    return 3;  // 由 core-settings 提供，M6 之前不接
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
    } catch (const std::exception& error) {
        // 配置坏了不阻塞启动：记一条 warn，按「没有停用记录」继续。
        log_warn("registry", std::string("registry.json 解析失败，按空配置继续：") + error.what());
    }
}

void Kernel::save_registry() const {
    nlohmann::json document;
    document["schema_version"] = 1;
    document["disabled"] = disabled_;

    const auto path = data_root_ / "registry.json";
    std::ofstream output(path, std::ios::trunc);
    if (!output) {
        log_error("registry", "无法写入 " + path.string());
        return;
    }
    output << document.dump(2) << "\n";
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

std::string Kernel::uninstall(const std::string& plugin_id, bool purge) {
    LoadedPlugin* plugin = find(plugin_id);
    if (plugin != nullptr) {
        if (plugin->state == PluginState::kActivated) {
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
            if (plugin->module != nullptr) {
                unload_library(plugin->module);
                plugin->module = nullptr;
            }
            plugin->handle = nullptr;
            plugin->state = PluginState::kInstalled;
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

    log_info("loader", std::string("已卸载 ") + plugin_id + (purge ? "（含数据）" : "（保留数据）"));
    return {};
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
            ++failures;
        }
    }
    return failures;
}

std::string Kernel::activate(LoadedPlugin& plugin) {
    const auto path = plugins_dir() / plugin.manifest.id / plugin.manifest.artifact;
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
