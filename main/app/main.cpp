// 命令行入口。S1 提供 install / uninstall / plugins 三个子命令，
// 其余（serve、enable、disable、logs、doctor）按 docs/ling/05-构造与开发指南.md §5.1
// 的切片顺序后续补上。
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <string>
#include <vector>

#include "ling/kernel.hpp"
#include "ling/version.hpp"

namespace {

void print_usage(std::FILE* out) {
    std::fprintf(out, "ling %s\n", ling::version_string());
    std::fprintf(out, "用法：ling <子命令> [选项] [--data-dir <路径>]\n");
    std::fprintf(out, "  install <目录>      安装插件\n");
    std::fprintf(out, "  uninstall <编号>    卸载插件，--purge 连 data/ 一起删\n");
    std::fprintf(out, "  plugins             列出插件与状态\n");
    std::fprintf(out, "  enable <编号>       启用插件\n");
    std::fprintf(out, "  disable <编号>      停用插件，下次启动不装载\n");
    std::fprintf(out, "  logs                打印当天内核日志的后 40 行\n");
    std::fprintf(out, "  --version           打印版本\n");
    std::fprintf(out, "  --help              打印本说明\n");
}

// 读环境变量。MSVC 把 getenv 标为不安全（C4996），而 /WX 会把它当错误，
// 所以在 Windows 上用 _dupenv_s，其它平台仍用 getenv。
std::string read_env(const char* name) {
#ifdef _WIN32
    char* value = nullptr;
    size_t length = 0;
    if (_dupenv_s(&value, &length, name) != 0 || value == nullptr) {
        return {};
    }
    std::string result(value);
    std::free(value);
    return result;
#else
    const char* value = std::getenv(name);
    return value == nullptr ? std::string() : std::string(value);
#endif
}

std::filesystem::path default_data_root() {
#ifdef _WIN32
    const std::string appdata = read_env("APPDATA");
    if (!appdata.empty()) {
        return std::filesystem::path(appdata) / "ling";
    }
    return std::filesystem::path(".") / "data";
#else
    const std::string home = read_env("HOME");
    if (!home.empty()) {
        return std::filesystem::path(home) / ".local" / "share" / "ling";
    }
    return std::filesystem::path(".") / "data";
#endif
}

struct Args {
    std::string command;
    std::vector<std::string> positional;
    std::filesystem::path data_dir;
    bool purge = false;
};

}  // namespace

int main(int argc, char** argv) {
    Args args;
    args.data_dir = default_data_root();

    for (int i = 1; i < argc; ++i) {
        const std::string token = argv[i];
        if (token == "--version") {
            std::printf("ling %s\n", ling::version_string());
            return 0;
        }
        if (token == "--help") {
            print_usage(stdout);
            return 0;
        }
        if (token == "--data-dir" && i + 1 < argc) {
            args.data_dir = argv[++i];
            continue;
        }
        if (token == "--purge") {
            args.purge = true;
            continue;
        }
        if (args.command.empty()) {
            args.command = token;
            continue;
        }
        args.positional.push_back(token);
    }

    if (args.command.empty()) {
        print_usage(stderr);
        return 2;
    }

    if (args.command == "install" && args.positional.size() == 1) {
        ling::Kernel kernel(args.data_dir);
        if (kernel.load_all() != 0) {
            std::fprintf(stderr, "提示：部分已装插件激活失败，见日志\n");
        }
        const std::string code = kernel.install(args.positional[0]);
        if (!code.empty()) {
            std::fprintf(stderr, "安装失败：%s\n", code.c_str());
            return 1;
        }
        std::printf("已安装 %s\n", args.positional[0].c_str());
        return 0;
    }

    if (args.command == "uninstall" && args.positional.size() == 1) {
        ling::Kernel kernel(args.data_dir);
        const std::string code = kernel.uninstall(args.positional[0], args.purge);
        if (!code.empty()) {
            std::fprintf(stderr, "卸载失败：%s\n", code.c_str());
            return 1;
        }
        std::printf("已卸载 %s%s\n", args.positional[0].c_str(),
                    args.purge ? "（含数据）" : "（保留数据）");
        return 0;
    }

    if ((args.command == "enable" || args.command == "disable") && args.positional.size() == 1) {
        ling::Kernel kernel(args.data_dir);
        const std::string code =
            kernel.set_enabled(args.positional[0], args.command == "enable");
        if (!code.empty()) {
            std::fprintf(stderr, "操作失败：%s\n", code.c_str());
            return 1;
        }
        std::printf("%s %s\n", args.command == "enable" ? "已启用" : "已停用",
                    args.positional[0].c_str());
        return 0;
    }

    if (args.command == "logs") {
        ling::Kernel kernel(args.data_dir);  // 构造时会初始化日志目录
        const std::filesystem::path log_dir = args.data_dir / "logs";
        std::filesystem::path latest;
        std::error_code ec;
        // 日志按天分文件，文件名里的日期可字典序比较，取最大的那个即最新。
        for (const auto& entry : std::filesystem::directory_iterator(log_dir, ec)) {
            if (!entry.is_regular_file()) {
                continue;
            }
            const std::string name = entry.path().filename().string();
            if (name.rfind("kernel-", 0) != 0) {
                continue;
            }
            if (latest.empty() || name > latest.filename().string()) {
                latest = entry.path();
            }
        }
        if (latest.empty()) {
            std::printf("还没有日志\n");
            return 0;
        }
        std::ifstream input(latest);
        std::vector<std::string> lines;
        std::string line;
        while (std::getline(input, line)) {
            lines.push_back(line);
        }
        const std::size_t begin = lines.size() > 40 ? lines.size() - 40 : 0;
        for (std::size_t i = begin; i < lines.size(); ++i) {
            std::printf("%s\n", lines[i].c_str());
        }
        return 0;
    }

    if (args.command == "plugins") {
        ling::Kernel kernel(args.data_dir);
        kernel.load_all();
        const auto& plugins = kernel.plugins();
        std::size_t printed = 0;
        // 已停用的插件不会进装载表，但它们确实装着，所以单独列一遍。
        for (const auto& plugin : plugins) {
            std::printf("%-24s %-10s %s\n", plugin.manifest.id.c_str(),
                        plugin.manifest.version.c_str(), ling::state_name(plugin.state));
            ++printed;
        }
        std::error_code ec;
        for (const auto& entry : std::filesystem::directory_iterator(kernel.plugins_dir(), ec)) {
            if (!entry.is_directory()) {
                continue;
            }
            const std::string id = entry.path().filename().string();
            if (!kernel.is_disabled(id)) {
                continue;
            }
            if (!std::filesystem::exists(entry.path() / "plugin.toml")) {
                continue;
            }
            std::printf("%-24s %-10s %s\n", id.c_str(), "-", "disabled");
            ++printed;
        }
        if (printed == 0) {
            std::printf("没有已安装的插件\n");
        }
        return 0;
    }

    print_usage(stderr);
    return 2;
}
