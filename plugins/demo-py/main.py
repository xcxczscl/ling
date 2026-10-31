"""Python 侧车示例。

它验证四件事：用 Python 起进程、握手、代登记、调用往返与事件送达。
与 C++ 的 demo-sidecar 做同一批事，区别只在用什么语言写。
"""

import os
import sys

# SDK 由宿主经 LING_SDK_DIR 指过来，那样插件不用把 sdk 拷一份进来。
sys.path.insert(0, os.environ.get("LING_SDK_DIR", ""))
from ling_sidecar import Host  # noqa: E402


host = Host()


@host.method("demo-py.ping")
def ping(args):
    return "pong from python"


@host.event_handler
def on_event(topic, payload):
    # 心跳每 200 毫秒一次，别每次都写日志。
    if host._ticks % 50 == 0:
        host.log("收到事件 %d 次（%s）" % (host._ticks, topic))


host.subscribe("kernel.tick")
# 收到第一个心跳时请宿主调自己一次：这是「侧车 → 宿主 → 侧车」的往返自测。
host.self_test("demo-py.ping")

if __name__ == "__main__":
    sys.exit(host.run())
