#!/usr/bin/env python3
"""ling 侧车示例（docs/04 §6：jsonl 控制面，端点经 LING_SOCKET 传入）。

方法：
- demo-sidecar.ping        → "pong from python"
- demo-sidecar.announce    → 向宿主广播 demo-sidecar.announce 事件
- demo-sidecar.last_event  → 返回最近收到的订阅事件（证明 event 通路）
- demo-sidecar.pid         → 返回进程号（热更新验证用）
- demo-sidecar.crash       → 立即退出（崩溃注入）
"""
import json
import os
import socket


def main():
    endpoint = os.environ["LING_SOCKET"]  # 形如 tcp:127.0.0.1:12345
    host, port = endpoint.split(":", 1)[1].rsplit(":", 1)
    sock = socket.create_connection((host, int(port)))
    stream = sock.makefile("rwb", buffering=0)

    def send(obj):
        stream.write((json.dumps(obj, ensure_ascii=False) + "\n").encode("utf-8"))

    events = []
    running = True
    while running:
        line = stream.readline()
        if not line:
            break
        msg = json.loads(line.decode("utf-8"))
        op = msg.get("op")
        if op == "activate":
            send({"op": "register_method", "name": "demo-sidecar.ping"})
            send({"op": "register_method", "name": "demo-sidecar.announce"})
            send({"op": "register_method", "name": "demo-sidecar.last_event"})
            send({"op": "register_method", "name": "demo-sidecar.pid"})
            send({"op": "register_method", "name": "demo-sidecar.crash"})
            send({"op": "subscribe", "pattern": "demo-sidecar.note"})
        elif op == "invoke":
            name = msg["method"]
            req = msg["id"]
            if name == "demo-sidecar.ping":
                send({"op": "result", "id": req, "result": "pong from python"})
            elif name == "demo-sidecar.announce":
                send({"op": "emit", "topic": "demo-sidecar.announce",
                      "payload": {"by": "python"}})
                send({"op": "result", "id": req, "result": "ok"})
            elif name == "demo-sidecar.last_event":
                send({"op": "result", "id": req, "result": events[-1] if events else None})
            elif name == "demo-sidecar.pid":
                send({"op": "result", "id": req, "result": os.getpid()})
            elif name == "demo-sidecar.crash":
                send({"op": "result", "id": req, "result": "crashing"})
                stream.flush()
                os._exit(1)
            else:
                send({"op": "error", "id": req,
                      "error": {"code": "E_METHOD_MISSING", "message": name}})
        elif op == "event":
            events.append({"topic": msg.get("topic"), "payload": msg.get("payload")})
        elif op == "deactivate":
            send({"op": "deactivated"})
        elif op == "shutdown":
            running = False
    sock.close()


if __name__ == "__main__":
    main()
