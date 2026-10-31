"""界面进程（core-ui 服务端）的冒烟测试。

覆盖 docs/ling/builtin-plugins/core-ui/02-服务端与接口.md 里的三块：
认证、/api/* 转发、失败处理。页面还没做，只验服务端。

用法：
    python tests/acceptance/ui_smoke.py [--preset win-debug]
"""

import argparse
import json
import pathlib
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[2]


def admin_frame(port, token, method, args):
    """直接对管理端口发一帧，用来验证 kernel.call 的白名单。"""
    with socket.create_connection(("127.0.0.1", port), timeout=5) as sock:
        handle = sock.makefile("rwb")
        frame = {"v": 1, "id": 0, "op": "auth", "token": token}
        handle.write((json.dumps(frame) + "\n").encode())
        handle.flush()
        handle.readline()
        call = {"v": 1, "id": 1, "op": "call", "method": "kernel.call",
                "args": {"method": method, "args": args}}
        handle.write((json.dumps(call) + "\n").encode())
        handle.flush()
        return json.loads(handle.readline().decode())


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
    ui = ROOT / "run" / args.preset / "ling-ui.exe"
    # 系统临时目录：每次从空白开始，也不碰仓库里的目录。
    data = pathlib.Path(tempfile.mkdtemp(prefix="ling-test-ui-smoke-"))
    if not ui.exists():
        print(f"找不到 {ui}，先构建 {args.preset}")
        return 1
    # mkdtemp 已经建好了

    results = []

    def check(name, ok, detail=""):
        results.append((name, ok))
        print(("通过  " if ok else "失败  ") + name + ("  " + detail if detail else ""))

    host = subprocess.Popen([str(exe), "serve", "--data-dir", str(data), "--port", "7900"],
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
                            encoding="utf-8", errors="replace")
    time.sleep(2)
    # 单独一个进程组，才能给它发 CTRL_BREAK 走优雅退出（与按 Ctrl+C 同一条路径）。
    ui_proc = subprocess.Popen([str(ui), "--data-dir", str(data)],
                               stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
                               encoding="utf-8", errors="replace",
                               creationflags=subprocess.CREATE_NEW_PROCESS_GROUP)
    time.sleep(2)

    check("界面进程存活", ui_proc.poll() is None,
          "" if ui_proc.poll() is None else (ui_proc.stdout.read() or ""))

    registry = json.loads((data / "registry.json").read_text(encoding="utf-8"))
    token = json.loads((data / "session.json").read_text(encoding="utf-8"))["token"]
    port = registry.get("ui_port")
    if not port:
        check("registry.json 里有 ui_port", False)
        ui_proc.kill()
        host.kill()
        return 1
    check("registry.json 里有 ui_port", True, str(port))
    base = f"http://127.0.0.1:{port}"

    status, body = http(base + "/api/health")
    check("健康检查免认证", status == 200 and json.loads(body)["ok"], str(status))

    status, _ = http(base + "/")
    check("页面无口令回 401", status == 401, str(status))

    status, body = http(base + "/?token=" + token)
    check("页面带口令回 HTML", status == 200 and "<html" in body.lower(), str(status))

    status, _ = http(base + "/api/plugins", "POST")
    check("接口无口令回 401", status == 401, str(status))

    status, body = http(base + "/api/plugins", "POST", token)
    parsed = json.loads(body)
    ids = [item["id"] for item in parsed["result"]["plugins"]]
    check("插件列表", status == 200 and "core-job" in ids, str(ids))

    status, body = http(base + "/api/config.set", "POST", token,
                        {"key": "demo-x.timeout", "value": 30})
    check("配置写入", status == 200 and json.loads(body)["ok"], body[:100])

    status, body = http(base + "/api/config.get", "POST", token, {"key": "demo-x.timeout"})
    check("配置读回", status == 200 and json.loads(body)["result"].get("value") == 30, body[:100])

    status, body = http(base + "/api/config.list", "POST", token, {"prefix": "demo-x."})
    keys = json.loads(body)["result"]["keys"] if status == 200 else []
    check("配置列表", keys == ["demo-x.timeout"], str(keys))

    status, body = http(base + "/api/config.set", "POST", token, {"key": "badkey", "value": 1})
    check("无插件名前缀的键被拒", status == 400 and not json.loads(body)["ok"], str(status))

    status, body = http(base + "/api/jobs.schedule", "POST", token,
                        {"name": "probe", "cron": "* * * * *", "method": "core-log.write",
                         "args": {"plugin": "core-job", "level": "info", "message": "from ui"},
                         "catch_up": False})
    check("建定时任务", status == 200 and json.loads(body)["ok"], body[:100])

    status, body = http(base + "/api/jobs.list", "POST", token)
    names = [item["name"] for item in json.loads(body)["result"]["jobs"]] if status == 200 else []
    check("任务列表含新任务", "probe" in names, str(names))

    status, body = http(base + "/api/jobs.cancel", "POST", token, {"name": "probe"})
    check("取消任务", status == 200 and json.loads(body)["ok"], body[:100])

    status, _ = http(base + "/api/nope", "POST", token, {})
    check("未知接口回 404", status == 404, str(status))

    status, _ = http(base + "/api/plugins", "GET", token)
    check("GET 回 405", status == 405, str(status))

    reply = admin_frame(registry["port"], token, "demo-a.ping", {})
    check("业务插件方法不被 kernel.call 代理",
          reply.get("ok") is False and reply["error"]["code"] == "E_NAMESPACE_VIOLATION",
          str(reply)[:100])

    reply = admin_frame(registry["port"], token, "core-settings.list", {"prefix": ""})
    check("白名单内的方法可代理", reply.get("ok") is True, str(reply)[:100])

    ui_proc.send_signal(signal.CTRL_BREAK_EVENT)
    try:
        ui_proc.wait(timeout=10)
        check("收到 Ctrl+Break 后自行退出", True)
    except subprocess.TimeoutExpired:
        ui_proc.kill()
        ui_proc.wait(timeout=5)
        check("收到 Ctrl+Break 后自行退出", False)

    subprocess.run([str(exe), "shutdown", "--data-dir", str(data)],
                   capture_output=True, text=True)
    host.wait(timeout=10)

    after = json.loads((data / "registry.json").read_text(encoding="utf-8"))
    check("退出后清掉 ui_port", "ui_port" not in after, str(after))

    passed = sum(1 for _, ok in results if ok)
    print(f"\n{passed}/{len(results)} 通过")
    return 0 if passed == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())
