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
        # 判定写成 `a and b` 时结果可能是非布尔值（例如版本号字符串），这里收成 bool，
        # 否则最后 sum(results) 会撞上「int + str」。
        results.append(bool(ok))
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

    # 认证：免认证的只有「GET 一个文件或一段常量」，有副作用的一律要 token
    # （docs/ling/builtin-plugins/core-ui/04-详细设计/01-服务端与接口.md §2.4）。
    status, body = http(base + "/api/ui/theme.css")
    check("theme.css 免认证", status == 200 and "--ling-bg:" in body, str(status))
    status, body = http(base + "/")
    check("外壳页免认证（口令在 URL 里才带进来）", status == 200 and "<html" in body.lower(),
          str(status))
    check("外壳页不内嵌插件数据",
          "core-settings" not in body and "core-log" not in body, "")
    check("外壳页有面板区与设置区",
          'id="panel-area"' in body and 'id="settings-tabs"' in body and 'id="nav-views"' in body,
          "")
    status, _ = http(base + "/api/panels", "POST")
    check("接口无口令回 401", status == 401, str(status))
    status, _ = http(base + "/api/panels", "GET", token)
    check("接口只认 POST", status == 405, str(status))

    # 面板资源五道检查：越界的几种写法都要落在 404，正常文件要读得出来。
    # 先装一个带 [ui] 的测试插件，面板才有东西可指。
    status, body = http(base + "/api/install", "POST", token,
                        {"path": str(artifacts / "ui-demo")})
    check("安装 ui-demo", status == 200 and json.loads(body)["ok"], body[:100])
    status, body = http(base + "/panel/ui-demo/index.html")
    check("面板文件可读", status == 200 and "<html" in body.lower(), str(status))
    status, _ = http(base + "/panel/ui-demo/../../registry.json")
    check("面板路径越界被拦", status == 404, str(status))
    status, _ = http(base + "/panel/ui-demo/%2e%2e%2fregistry.json")
    check("编码过的越界也被拦", status == 404, str(status))
    status, _ = http(base + "/panel/nope/index.html")
    check("不存在的面板回 404", status == 404, str(status))

    # 插件列表
    status, body = http(base + "/api/plugins", "POST", token)
    ids = [item["id"] for item in json.loads(body)["result"]]
    check("插件列表含五个内置", status == 200 and all(bid in ids for bid in BUILTINS), str(ids))

    # 面板列表：只列带 [ui] 且已装载的
    status, body = http(base + "/api/panels", "POST", token)
    panels = json.loads(body)["result"]
    check("面板列表只含 ui-demo",
          status == 200 and [item["id"] for item in panels] == ["ui-demo"], str(panels))
    check("面板条目带入口与状态",
          bool(panels) and panels[0]["entry"] == "/panel/ui-demo/index.html"
          and panels[0]["state"] == "running", str(panels[:1]))

    # 面板调用的两道闸：命名空间越界、面板不存在的插件
    status, body = http(base + "/api/panel/call", "POST", token,
                        {"plugin": "ui-demo", "method": "ui-demo.echo", "params": {"a": 1}})
    check("面板调用通", status == 200 and json.loads(body)["result"]["pong"] is True, body[:80])
    status, body = http(base + "/api/panel/call", "POST", token,
                        {"plugin": "ui-demo", "method": "kernel.plugins"})
    check("面板不能调别人的方法",
          status == 403 and json.loads(body)["error"]["code"] == "E_UI_FORBIDDEN", body[:100])

    # 停用之后面板从列表里消失，调用也调不动
    status, _ = http(base + "/api/disable", "POST", token, {"id": "ui-demo"})
    status, body = http(base + "/api/panels", "POST", token)
    check("停用后面板标成 stopped",
          json.loads(body)["result"][0]["state"] == "stopped", body[:100])
    status, body = http(base + "/api/panel/call", "POST", token,
                        {"plugin": "ui-demo", "method": "ui-demo.echo"})
    check("停用后调不动", status == 403, str(status))
    http(base + "/api/enable", "POST", token, {"id": "ui-demo"})

    # 关于页的数据
    status, body = http(base + "/api/meta", "POST", token)
    meta = json.loads(body)["result"]
    check("meta 带宿主与界面版本", status == 200 and meta["hostVersion"] and meta["uiVersion"],
          str(meta))

    # 配置
    status, body = http(base + "/api/config.set", "POST", token, {"key": "ui-demo.timeout", "value": 30})
    check("配置写入", status == 200 and json.loads(body)["result"]["saved"] is True, body[:80])
    status, body = http(base + "/api/config.get", "POST", token, {"key": "ui-demo.timeout"})
    check("配置读回", status == 200 and json.loads(body)["result"].get("value") == 30, body[:80])
    status, body = http(base + "/api/config.list", "POST", token, {"prefix": "ui-demo."})
    keys = [item["key"] for item in json.loads(body)["result"]["items"]]
    check("配置列表", keys == ["ui-demo.timeout"], str(keys))
    status, body = http(base + "/api/config.set", "POST", token, {"key": "badkey", "value": 1})
    check("无插件名前缀的键被拒", status == 400 and not json.loads(body)["ok"], str(status))

    # 任务：校验 → 建 → 列 → 删
    status, body = http(base + "/api/jobs.validate", "POST", token, {"cron": "0 25 * * *"})
    check("非法时间表当场指出", status == 200 and json.loads(body)["result"].get("ok") is False
          and "时" in json.loads(body)["result"].get("reason", ""), body[:120])
    status, body = http(base + "/api/jobs.schedule", "POST", token,
                        {"name": "probe", "cron": "0 3 * * *", "method": "core-log.write",
                         "params": {"plugin": "core-job", "level": "info", "message": "from ui"}})
    check("建定时任务", status == 200 and json.loads(body)["ok"], body[:80])
    status, body = http(base + "/api/jobs.list", "POST", token)
    check("任务列表含新任务",
          "probe" in [item["name"] for item in json.loads(body)["result"]["jobs"]], body[:120])
    status, body = http(base + "/api/jobs.cancel", "POST", token, {"name": "probe"})
    check("取消任务", status == 200 and json.loads(body)["ok"], body[:80])

    # 日志：宿主自己的与某个插件的，都是 {lines:[{ts,level,text}]}
    status, body = http(base + "/api/logs", "POST", token, {"lines": 5})
    lines = json.loads(body)["result"]["lines"]
    check("宿主日志可读", status == 200 and isinstance(lines, list), body[:80])
    check("日志行切成三段", not lines or all(
        name in lines[-1] for name in ("ts", "level", "text")), str(lines[-1:]))

    # 孤儿扫描
    status, body = http(base + "/api/orphans", "POST", token, {})
    check("孤儿扫描接口可用", status == 200 and isinstance(json.loads(body)["result"], list),
          body[:80])

    # 卸载 ui-demo（连数据）
    status, body = http(base + "/api/uninstall", "POST", token, {"id": "ui-demo", "purge": True})
    check("卸载 ui-demo", status == 200 and state_of("ui-demo") is None, str(state_of("ui-demo")))

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
