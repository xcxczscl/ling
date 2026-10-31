"""ling 侧车 SDK（Python）。

宿主用 jsonl 帧与侧车通信：一行一个 JSON 对象。这份文件只做三件事——
握手、把帧分派给插件的处理函数、把插件的请求写成帧。

协议原文见 docs/ling/04-详细设计/02-插件装载与接口.md §8。

一条要紧的约定：**stdout 只许写帧**。宿主把侧车的 stderr 与 stdout 接到同一根
管道上（这样崩溃信息不会丢），所以任何一句 print 都会被当成一帧，宿主会判定
帧不合法并断开。要打日志请用 log()，它写到 <数据目录>/<插件名>.log。
"""

import json
import os
import sys
import traceback

PROTOCOL_VERSION = 1


class Host:
    def __init__(self):
        self.plugin_id = os.environ.get("LING_PLUGIN_ID", "")
        self.plugin_version = "1.0.0"
        self.token = os.environ.get("LING_TOKEN", "")
        self.data_dir = os.environ.get("LING_DATA_DIR", "")
        self._methods = {}
        self._events = None
        self._subscriptions = []
        self._self_test = None
        self._self_test_sent = False
        self._ticks = 0

        # stderr 与 stdout 在宿主那侧是一根管道，重定向到日志文件，
        # 免得 traceback 被当成半截帧。
        if self.data_dir:
            os.makedirs(self.data_dir, exist_ok=True)
            self._log_path = os.path.join(self.data_dir, self.plugin_id + ".log")
        else:
            self._log_path = None

    # ---------- 插件侧要用的几样 ----------

    def log(self, message):
        if not self._log_path:
            return
        try:
            with open(self._log_path, "a", encoding="utf-8") as handle:
                handle.write(message + "\n")
        except OSError:
            pass

    def method(self, name):
        """登记一个方法：@host.method("demo-py.ping")。"""

        def register(fn):
            self._methods[name] = fn
            return fn

        return register

    def event_handler(self, fn):
        """收到订阅的事件时调用 fn(topic, payload)。"""
        self._events = fn
        return fn

    def subscribe(self, pattern):
        self._subscriptions.append(pattern)

    def emit(self, topic, payload=None):
        self._send({"v": PROTOCOL_VERSION, "op": "emit", "topic": topic,
                    "payload": payload if payload is not None else {}})

    def self_test(self, method, args=None):
        """登记一次自测：第一个心跳到达时请宿主调自己一次，验证往返。"""
        self._self_test = (method, args or {})

    # ---------- 内部 ----------

    def _send(self, frame):
        sys.stdout.write(json.dumps(frame, ensure_ascii=False) + "\n")
        sys.stdout.flush()

    def _reply(self, call_id, ok, body):
        frame = {"v": PROTOCOL_VERSION, "id": call_id,
                 "op": "result" if ok else "error"}
        if ok:
            frame["ok"] = True
            frame["result"] = body
        else:
            frame["ok"] = False
            frame["error"] = body
        self._send(frame)

    def _handle(self, frame):
        op = frame.get("op", "")

        if op == "activate":
            for name in self._methods:
                self._send({"v": PROTOCOL_VERSION, "op": "register_method", "name": name})
            for pattern in self._subscriptions:
                self._send({"v": PROTOCOL_VERSION, "op": "subscribe", "pattern": pattern})
            self.log("已激活")
            return True

        if op == "invoke":
            call_id = frame.get("id", 0)
            name = frame.get("method", "")
            fn = self._methods.get(name)
            if fn is None:
                self.log("收到不认识的方法：" + name)
                self._reply(call_id, False,
                            {"code": "E_METHOD_MISSING", "message": "没有这个方法：" + name})
                return True
            try:
                result = fn(frame.get("args") or {})
                self._reply(call_id, True, result)
            except Exception as error:  # 插件自己的异常不能带走整个进程
                self.log("方法 " + name + " 抛异常：" + repr(error))
                self._reply(call_id, False,
                            {"code": "E_INTERNAL", "message": str(error)})
            return True

        if op == "event":
            self._ticks += 1
            if self._events is not None:
                try:
                    self._events(frame.get("topic", ""), frame.get("payload") or {})
                except Exception as error:
                    self.log("事件处理抛异常：" + repr(error))
            if self._self_test is not None and not self._self_test_sent:
                self._self_test_sent = True
                method, args = self._self_test
                self._send({"v": PROTOCOL_VERSION, "id": 9001, "op": "call",
                            "method": method, "args": args})
            return True

        if op in ("result", "error"):
            if frame.get("id", 0) == 9001:
                self.log("自测调用成功：" + json.dumps(frame.get("result"), ensure_ascii=False)
                         if op == "result" else "自测调用失败：" + json.dumps(frame.get("error")))
            return True

        if op in ("deactivate", "shutdown"):
            self.log("收到 " + op + "，退出")
            if op == "deactivate":
                self._send({"v": PROTOCOL_VERSION, "op": "deactivated"})
            return False

        self.log("收到没见过的 op：" + op)
        return True

    def run(self):
        if not self.plugin_id:
            self._send_error_and_exit("缺少 LING_PLUGIN_ID：这个程序只能由 ling 宿主拉起")
            return 2

        self._send({"v": PROTOCOL_VERSION, "op": "hello", "plugin_id": self.plugin_id,
                    "plugin_version": self.plugin_version, "token": self.token})

        for line in sys.stdin.buffer:
            line = line.decode("utf-8", errors="replace").strip()
            if not line:
                continue
            try:
                frame = json.loads(line)
            except ValueError:
                self.log("收到不合法的帧：" + line[:120])
                continue
            try:
                if not self._handle(frame):
                    return 0
            except Exception as error:
                self.log("处理帧时出错：" + repr(error) + "\n" + traceback.format_exc())
        return 0

    def _send_error_and_exit(self, message):
        # 起不来的时候也走帧，让宿主能把原因显示给用户。
        self._send({"v": PROTOCOL_VERSION, "op": "error",
                    "error": {"code": "E_PROTOCOL", "message": message}})


def main(host):
    """插件入口：把装配好的 Host 交给 SDK 跑起来。"""
    return host.run()
