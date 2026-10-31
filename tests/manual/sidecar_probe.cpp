// 侧车管道层的手工探针：直接驱动 Sidecar，不经过内核。
//
// 用途：把「握手、代登记、调用」这条链路从内核里剥出来单独跑，
// 这样出问题时就分得清是管道层还是内核接线的问题。
//
// 用法：
//   ling_sidecar_probe <侧车 exe> <数据目录>
#include <cstdio>
#include <string>

#include "ling/sidecar_host.hpp"
#include "ling/sidecar_proto.hpp"

namespace {

int g_failures = 0;

void check(bool ok, const std::string& what) {
    std::printf("%s %s\n", ok ? "[ok]  " : "[FAIL]", what.c_str());
    if (!ok) {
        ++g_failures;
    }
}

}  // namespace

int main(int argc, char** argv) {
    if (argc < 3) {
        std::fprintf(stderr, "用法：ling_sidecar_probe <侧车 exe> <数据目录>\n");
        return 2;
    }
    const std::string exe = argv[1];
    const std::string data_dir = argv[2];

    ling::Sidecar sidecar;
    std::string error = sidecar.start(exe, "", "", "demo-sidecar", "probe-token", data_dir, "");
    if (!error.empty()) {
        std::printf("[FAIL] 起进程：%s\n", error.c_str());
        return 1;
    }
    std::printf("[ok]   进程已起，pid=%ld\n", sidecar.pid());

    bool closed = false;
    ling::sidecar::Frame frame;

    // 1. hello
    check(sidecar.read(&frame, 3000, &closed, &error) && frame.op == "hello",
          "读到 hello（closed=" + std::string(closed ? "1" : "0") + " error=" + error + "）");
    check(frame.fields.value("token", "") == "probe-token", "hello 带回口令");

    // 2. activate → register_method / subscribe
    ling::sidecar::Frame activate;
    activate.op = "activate";
    activate.fields = nlohmann::json::object();
    check(sidecar.send(activate, &error), "发出 activate");

    int registered = 0;
    int subscribed = 0;
    for (int i = 0; i < 4; ++i) {
        if (!sidecar.read(&frame, 3000, &closed, &error)) {
            break;
        }
        if (frame.op == "register_method") {
            ++registered;
        }
        if (frame.op == "subscribe") {
            ++subscribed;
        }
    }
    check(registered == 1, "收到一条 register_method");
    check(subscribed == 1, "收到一条 subscribe");

    // 3. invoke → result
    ling::sidecar::Frame invoke;
    invoke.id = 7;
    invoke.op = "invoke";
    invoke.fields = {{"method", "demo-sidecar.ping"}, {"args", nlohmann::json::object()}};
    check(sidecar.send(invoke, &error), "发出 invoke id=7");

    const bool got = sidecar.read(&frame, 5000, &closed, &error);
    check(got && frame.op == "result" && frame.id == 7,
          "读到 result（op=" + frame.op + " id=" + std::to_string(frame.id) +
              " closed=" + (closed ? "1" : "0") + " error=" + error + "）");
    if (got && frame.op == "result") {
        std::printf("       result = %s\n", frame.fields["result"].dump().c_str());
    }

    // 4. 再发一次，确认不是一次性的
    invoke.id = 8;
    check(sidecar.send(invoke, &error), "再发一次 invoke id=8");
    const bool got2 = sidecar.read(&frame, 5000, &closed, &error);
    check(got2 && frame.id == 8, "第二次也收到 result（id=" + std::to_string(frame.id) + "）");

    sidecar.terminate(2000);
    std::printf("%s\n", g_failures == 0 ? "全部通过" : "有失败项");
    return g_failures == 0 ? 0 : 1;
}
