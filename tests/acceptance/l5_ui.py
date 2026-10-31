"""M5 界面的验收：core-ui 作为侧车插件、页面与接口、可卸载、崩溃重启。

core-ui 现在是宿主自动预装并拉起的侧车插件，不再需要手动起一个独立进程。
这份脚本取代 ui_smoke.py（那个测的是旧的独立 ling-ui）。

用法：
    python tests/acceptance/l5_ui.py [--preset win-debug]
"""

import argparse
import json
import pathlib
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[2]

BUILTINS = ["core-settings", "core-plugin-manager", "core-log", "core-job", "core-ui"]


def http(url, method="GET", token=None, body=None):
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(url, data=data, method=method)
    if token:
        request.add_header("Authorization", "Bearer " + token)
    if data is not None:
        request.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            return response.status, response.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode("utf-8", "replace")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--preset", default="win-debug")
    args = parser.parse_args()

    exe = ROOT / "run" / args.preset / "ling.exe"
    artifacts = ROOT / "run" / args.preset / "plugins"
    data = pathlib.Path(tempfile.mkdtemp(prefix="ling-test-l5ui-"))

    results = []

    def check(name, ok, detail=""):
        results.append(ok)
        print(("通过  " if ok else "失败  ") + name + ("  " + detail if detail else ""))

    def ling(*extra):
        done = subprocess.run([str(exe), *extra, "--data-dir", str(data)],
                              capture_output=True, text=True, encoding="utf-8", errors="replace")
        return done.returncode, (done.stdout or "") + (done.stderr or "")

    def state_of(plugin_id):
        code, out = ling("plugins", "--json")
        if code != 0:
            return None
        for item in json.loads(out)["plugins"]:
            if item["id"] == plugin_id:
                return item
        return None

    host_log = (data / "stdout.log").open("w", encoding="utf-8")
    host = subprocess.Popen([str(exe), "serve", "--data-dir", str(data), "--port", "7900"],
                            stdout=host_log, stderr=subprocess.STDOUT)

    # 等 core-ui 起来：宿主先 load_all 再开端口，界面自检会等它，这里也等。
    token = None
    port = None
    for _ in range(30):
        try:
            token = json.loads((data / "session.json").read_text(encoding="utf-8"))["token"]
            registry = json.loads((data / "registry.json").read_text(encoding="utf-8"))
            if registry.get("ui_port"):
                port = registry["ui_port"]
                break
        except (FileNotFoundError, KeyError, json.JSONDecodeError):
            pass
        time.sleep(0.5)

    check("五个内置插件预装", all(state_of(bid) is not None for bid in BUILTINS),
          str([(bid, state_of(bid)["state"] if state_of(bid) else None) for bid in BUILTINS]))
    check("core-ui 是独立进程", state_of("core-ui") is not None
          and state_of("core-ui")["track"] == "sidecar", str(state_of("core-ui")))
    check("registry 里有 ui_port", port is not None, str(port))

    if not port or not token:
        print("\n界面没起来，跳过 HTTP 部分")
        subprocess.run([str(exe), "shutdown", "--data-dir", str(data)],
                       capture_output=True, text=True)
        host.wait(timeout=10)
        passed = sum(results)
        print(f"\n{passed}/{len(results)} 通过")
        return 1

    base = f"http://127.0.0.1:{port}"

    # 认证
    status, body = http(base + "/api/health")
    check("健康检查免认证", status == 200 and json.loads(body)["ok"], str(status))
    status, _ = http(base + "/")
    check("页面无口令回 401", status == 401, str(status))
    status, body = http(base + "/?token=" + token)
    check("页面带口令回 HTML", status == 200 and "<html" in body.lower(), str(status))
    check("页面里有五个视图的导航",
          all(word in body for word in ["插件", "日志", "配置", "任务", "设置"]), "")
    status, _ = http(base + "/api/plugins", "POST")
    check("接口无口令回 401", status == 401, str(status))

    # 插件列表
    status, body = http(base + "/api/plugins", "POST", token)
    ids = [item["id"] for item in json.loads(body)["result"]["plugins"]]
    check("插件列表含五个内置", status == 200 and all(bid in ids for bid in BUILTINS), str(ids))

    # 预览（只看不改）
    status, body = http(base + "/api/plan", "POST", token, {})
    check("预览接口可用", status == 200 and "steps" in json.loads(body)["result"], body[:80])

    # 装 / 停 / 启 / 卸 demo-a
    status, body = http(base + "/api/install", "POST", token, {"path": str(artifacts / "demo-a")})
    check("安装 demo-a", status == 200 and json.loads(body)["ok"], body[:100])
    check("demo-a 已启用", state_of("demo-a") is not None and state_of("demo-a")["state"] == "activated",
          str(state_of("demo-a")))
    status, body = http(base + "/api/disable", "POST", token, {"id": "demo-a"})
    check("停用 demo-a", status == 200 and state_of("demo-a")["state"] == "deactivated",
          str(state_of("demo-a")))
    status, body = http(base + "/api/enable", "POST", token, {"id": "demo-a"})
    check("启用 demo-a", status == 200 and state_of("demo-a")["state"] == "activated",
          str(state_of("demo-a")))

    # 配置
    status, body = http(base + "/api/config.set", "POST", token, {"key": "demo-x.timeout", "value": 30})
    check("配置写入", status == 200 and json.loads(body)["ok"], body[:80])
    status, body = http(base + "/api/config.get", "POST", token, {"key": "demo-x.timeout"})
    check("配置读回", status == 200 and json.loads(body)["result"].get("value") == 30, body[:80])
    status, body = http(base + "/api/config.list", "POST", token, {"prefix": "demo-x."})
    check("配置列表", json.loads(body)["result"]["keys"] == ["demo-x.timeout"],
          str(json.loads(body)["result"]["keys"]))
    status, body = http(base + "/api/config.set", "POST", token, {"key": "badkey", "value": 1})
    check("无插件名前缀的键被拒", status == 400 and not json.loads(body)["ok"], str(status))

    # 任务：校验 → 建 → 列 → 删
    status, body = http(base + "/api/jobs.validate", "POST", token, {"cron": "0 25 * * *"})
    check("非法时间表当场指出", status == 200 and json.loads(body)["result"].get("ok") is False
          and "时" in json.loads(body)["result"].get("reason", ""), body[:120])
    status, body = http(base + "/api/jobs.schedule", "POST", token,
                        {"name": "probe", "cron": "0 3 * * *", "method": "core-log.write",
                         "args": {"plugin": "core-job", "level": "info", "message": "from ui"}})
    check("建定时任务", status == 200 and json.loads(body)["ok"], body[:80])
    status, body = http(base + "/api/jobs.list", "POST", token)
    check("任务列表含新任务",
          "probe" in [item["name"] for item in json.loads(body)["result"]["jobs"]], body[:120])
    status, body = http(base + "/api/jobs.cancel", "POST", token, {"name": "probe"})
    check("取消任务", status == 200 and json.loads(body)["ok"], body[:80])

    # 孤儿扫描
    status, body = http(base + "/api/orphans", "POST", token, {})
    check("孤儿扫描接口可用", status == 200 and "orphans" in json.loads(body)["result"], body[:80])

    # 卸载 demo-a（连数据）
    status, body = http(base + "/api/uninstall", "POST", token, {"id": "demo-a", "purge": True})
    check("卸载 demo-a", status == 200 and state_of("demo-a") is None, str(state_of("demo-a")))

    # 界面崩溃后宿主不受影响、2 秒重启
    subprocess.run(["taskkill", "/IM", "core-ui.exe", "/F"],
                   capture_output=True, text=True, errors="replace")
    time.sleep(4)
    item = state_of("core-ui")
    check("界面崩溃后宿主还在、2 秒重启", item is not None and item["state"] == "activated",
          str(item))

    # doctor：四项全正常时退出码 0
    code, out = ling("doctor")
    check("doctor 四项正常退出码 0", code == 0 and "四项都正常" in out, out.strip()[-40:])

    # 卸载 core-ui 与 core-log，宿主还得能启动（LING-REQ-081）
    code, _ = ling("uninstall", "core-ui")
    check("卸载 core-ui", code == 0)
    code, _ = ling("uninstall", "core-log")
    check("卸载 core-log", code == 0)

    subprocess.run([str(exe), "shutdown", "--data-dir", str(data)],
                   capture_output=True, text=True)
    host.wait(timeout=15)

    # 再起宿主，只验它能响应即可；卸载了界面的宿主没有 ui_port。
    host2 = subprocess.Popen([str(exe), "serve", "--data-dir", str(data), "--port", "7900"],
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(3)
    code, out = ling("plugins", "--json")
    ids2 = [item["id"] for item in json.loads(out)["plugins"]] if code == 0 else []
    check("卸载 core-ui、core-log 后宿主仍能启动",
          code == 0 and "core-ui" not in ids2 and "core-log" not in ids2, str(ids2))
    subprocess.run([str(exe), "shutdown", "--data-dir", str(data)],
                   capture_output=True, text=True)
    host2.wait(timeout=15)

    passed = sum(results)
    print(f"\n{passed}/{len(results)} 通过")
    return 0 if passed == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())
