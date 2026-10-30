// 调用延迟基准插件。LING-REQ-089 要求 P95 ≤ 1 微秒、P99 ≤ 3 微秒（10 万次）。
//
// 它与 demo-caller 分开：demo-caller 是「调通一次」的演示，这个只负责计时，
// 激活时跑一轮并把结果写进自己的 data 与 stdout，供 l1_bench_call 采集。
#include <algorithm>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

#include "ling/plugin_api.h"

namespace {

constexpr int kRounds = 100000;

struct State {
    const ling_host_api* host = nullptr;
    void* ctx = nullptr;
};

State g;

void write_line(const std::string& text) {
    const char* dir = g.host->data_dir(g.ctx);
    if (dir == nullptr) {
        return;
    }
    std::string path(dir);
    path += "/bench.log";
    std::FILE* file = nullptr;
#ifdef _WIN32
    if (fopen_s(&file, path.c_str(), "ab") != 0) {
        file = nullptr;
    }
#else
    file = std::fopen(path.c_str(), "ab");
#endif
    if (file == nullptr) {
        return;
    }
    std::fputs(text.c_str(), file);
    std::fputc('\n', file);
    std::fclose(file);
    std::printf("[demo-bench] %s\n", text.c_str());
}

double percentile(const std::vector<double>& samples, double ratio) {
    if (samples.empty()) {
        return 0.0;
    }
    const auto index = static_cast<std::size_t>(ratio * static_cast<double>(samples.size() - 1));
    return samples[index];
}

}  // namespace

LING_PLUGIN_EXPORT int32_t ling_plugin_entry(const ling_host_api* host,
                                             ling_plugin_descriptor* out_desc,
                                             void** out_plugin) {
    if (host == nullptr || out_desc == nullptr || out_plugin == nullptr) {
        return 1;
    }

    void* ctx = host->context_alloc("demo-bench", 0);
    if (ctx == nullptr) {
        return 2;
    }
    g.host = host;
    g.ctx = ctx;

    // 先热身，避免第一次调用的冷启动算进分位数。
    for (int i = 0; i < 1000; ++i) {
        uint8_t* out = nullptr;
        size_t out_len = 0;
        host->call(ctx, "demo-a.ping", nullptr, 0, &out, &out_len);
        if (out != nullptr) {
            host->free_buffer(ctx, out, out_len);
        }
    }

    std::vector<double> samples;
    samples.reserve(kRounds);
    for (int i = 0; i < kRounds; ++i) {
        uint8_t* out = nullptr;
        size_t out_len = 0;
        const auto started = std::chrono::steady_clock::now();
        host->call(ctx, "demo-a.ping", nullptr, 0, &out, &out_len);
        const auto cost = std::chrono::duration<double, std::micro>(
            std::chrono::steady_clock::now() - started);
        samples.push_back(cost.count());
        if (out != nullptr) {
            host->free_buffer(ctx, out, out_len);
        }
    }
    std::sort(samples.begin(), samples.end());

    const double p50 = percentile(samples, 0.50);
    const double p95 = percentile(samples, 0.95);
    const double p99 = percentile(samples, 0.99);
    char buffer[160] = {};
    std::snprintf(buffer, sizeof(buffer), "p50=%.3fus p95=%.3fus p99=%.3fus rounds=%d", p50, p95,
                  p99, kRounds);
    write_line(buffer);

    out_desc->abi_version = LING_ABI_VERSION;
    out_desc->plugin_id = "demo-bench";
    out_desc->plugin_version = "1.0.0";
    out_desc->api = "1";
    *out_plugin = ctx;
    return 0;
}
