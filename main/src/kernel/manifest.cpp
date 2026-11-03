#include "ling/manifest.hpp"

#include "ling/depgraph.hpp"

#include <algorithm>
#include <array>
#include <cctype>
#include <string_view>
#include <utility>

#include "ling/errors.hpp"
#include "ling/sys.hpp"

#if defined(__has_include)
#if __has_include(<toml.hpp>)
#include <toml.hpp>
#endif
#endif

namespace ling {
namespace {

// MVP 阶段的两条临时限制（docs/ling/11-MVP 范围.md §4.2）：
// 声明了依赖或走侧车轨道的插件一律拒绝装载，而不是静默忽略。
// 静默忽略会让插件以为条件已满足，出错现象跑到调用时才出现。
// 依赖声明随 M3 放开（04/04）；侧车随 M4 放开（04/02 §8）。
// 两个开关留着，是为了将来若要临时关掉某条轨道时不用翻代码。
constexpr bool kMvpRejectRequires = false;
constexpr bool kMvpRejectSidecar = false;

constexpr const char* kSupportedApis[] = {"1", "0"};

bool is_supported_api(const std::string& api) {
    for (const char* candidate : kSupportedApis) {
        if (api == candidate) {
            return true;
        }
    }
    return false;
}

bool has_leading_core(const std::string& id) { return id.rfind("core-", 0) == 0; }

// 五个内置插件，随宿主分发、默认预装、可卸载（04/07）。
constexpr const char* kBuiltinIds[] = {"core-settings", "core-plugin-manager", "core-log",
                                       "core-job", "core-ui"};

// 拆三段版本号，失败返回空。
// 三段数字，发布前可以带一个 -bN（docs/common/02-版本号与兼容约定.md §1）。
// 比较只看前三段：-b3 与 -b4 在兼容判断里是同一个版本，
// 构建序号不参与「谁更新」的判断。
std::array<int, 3> split_version(const std::string& text) {
    std::array<int, 3> parts{-1, -1, -1};
    std::size_t at = 0;
    for (int index = 0; index < 3; ++index) {
        const std::size_t start = at;
        int value = 0;
        while (at < text.size() && text[at] >= '0' && text[at] <= '9') {
            value = value * 10 + (text[at] - '0');
            if (value > 65535) {
                return {-1, -1, -1};
            }
            ++at;
        }
        if (at == start) {
            return {-1, -1, -1};  // 这一段没有数字
        }
        parts[static_cast<std::size_t>(index)] = value;
        if (index < 2) {
            if (at >= text.size() || text[at] != '.') {
                return {-1, -1, -1};
            }
            ++at;
        }
    }
    if (at == text.size()) {
        return parts;
    }
    // 只认 -b<数字> 这一种后缀。
    if (text[at] != '-' || at + 1 >= text.size() || text[at + 1] != 'b' ||
        at + 2 >= text.size()) {
        return {-1, -1, -1};
    }
    std::size_t digits = 0;
    for (std::size_t i = at + 2; i < text.size(); ++i) {
        if (text[i] < '0' || text[i] > '9') {
            return {-1, -1, -1};
        }
        ++digits;
        if (digits > 9) {
            return {-1, -1, -1};  // 构建号最多 9 位，挡住异常长的数字
        }
    }
    return parts;
}

std::string lower(std::string text) {
    std::transform(text.begin(), text.end(), text.begin(),
                   [](unsigned char ch) { return static_cast<char>(std::tolower(ch)); });
    return text;
}

}  // namespace

bool valid_plugin_id(const std::string& id) {
    if (id.size() < 2 || id.size() > 64) {
        return false;
    }
    auto allowed = [](unsigned char ch) {
        return std::islower(ch) != 0 || std::isdigit(ch) != 0 || ch == '-';
    };
    // 首字符只允许字母或数字：id 形如 ^[a-z0-9][a-z0-9-]*$，连字符不能打头。
    const auto first = static_cast<unsigned char>(id.front());
    if (std::islower(first) == 0 && std::isdigit(first) == 0) {
        return false;
    }
    return std::all_of(id.begin(), id.end(),
                       [&](char ch) { return allowed(static_cast<unsigned char>(ch)); });
}

bool valid_version(const std::string& version) {
    return split_version(version)[0] >= 0;
}

bool version_at_least(const std::string& candidate, const std::string& required) {
    if (required.empty()) {
        return true;
    }
    const auto lhs = split_version(candidate);
    const auto rhs = split_version(required);
    if (lhs[0] < 0 || rhs[0] < 0) {
        return false;
    }
    for (std::size_t i = 0; i < lhs.size(); ++i) {
        if (lhs[i] != rhs[i]) {
            return lhs[i] > rhs[i];
        }
    }
    return true;
}

ManifestResult load_manifest(const std::filesystem::path& dir, const std::string& host_version) {
    ManifestResult result;

    const auto manifest_path = dir / "plugin.toml";
    if (!std::filesystem::exists(manifest_path)) {
        result.code = err::kManifestInvalid;
        result.detail = "缺少 plugin.toml";
        return result;
    }

#if defined(LING_HAS_TOMLPP)
    toml::table table;
    try {
        table = toml::parse_file(manifest_path.string());
    } catch (const toml::parse_error& error) {
        result.code = err::kManifestInvalid;
        result.detail = std::string("清单解析失败：") + std::string(error.description());
        return result;
    }

    Manifest manifest;

    auto require_string = [&](const char* key, std::string* out) -> bool {
        const auto* node = table.get(key);
        if (node == nullptr || !node->is_string()) {
            result.code = err::kManifestInvalid;
            result.detail = std::string("缺少必填字段 ") + key;
            return false;
        }
        *out = node->value<std::string>().value_or("");
        return true;
    };

    if (!require_string("id", &manifest.id) || !require_string("version", &manifest.version) ||
        !require_string("api", &manifest.api) || !require_string("name", &manifest.name)) {
        return result;
    }

    if (const auto* node = table.get("min_host"); node != nullptr && node->is_string()) {
        manifest.min_host = node->value<std::string>().value_or("");
    }
    if (const auto* node = table.get("description"); node != nullptr && node->is_string()) {
        manifest.description = node->value<std::string>().value_or("");
    }
    if (const auto* node = table.get("capabilities"); node != nullptr && node->is_array()) {
        for (const auto& item : *node->as_array()) {
            if (item.is_string()) {
                manifest.capabilities.push_back(item.value<std::string>().value_or(""));
            }
        }
    }
    // [ui] 段：只解析，校验放在第 11 步（那时能力表已经读完）。
    if (const auto* ui = table.get("ui"); ui != nullptr) {
        if (!ui->is_table()) {
            result.code = err::kUiPanelDecl;
            result.detail = "[ui] 必须是一个表（[ui] 段），不是一个值";
            return result;
        }
        manifest.ui.present = true;
        if (const auto* entry = ui->as_table()->get("entry"); entry != nullptr && entry->is_string()) {
            manifest.ui.entry = entry->value<std::string>().value_or("");
        }
        if (const auto* title = ui->as_table()->get("title"); title != nullptr && title->is_string()) {
            manifest.ui.title = title->value<std::string>().value_or("");
        }
        if (const auto* icon = ui->as_table()->get("icon"); icon != nullptr && icon->is_string()) {
            manifest.ui.icon = icon->value<std::string>().value_or("");
        }
    }
    if (const auto* requires = table.get("requires"); requires != nullptr && requires->is_table()) {
        if (const auto* plugins = requires->as_table()->get("plugins");
            plugins != nullptr && plugins->is_array()) {
            for (const auto& item : *plugins->as_array()) {
                if (item.is_string()) {
                    manifest.requires_plugins.push_back(item.value<std::string>().value_or(""));
                }
            }
        }
    }
    if (const auto* sidecar = table.get("sidecar"); sidecar != nullptr && sidecar->is_table()) {
        manifest.sidecar.present = true;
        if (const auto* exe = sidecar->as_table()->get("executable");
            exe != nullptr && exe->is_string()) {
            manifest.sidecar.executable = exe->value<std::string>().value_or("");
        }
        if (const auto* restart = sidecar->as_table()->get("restart");
            restart != nullptr && restart->is_string()) {
            manifest.sidecar.restart = restart->value<std::string>().value_or("on-crash");
        }
        if (const auto* runtime = sidecar->as_table()->get("runtime");
            runtime != nullptr && runtime->is_string()) {
            manifest.sidecar.runtime = runtime->value<std::string>().value_or("process");
        }
        if (const auto* entry = sidecar->as_table()->get("entry");
            entry != nullptr && entry->is_string()) {
            manifest.sidecar.entry = entry->value<std::string>().value_or("");
        }
        if (const auto* idle = sidecar->as_table()->get("idle_timeout");
            idle != nullptr && idle->is_integer()) {
            manifest.sidecar.idle_timeout = idle->value<std::int32_t>().value_or(60);
        }
    }

    // 第 1 步：目录名等于 id。
    if (dir.filename().string() != manifest.id) {
        result.code = err::kManifestInvalid;
        result.detail = "目录名 " + dir.filename().string() + " 与清单 id " + manifest.id + " 不同";
        return result;
    }

    // 第 3 步：id 格式。
    if (!valid_plugin_id(manifest.id)) {
        result.code = err::kManifestInvalid;
        result.detail = "id 格式不合法：" + manifest.id;
        return result;
    }

    // core- 前缀保留给内置插件：表里的编号放行，表外的拒绝（04/07 §2.3）。
    if (has_leading_core(manifest.id) && !is_builtin_plugin(manifest.id)) {
        result.code = err::kManifestInvalid;
        result.detail = "core- 前缀保留给内置插件";
        return result;
    }

    // 第 4 步：version 三段式。
    if (!valid_version(manifest.version)) {
        result.code = err::kManifestInvalid;
        result.detail = "version 必须是三段式且每段在 0 至 65535：" + manifest.version;
        return result;
    }

    // 第 5 步：api 兼容。
    if (!is_supported_api(manifest.api)) {
        result.code = err::kApiMismatch;
        result.detail = "宿主支持 api 1 与 0，清单声明 " + manifest.api;
        return result;
    }

    // 第 6 步：min_host。
    if (!manifest.min_host.empty() && !version_at_least(host_version, manifest.min_host)) {
        result.code = err::kHostTooOld;
        result.detail = "需要宿主 " + manifest.min_host + "，当前 " + host_version;
        return result;
    }

    // 第 7 步：路径不含绝对路径与 ..
    auto unsafe_relative = [](const std::string& text) {
        return text.empty() || std::filesystem::path(text).is_absolute() ||
               text.find("..") != std::string::npos;
    };

    // 第 8 步：产物组合。
    const std::string artifact_name = manifest.id + sys::shared_library_suffix();
    manifest.artifact = artifact_name;
    const bool has_artifact = std::filesystem::exists(dir / artifact_name);
    if (manifest.sidecar.present && has_artifact) {
        result.code = err::kManifestInvalid;
        result.detail = "动态库与 [sidecar] 段并存";
        return result;
    }
    if (manifest.sidecar.present && manifest.sidecar.runtime != "process" &&
        manifest.sidecar.runtime != "python") {
        result.code = err::kManifestInvalid;
        result.detail = "sidecar.runtime 只能是 process 或 python，现在写的是 " +
                        manifest.sidecar.runtime;
        return result;
    }
    if (manifest.sidecar.present && manifest.sidecar.runtime == "python") {
        if (manifest.sidecar.entry.empty()) {
            result.code = err::kManifestInvalid;
            result.detail = "runtime = python 时必须写 sidecar.entry（脚本路径）";
            return result;
        }
        if (unsafe_relative(manifest.sidecar.entry)) {
            result.code = err::kManifestInvalid;
            result.detail = "sidecar.entry 必须是插件目录内的相对路径";
            return result;
        }
        if (!std::filesystem::exists(dir / manifest.sidecar.entry)) {
            result.code = err::kManifestInvalid;
            result.detail = "找不到侧车脚本 " + manifest.sidecar.entry;
            return result;
        }
    }
    if (manifest.sidecar.present && manifest.sidecar.runtime == "process" && unsafe_relative(manifest.sidecar.executable)) {
        result.code = err::kManifestInvalid;
        result.detail = "sidecar.executable 必须是插件目录内的相对路径";
        return result;
    }
    if (manifest.sidecar.present && manifest.sidecar.runtime == "process" &&
        !std::filesystem::exists(dir / manifest.sidecar.executable)) {
        // 侧车装不上时最常见的错就是「清单写了，exe 没跟着来」。
        result.code = err::kManifestInvalid;
        result.detail = "找不到侧车可执行文件 " + manifest.sidecar.executable;
        return result;
    }
    if (!manifest.sidecar.present && !has_artifact) {
        result.code = err::kManifestInvalid;
        result.detail = "缺少产物 " + artifact_name;
        return result;
    }

    // 依赖声明：写法在装载前就要拦住。写错一条的后果是它永远不满足，
    // 而用户只看到「装不上」，所以这里逐条解析，错在哪一条直接指出来。
    for (const auto& raw : manifest.requires_plugins) {
        Dependency dependency;
        if (!parse_requirement(raw, &dependency)) {
            result.code = err::kManifestInvalid;
            result.detail = "依赖写法不对：" + raw + "（应写成「插件名」或「插件名@主版本」）";
            return result;
        }
    }
    if (kMvpRejectRequires && !manifest.requires_plugins.empty()) {
        result.code = err::kManifestInvalid;
        result.detail = "本版不支持依赖声明（M3 起支持）";
        return result;
    }
    // 侧车的两个字段写错了不会在装载时报错，而是等到「该退出时不退出」才发现，
    // 所以在这里拦住。取值表见 04/02 §8.7。
    if (manifest.sidecar.present) {
        const std::string& policy = manifest.sidecar.restart;
        if (policy != "on-crash" && policy != "always" && policy != "never") {
            result.code = err::kManifestInvalid;
            result.detail = "sidecar.restart 只能是 on-crash / always / never，现在写的是 " + policy;
            return result;
        }
        if (manifest.sidecar.idle_timeout < 0) {
            result.code = err::kManifestInvalid;
            result.detail = "sidecar.idle_timeout 不能是负数";
            return result;
        }
    }
    if (kMvpRejectSidecar && manifest.sidecar.present) {
        result.code = err::kManifestInvalid;
        result.detail = "本版不支持侧车（M4 起支持）";
        return result;
    }

    // 第 10 步：能力取值。
    for (const auto& capability : manifest.capabilities) {
        const auto text = lower(capability);
        if (text != "fs" && text != "network" && text != "ui" && text != "shell") {
            result.code = err::kCapabilityUnknown;
            result.detail = "未知能力 " + capability;
            return result;
        }
    }

    // 第 11 步：[ui] 段（docs/ling/builtin-plugins/core-ui/04-详细设计/03 §1.2）。
    // 六步校验，顺序与那份文档一一对应；给插件作者看的同一套规则在
    // docs/ling/plugin-support/ui/01-面板声明与资源.md §1、§2，改一处必须同步另一处。
    if (manifest.ui.present) {
        auto decl_error = [&result](const std::string& detail) {
            result.code = err::kUiPanelDecl;
            result.detail = detail;
            return result;
        };

        // 1. 声明了 [ui] 就必须有 ui 能力。挡的是「复制清单时忘了改能力」，
        //    让错误在装载期暴露，而不是在打开面板时白屏。
        bool has_ui_capability = false;
        for (const auto& capability : manifest.capabilities) {
            if (lower(capability) == "ui") {
                has_ui_capability = true;
                break;
            }
        }
        if (!has_ui_capability) {
            return decl_error("声明了 [ui] 但缺少 ui 能力");
        }

        // 2. entry 与 title 必填且非空。
        if (manifest.ui.entry.empty()) {
            return decl_error("[ui] 缺少 entry");
        }
        if (manifest.ui.title.empty()) {
            return decl_error("[ui] 缺少 title");
        }

        // 3. 两个路径都不许绝对、不许穿越、不许带盘符。
        auto path_shape_ok = [](const std::string& text, const char* field) {
            if (text.empty()) {
                return true;  // icon 可空
            }
            if (text.find("..") != std::string::npos || text.front() == '/' ||
                text.front() == '\\' || text.find(':') != std::string::npos) {
                (void)field;
                return false;
            }
            return true;
        };
        if (!path_shape_ok(manifest.ui.entry, "entry") ||
            !path_shape_ok(manifest.ui.icon, "icon")) {
            return decl_error("[ui] 的路径必须是插件目录内的相对路径：" +
                              (path_shape_ok(manifest.ui.entry, "entry") ? manifest.ui.icon
                                                                         : manifest.ui.entry));
        }

        // 4. 解析后的真实路径必须落在 ui/ 内。先解析符号链接再比前缀：
        //    只比字符串挡不住「ui/ 里放一个指向 ../../ 的链接」。
        std::error_code ui_ec;
        const auto ui_root = std::filesystem::weakly_canonical(dir / "ui", ui_ec);
        auto inside_ui = [&](const std::string& relative) {
            if (relative.empty()) {
                return true;
            }
            std::error_code ignored;
            const auto target =
                std::filesystem::weakly_canonical(dir / relative, ignored);
            if (ignored || ui_root.empty()) {
                return false;  // 解析失败就无法证明目标在 ui/ 内，按越界拒绝
            }
            auto it_root = ui_root.begin();
            auto it_target = target.begin();
            for (; it_root != ui_root.end(); ++it_root, ++it_target) {
                if (it_target == target.end() || *it_root != *it_target) {
                    return false;
                }
            }
            return true;
        };
        if (!inside_ui(manifest.ui.entry)) {
            return decl_error("[ui].entry 必须位于 ui/ 目录下：" + manifest.ui.entry);
        }
        if (!inside_ui(manifest.ui.icon)) {
            return decl_error("[ui].icon 必须位于 ui/ 目录下：" + manifest.ui.icon);
        }

        // 5. 后缀。
        auto ends_with = [](const std::string& text, const std::string& tail) {
            return text.size() >= tail.size() &&
                   text.compare(text.size() - tail.size(), tail.size(), tail) == 0;
        };
        if (!ends_with(lower(manifest.ui.entry), ".html")) {
            return decl_error("[ui].entry 必须是 .html：" + manifest.ui.entry);
        }
        if (!manifest.ui.icon.empty() && !ends_with(lower(manifest.ui.icon), ".svg") &&
            !ends_with(lower(manifest.ui.icon), ".png")) {
            return decl_error("[ui].icon 只能是 .svg 或 .png：" + manifest.ui.icon);
        }

        // 6. title 按字符数算（UTF-8 里一个汉字是三个字节，不能按字节数卡）。
        std::size_t title_chars = 0;
        for (std::size_t i = 0; i < manifest.ui.title.size(); ++i) {
            if ((static_cast<unsigned char>(manifest.ui.title[i]) & 0xC0) != 0x80) {
                ++title_chars;
            }
        }
        if (title_chars < 1 || title_chars > 32) {
            return decl_error("[ui].title 长度要在 1 至 32 个字符之间");
        }
    }

    result.manifest = std::move(manifest);
    return result;
#else
    (void)host_version;
    result.code = err::kInternal;
    result.detail = "构建时未找到 toml.hpp";
    return result;
#endif
}

bool is_builtin_plugin(const std::string& id) {
    for (const char* builtin : kBuiltinIds) {
        if (id == builtin) {
            return true;
        }
    }
    return false;
}

}  // namespace ling
