#include "ling/manifest.hpp"

#include <algorithm>
#include <array>
#include <cctype>
#include <string_view>
#include <utility>

#include "ling/errors.hpp"

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
constexpr bool kMvpRejectRequires = true;
constexpr bool kMvpRejectSidecar = true;

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

// 内置插件清单。MVP 只有三个，core-ui 与 core-job 待 M5 加回（11 §4.2 第三条）。
constexpr const char* kBuiltinIds[] = {"core-settings", "core-plugin-manager", "core-log",
                                       "core-job"};

// 拆三段版本号，失败返回空。
std::array<int, 3> split_version(const std::string& text) {
    std::array<int, 3> parts{-1, -1, -1};
    int index = 0;
    int value = 0;
    bool digits = false;
    for (char ch : text) {
        if (ch >= '0' && ch <= '9') {
            value = value * 10 + (ch - '0');
            if (value > 65535) {
                return {-1, -1, -1};
            }
            digits = true;
            continue;
        }
        if (ch == '.' && index < 2 && digits) {
            parts[static_cast<std::size_t>(index)] = value;
            ++index;
            value = 0;
            digits = false;
            continue;
        }
        return {-1, -1, -1};
    }
    if (!digits || index != 2) {
        return {-1, -1, -1};
    }
    parts[2] = value;
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
        result.detail = "宿主支持 api 1，清单声明 " + manifest.api;
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
#ifdef _WIN32
    const std::string artifact_name = manifest.id + ".dll";
#else
    const std::string artifact_name = manifest.id + ".so";
#endif
    manifest.artifact = artifact_name;
    const bool has_artifact = std::filesystem::exists(dir / artifact_name);
    if (manifest.sidecar.present && has_artifact) {
        result.code = err::kManifestInvalid;
        result.detail = "动态库与 [sidecar] 段并存";
        return result;
    }
    if (manifest.sidecar.present && unsafe_relative(manifest.sidecar.executable)) {
        result.code = err::kManifestInvalid;
        result.detail = "sidecar.executable 必须是插件目录内的相对路径";
        return result;
    }
    if (!manifest.sidecar.present && !has_artifact) {
        result.code = err::kManifestInvalid;
        result.detail = "缺少产物 " + artifact_name;
        return result;
    }

    // MVP 阶段的两条临时限制。
    if (kMvpRejectRequires && !manifest.requires_plugins.empty()) {
        result.code = err::kManifestInvalid;
        result.detail = "本版不支持依赖声明（M3 起支持）";
        return result;
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
