// 平台层文件指纹的单元测试。判据来自
// docs/ling/main/04-详细设计/02-依赖解析与调和.md §7.2：
// 宿主主循环靠它判断「插件清单被改过没有」，所以改过就必须看得出来。
#include <catch_amalgamated.hpp>

#include <chrono>
#include <cstdint>
#include <filesystem>
#include <fstream>
#include <thread>

#include "ling/sys.hpp"

TEST_CASE("文件指纹能看出短时间内的一次改写", "[sys]") {
    // 这条用例盯的是一个具体的坑：MinGW 的 std::filesystem::last_write_time 把 Windows 的
    // FILETIME 折成纳秒时会溢出，分辨率退化到秒，300 毫秒内改写两次取回同一个值。真出了
    // 这种退化，宿主就会一直扫目录却永远认为「没变化」，「改清单后自动重算」随之失效。
    const auto dir = std::filesystem::temp_directory_path() / "ling-test-file-stamp";
    std::filesystem::remove_all(dir);
    std::filesystem::create_directories(dir);
    const auto file = dir / "plugin.toml";

    {
        std::ofstream out(file);
        out << "id = \"demo-a\"\n";
    }
    const std::uint64_t first = ling::sys::file_stamp(file);
    REQUIRE(first != 0);   // 0 是「取不到」，不能和真指纹混在一起

    std::this_thread::sleep_for(std::chrono::milliseconds(300));
    {
        std::ofstream out(file);
        out << "id = \"demo-b\"\n";   // 等长内容：只看大小区分不出来，必须靠时间戳
    }
    const std::uint64_t second = ling::sys::file_stamp(file);
    CHECK(second != first);

    // 取不到时给 0，调用方据此跳过这个条目（只剩 data/ 的目录算未安装）。
    CHECK(ling::sys::file_stamp(dir / "missing.toml") == 0);

    std::filesystem::remove_all(dir);
}
