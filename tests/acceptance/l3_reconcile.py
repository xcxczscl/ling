"""依赖调和的验收：先装依赖方、后装被依赖方，前者自动启用。

对应 docs/ling/04-详细设计/04-依赖解析与调和.md §5 与
docs/ling/11-MVP 范围.md 的 M3 判定（`10-路线图与里程碑.md` §3.4）。
"""

import argparse
import json
import pathlib
import shutil
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--preset", default="win-debug")
    args = parser.parse_args()

    exe = ROOT / "run" / args.preset / "ling.exe"
    artifacts = ROOT / "run" / args.preset / "plugins"
    data = ROOT / "run" / args.preset / "data-test" / "reconcile"
    if data.exists():
        shutil.rmtree(data)
    data.mkdir(parents=True)

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

    # 输出写文件而不是管道：管道没人读时，宿主写日志可能阻塞在写入上，
    # 于是「让它退出」这一步会等不到进程结束。写文件没有这个坑。
    host_log = (data / "stdout.log").open("w", encoding="utf-8")
    host = subprocess.Popen([str(exe), "serve", "--data-dir", str(data), "--port", "7900"],
                            stdout=host_log, stderr=subprocess.STDOUT)
    time.sleep(2)

    code, out = ling("install", str(artifacts / "demo-b"))
    check("装依赖方：提示还说不了话", code == 0 and "用不了" in out and "demo-a" in out,
          out.strip().splitlines()[0] if out.strip() else "")

    item = state_of("demo-b")
    check("装上了但状态是已停用", item is not None and item["state"] == "deactivated",
          str(item))
    check("列表里写明缺什么", item is not None and "demo-a" in item.get("reason", ""),
          str(item.get("reason") if item else None))

    code, out = ling("install", str(artifacts / "demo-a"))
    check("装被依赖方时提示可用", code == 0 and "现在可以用" in out, out.strip()[:60])

    item_a = state_of("demo-a")
    item_b = state_of("demo-b")
    check("两个都已启用", item_a["state"] == "activated" and item_b["state"] == "activated",
          f"a={item_a['state']} b={item_b['state']}")
    check("原因已清掉", "reason" not in item_b, str(item_b.get("reason")))

    code, out = ling("uninstall", "demo-a")
    check("卸掉被依赖方", code == 0, out.strip()[:50])

    item_b = state_of("demo-b")
    check("依赖方跟着停用并给出原因",
          item_b["state"] == "deactivated" and "demo-a" in item_b.get("reason", ""),
          str(item_b))

    code, out = ling("install", str(artifacts / "demo-a"))
    item_b = state_of("demo-b")
    check("装回来之后自动启用，无需重启", code == 0 and item_b["state"] == "activated",
          str(item_b.get("state")))

    subprocess.run([str(exe), "shutdown", "--data-dir", str(data)], capture_output=True, text=True)
    host.wait(timeout=10)

    passed = sum(1 for ok in results if ok)
    print(f"\n{passed}/{len(results)} 通过")
    return 0 if passed == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())
