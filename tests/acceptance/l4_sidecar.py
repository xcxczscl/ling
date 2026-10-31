"""侧车轨道的验收：独立进程的启动、握手、代登记、调用往返、崩溃重启、停用。

对应 docs/ling/04-详细设计/02-插件装载与接口.md §8 与
docs/ling/04-详细设计/07-内置插件.md 里「两种轨道」的区别。

侧车是跨进程的，出问题时「没反应」可能是没起来、握手失败、管道读错、帧不合法。
所以这里按层验：进程在不在 → 有没有登记方法 → 调用能不能往返 → 崩了能不能回来。
"""

import argparse
import json
import pathlib
import subprocess
import sys
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--preset", default="win-debug")
    args = parser.parse_args()

    exe = ROOT / "run" / args.preset / "ling.exe"
    artifacts = ROOT / "run" / args.preset / "plugins"
    data = pathlib.Path(tempfile.mkdtemp(prefix="ling-test-sidecar-"))

    results = []

    def check(name, ok, detail=""):
        if isinstance(detail, list):
            detail = " ".join(detail)
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

    def read_text_safe(path):
        for _ in range(5):
            try:
                return path.read_text(encoding="utf-8", errors="replace") if path.exists() else ""
            except (PermissionError, OSError):
                time.sleep(0.2)
        return ""

    def sidecar_log():
        return read_text_safe(data / "plugins" / "demo-sidecar" / "data" / "demo-sidecar.log")

    def kernel_log():
        # 宿主一直在往这个文件写，Windows 上偶尔会碰上「别处正在写」，重试即可。
        logs = sorted((data / "logs").glob("kernel-*.log"))
        if not logs:
            return ""
        for _ in range(5):
            try:
                return logs[-1].read_text(encoding="utf-8", errors="replace")
            except (PermissionError, OSError):
                time.sleep(0.3)
        return ""

    def sidecar_alive():
        done = subprocess.run(["tasklist", "/FI", "IMAGENAME eq demo-sidecar.exe"],
                              capture_output=True, text=True, errors="replace")
        return "demo-sidecar.exe" in (done.stdout or "")

    def py_alive():
        """Python 侧车是 python 进程，用命令行里的脚本名认它，别把别的 python 算进来。

        wmic 在 Windows 11 已经移除，用 PowerShell 取命令行。
        """
        done = subprocess.run(
            ["powershell", "-NoProfile", "-Command",
             "(Get-CimInstance Win32_Process -Filter \"Name='python.exe'\").CommandLine"],
            capture_output=True, text=True, errors="replace")
        return "demo-py" in (done.stdout or "")

    host_log = (data / "stdout.log").open("w", encoding="utf-8")
    host = subprocess.Popen([str(exe), "serve", "--data-dir", str(data), "--port", "7900"],
                            stdout=host_log, stderr=subprocess.STDOUT)
    time.sleep(2)

    # 1. 装上就该是已启用，且轨道显示为独立进程
    code, out = ling("install", str(artifacts / "demo-sidecar"))
    check("装侧车插件", code == 0 and "现在可以用" in out,
          out.strip().splitlines()[0] if out.strip() else "")
    item = state_of("demo-sidecar")
    check("状态是已启用", item is not None and item["state"] == "activated", str(item))
    check("轨道显示为独立进程", item is not None and item.get("track") == "sidecar",
          str(item.get("track") if item else None))

    # 2. 进程真的起来了，宿主记了 pid
    check("侧车进程在", sidecar_alive())
    check("宿主记下了启动", "侧车已启动" in kernel_log())

    # 3. 代登记：宿主替它把方法挂进路由表
    check("宿主代登记方法", "登记方法 demo-sidecar.ping" in kernel_log())

    # 4. 调用往返：侧车在第一次心跳时自测，宿主调它、它应答、宿主再回给它
    deadline = time.time() + 10
    while time.time() < deadline and "自测调用成功" not in sidecar_log():
        time.sleep(0.3)
    log = sidecar_log()
    check("跨进程调用往返成功", "自测调用成功" in log and "pong from sidecar" in log,
          [line for line in log.splitlines() if "自测" in line][:1])
    check("宿主侧记下调用用时", "invoke #" in kernel_log() and "完成，用时" in kernel_log())

    # 5. 事件：侧车订阅了 kernel.tick，心跳一直往里送
    time.sleep(3)
    check("事件持续送达", "收到事件" in sidecar_log(),
          [line for line in sidecar_log().splitlines() if "收到事件" in line][-1:])

    # 6. 崩溃重启：强杀之后自己回来
    pids_before = kernel_log().count("侧车已启动")
    subprocess.run(["taskkill", "/IM", "demo-sidecar.exe", "/F"],
                   capture_output=True, text=True, errors="replace")
    time.sleep(4)   # 2 秒重启延迟 + 起来握手
    item = state_of("demo-sidecar")
    check("强杀之后重启并回到已启用",
          item is not None and item["state"] == "activated", str(item))
    check("宿主记了这一次重启", kernel_log().count("侧车已启动") > pids_before,
          f"before={pids_before} now={kernel_log().count('侧车已启动')}")

    # 7. 停用：发 deactivate，侧车自己退出，进程不在了
    code, out = ling("disable", "demo-sidecar")
    time.sleep(1.5)
    itail = sidecar_log()
    check("停用时侧车收到 deactivate 并退出", "收到 deactivate" in itail)
    check("停用后进程不在了", not sidecar_alive())
    item = state_of("demo-sidecar")
    check("状态是已停用", item is not None and item["state"] == "deactivated", str(item))

    # 8. 重新启用：再起一个新进程
    code, out = ling("enable", "demo-sidecar")
    time.sleep(2)
    item = state_of("demo-sidecar")
    check("重新启用后再起一个进程", item is not None and item["state"] == "activated"
          and sidecar_alive(), str(item))
    check("停着的时候轨道仍显示独立进程", item is not None and item.get("track") == "sidecar",
          str(item.get("track") if item else None))

    # 9. 按需启停：空闲超过 idle_timeout（示例里是 5 秒）后自己退出，状态仍是已启用
    time.sleep(8)
    item = state_of("demo-sidecar")
    check("空闲超时后进程退出", not sidecar_alive())
    check("退出后状态仍是已启用", item is not None and item["state"] == "activated", str(item))
    check("宿主记了空闲退出", "空闲" in kernel_log() and "已退出" in kernel_log(),
          [line for line in kernel_log().splitlines() if "空闲" in line][-1:])

    # 10. 冷启动：再来一次调用，进程应该被重新拉起
    #     定时任务最短一分钟，这里等一个分钟边界，顺带验证「按需」用的是真实路径。
    before = kernel_log().count("侧车已启动")
    code, out = ling("job", "add", "wake", "--cron", "* * * * *",
                     "--call", "demo-sidecar.ping")
    check("建一条每分钟的调用来唤醒它", code == 0, out.strip()[:60])
    deadline = time.time() + 70
    while time.time() < deadline and "按需拉起" not in kernel_log():
        time.sleep(1)
    check("被调用时重新拉起", "按需拉起" in kernel_log(),
          [line for line in kernel_log().splitlines() if "按需拉起" in line][-1:])
    check("确实又起了一个进程", kernel_log().count("侧车已启动") > before,
          f"before={before} now={kernel_log().count('侧车已启动')}")

    # 11. 双轨同时在线：C++ 侧车还在跑的情况下再起一个 Python 侧车
    code, out = ling("install", str(artifacts / "demo-py"))
    check("装 Python 侧车", code == 0 and "现在可以用" in out,
          out.strip().splitlines()[0] if out.strip() else "")
    item_py = state_of("demo-py")
    check("Python 侧车已启用且轨道正确",
          item_py is not None and item_py["state"] == "activated"
          and item_py.get("track") == "sidecar", str(item_py))
    time.sleep(2)
    check("两个侧车同时在跑", sidecar_alive() and py_alive(),
          f"c++={sidecar_alive()} py={py_alive()}")
    check("Python 侧车也走同一条协议接上了",
          "登记方法 demo-py.ping" in kernel_log())
    py_log_file = data / "plugins" / "demo-py" / "data" / "demo-py.log"
    py_log = read_text_safe(py_log_file)
    check("Python 侧车调用往返成功",
          "自测调用成功" in py_log and "pong from python" in py_log,
          [line for line in py_log.splitlines() if "自测" in line][:1])

    code, out = ling("uninstall", "demo-py")
    time.sleep(1.5)
    check("卸掉 Python 侧车", code == 0)
    check("Python 进程不在了", not py_alive())

    # 12. 卸载：进程收走，数据留着（没加 --purge）
    code, out = ling("uninstall", "demo-sidecar")
    time.sleep(1.5)
    check("卸载成功", code == 0, out.strip()[:50])
    check("卸载后进程不在了", not sidecar_alive())
    check("数据目录还在", (data / "plugins" / "demo-sidecar" / "data").exists())

    subprocess.run([str(exe), "shutdown", "--data-dir", str(data)],
                   capture_output=True, text=True)
    host.wait(timeout=10)

    passed = sum(1 for ok in results if ok)
    print(f"\n{passed}/{len(results)} 通过")
    return 0 if passed == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())
