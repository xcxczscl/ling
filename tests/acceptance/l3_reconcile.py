"""依赖调和的验收：先装依赖方、后装被依赖方，前者自动启用。

对应 docs/ling/04-详细设计/04-依赖解析与调和.md §5 与
docs/ling/11-MVP 范围.md 的 M3 判定（`10-路线图与里程碑.md` §3.4）。

用 acceptance.py 的 Harness：独立临时数据根、独立端口，宿主输出落文件。
"""

import argparse
import json
import pathlib
import socket
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

from acceptance import Harness


def free_port():
    """挑一个空闲端口，避免和别的验收脚本或本机服务撞端口。"""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--preset", default="win-debug")
    args = parser.parse_args()

    h = Harness("l3_reconcile", free_port(), args.preset)

    def state_of(plugin_id):
        code, out, err = h.cli("plugins", "--json")
        if code != 0:
            return None
        for item in json.loads(out)["plugins"]:
            if item["id"] == plugin_id:
                return item
        return None

    def wait_state(plugin_id, want, limit=6.0):
        """等状态变成 want 再断言。

        调和是「200 毫秒合并窗口 + 一轮轮询」才会发生的，固定 sleep 1.5 秒在机器忙的时候
        会赶在调和之前读，读到的还是旧状态——那是脚本的问题，不是调和的问题。
        """
        deadline = time.time() + limit
        item = None
        while time.time() < deadline:
            item = state_of(plugin_id)
            if item is not None and item.get("state") == want:
                return item
            time.sleep(0.2)
        return item

    def edit_manifest(plugin_id, text):
        """直接改已安装插件目录里的清单，模拟「换了依赖声明」。"""
        (h.data_root / "plugins" / plugin_id / "plugin.toml").write_text(text, encoding="utf-8")

    if not h.start():
        print("宿主启动失败，看 " + str(h.data_root / "serve.log"))
        h.stop()
        return 2
    try:
        code, out, err = h.cli("install", str(h.artifacts / "demo-b"))
        h.check("装依赖方：提示还说不了话",
                code == 0 and "用不了" in out and "demo-a" in out,
                out.strip().splitlines()[0] if out.strip() else "")

        item = wait_state("demo-b", "deactivated")
        h.check("装上了但状态是已停用", item is not None and item["state"] == "deactivated",
                str(item))
        h.check("列表里写明缺什么", item is not None and "demo-a" in item.get("reason", ""),
                str(item.get("reason") if item else None))

        code, out, err = h.cli("install", str(h.artifacts / "demo-a"))
        h.check("装被依赖方时提示可用", code == 0 and "现在可以用" in out, out.strip()[:60])

        item_a = wait_state("demo-a", "activated")
        item_b = wait_state("demo-b", "activated")
        h.check("两个都已启用",
                item_a is not None and item_a["state"] == "activated" and item_b is not None
                and item_b["state"] == "activated",
                f"a={item_a} b={item_b}")
        h.check("原因已清掉", item_b is not None and "reason" not in item_b,
                str(item_b.get("reason") if item_b else None))

        # 停用 vs 卸载：停用是「装着但不用」，依赖它的也要跟着停
        code, out, err = h.cli("disable", "demo-a")
        item_b = wait_state("demo-b", "deactivated")
        h.check("停用被依赖方：依赖方跟着停",
                code == 0 and item_b is not None and item_b["state"] == "deactivated",
                str(item_b))
        h.check("原因说清是「用不了」而不是「没装」",
                item_b is not None and "用不了" in item_b.get("reason", ""),
                str(item_b.get("reason") if item_b else None))

        code, out, err = h.cli("enable", "demo-a")
        item_b = wait_state("demo-b", "activated")
        h.check("重新启用后依赖方自动起来",
                code == 0 and item_b is not None and item_b["state"] == "activated",
                str(item_b.get("state") if item_b else None))

        # 重载：依赖方先停、换完再回来
        code, out, err = h.cli("reload", "demo-a")
        item_a = wait_state("demo-a", "activated")
        item_b = wait_state("demo-b", "activated")
        h.check("重载后自己与依赖方都在跑",
                code == 0 and item_a is not None and item_b is not None
                and item_a["state"] == "activated" and item_b["state"] == "activated",
                f"{item_a}/{item_b}")

        code, out, err = h.cli("restart", "demo-a")
        item_b = wait_state("demo-b", "activated")
        h.check("restart 之后依赖方也回来了",
                code == 0 and item_b is not None and item_b["state"] == "activated",
                str(item_b.get("state") if item_b else None))

        code, out, err = h.cli("reload", "not-installed")
        # 失败信息按 CLI 契约走 stderr，成功结果才走 stdout。
        h.check("重载不存在的插件给出人话", code == 4 and "找不到这个插件" in err,
                err.strip()[:60])

        # 预览：只看不做
        code, out, err = h.cli("plan")
        h.check("没有待办时 plan 说清楚", code == 0 and "什么都不用做" in out,
                out.strip()[:60])

        code, out, err = h.cli("disable", "demo-a")  # 让 demo-b 暂时起不来
        wait_state("demo-b", "deactivated")
        code, out, err = h.cli("plan")
        h.check("plan 列出起不来的插件与原因",
                code == 0 and "demo-b" in out and "用不了" in out, out.strip()[:120])
        h.cli("enable", "demo-a")

        # 改清单自动重算：把 demo-b 的依赖改成装不上的，等它自己停
        manifest = (h.data_root / "plugins" / "demo-b" / "plugin.toml").read_text(encoding="utf-8")
        edit_manifest("demo-b", manifest.replace('plugins = ["demo-a@1"]', 'plugins = ["demo-x@1"]'))
        item_b = wait_state("demo-b", "deactivated")
        h.check("改清单后自动重算，不用 reload",
                item_b is not None and item_b["state"] == "deactivated"
                and "demo-x" in item_b.get("reason", ""),
                str(item_b)[:110])

        edit_manifest("demo-b", manifest)
        item_b = wait_state("demo-b", "activated")
        h.check("改回来也自动恢复", item_b is not None and item_b["state"] == "activated",
                str(item_b)[:110])

        code, out, err = h.cli("uninstall", "demo-a")
        h.check("卸掉被依赖方", code == 0, out.strip()[:50])

        item_b = wait_state("demo-b", "deactivated")
        h.check("依赖方跟着停用并给出原因",
                item_b is not None and item_b["state"] == "deactivated"
                and "demo-a" in item_b.get("reason", ""),
                str(item_b))

        code, out, err = h.cli("install", str(h.artifacts / "demo-a"))
        item_b = wait_state("demo-b", "activated")
        h.check("装回来之后自动启用，无需重启",
                code == 0 and item_b is not None and item_b["state"] == "activated",
                str(item_b.get("state") if item_b else None))
    finally:
        h.stop()

    total = h.passed + h.failed
    print(f"\n{h.passed}/{total} 通过")
    return 0 if h.failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
