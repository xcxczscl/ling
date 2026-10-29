// 命令行入口。S0 只支持 --version 与 --help，其余子命令在 S1 之后逐段加上
// （docs/ling/05-构造与开发指南.md §5.1）。
#include <cstdio>
#include <cstring>

#include "ling/version.hpp"

namespace {

void print_usage(FILE* out) {
    std::fprintf(out, "ling %s\n", ling::version_string());
    std::fprintf(out, "用法：ling <子命令> [选项]\n");
    std::fprintf(out, "  --version   打印版本\n");
    std::fprintf(out, "  --help      打印本说明\n");
}

}  // namespace

int main(int argc, char** argv) {
    for (int i = 1; i < argc; ++i) {
        if (std::strcmp(argv[i], "--version") == 0) {
            std::printf("ling %s\n", ling::version_string());
            return 0;
        }
        if (std::strcmp(argv[i], "--help") == 0) {
            print_usage(stdout);
            return 0;
        }
    }

    // 不带参数或未知参数：打印用法并以 2 退出，与「成功」区分开。
    print_usage(stderr);
    return 2;
}
