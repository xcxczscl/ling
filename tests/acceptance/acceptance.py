#!/usr/bin/env python3
"""ling 验收脚本。依据 docs/ling/07-测试与验收.md §3、§4。

用法：
    python tests/acceptance/acceptance.py --script l1_manifest
    python tests/acceptance/acceptance.py --script all

每个脚本用独立数据根 run/<preset>/data-test/<脚本名>/，独立端口（8020 起）。
退出码 0 表示该脚本全部用例通过；1 表示有失败；2 表示环境问题（起不来宿主）。
"""
import argparse
import json
import pathlib
import shutil
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parents[2]
EXE_NAME = "ling.exe" if sys.platform.startswith("win") else "ling"

# 脚本名 → 起始端口（07 §3.2：验收脚本从 8020 起，彼此不重叠）
SCRIPTS = {
    "l1_manifest": 8020,
    "l1_route": 8021,
    "l1_event": 8022,
    "l2_uninstall": 8023,
    "l1_bench_call": 8024,
}


class Harness:
    def __init__(self, script, port, preset):
        self.script = script
        self.preset = preset
        self.exe = ROOT / "run" / preset / EXE_NAME
        self.artifacts = ROOT / "run" / preset / "plugins"
        self.data_root = ROOT / "run" / preset / "data-test" / script
        self.port = port
        self.proc = None
        self.passed = 0
        self.failed = 0
        self.skipped = 0

    def start(self):
        if self.data_root.exists():
            shutil.rmtree(self.data_root)
        self.data_root.mkdir(parents=True)
        # 宿主输出落到文件：失败时要能看到崩溃现场，用管道会丢。
        self.serve_log = self.data_root / "serve.log"
        self._serve_file = open(self.serve_log, "w", encoding="utf-8")
        self.proc = subprocess.Popen(
            [str(self.exe), "serve", "--data-dir", str(self.data_root), "--port", str(self.port)],
            stdout=self._serve_file,
            stderr=subprocess.STDOUT,
            text=True,
        )
        # 等到 session.json 出现：它比 registry.json 更晚写，出现了才算真的能连。
        deadline = time.time() + 15
        session = self.data_root / "session.json"
        while time.time() < deadline:
            if session.exists():
                return True
            if self.proc.poll() is not None:
                return False
            time.sleep(0.1)
        return False

    def cli(self, *args):
        result = subprocess.run(
            [str(self.exe), *args, "--data-dir", str(self.data_root)],
            capture_output=True,
            text=True,
        )
        return result.returncode, result.stdout, result.stderr

    def stop(self):
        if self.proc is None:
            return
        if self.proc.poll() is None:
            try:
                self.cli("shutdown")
            except Exception:
                pass
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=5)
        if getattr(self, "_serve_file", None) is not None:
            self._serve_file.close()
            self._serve_file = None
        self.proc = None

    # ---- 断言 ----
    def check(self, case, ok, detail=""):
        if ok:
            self.passed += 1
            print(f"  PASS {case}")
        else:
            self.failed += 1
            print(f"  FAIL {case}  {detail}")

    def skip(self, case, reason):
        self.skipped += 1
        print(f"  SKIP {case}  {reason}")

    def expect_error(self, case, args, code, needle=""):
        rc, out, err = self.cli(*args)
        text = out + err
        ok = rc != 0 and code in text and (not needle or needle in text)
        self.check(case, ok, f"rc={rc} out={text.strip()[:200]}")
        return text

    # ---- 夹具 ----
    def make_variant(self, dir_name, new_id, mutate=None):
        """复制 demo-a 的产物，按需改写清单。目录名由调用方决定（用来测目录名不符）。"""
        source = self.artifacts / "demo-a"
        target = self.data_root / "fixtures" / dir_name
        target.parent.mkdir(parents=True, exist_ok=True)
        if target.exists():
            shutil.rmtree(target)
        shutil.copytree(source, target)
        manifest = (target / "plugin.toml").read_text(encoding="utf-8")
        if mutate is not None:
            manifest = mutate(manifest)
        (target / "plugin.toml").write_text(manifest, encoding="utf-8")
        return target

    def expect_manifest_error(self, case, dir_name, new_id, mutate=None):
        variant = self.make_variant(dir_name, new_id, mutate)
        rc, out, err = self.cli("install", str(variant))
        text = out + err
        self.check(case, rc != 0 and "E_MANIFEST_INVALID" in text, text.strip()[:200])


def replace_line(manifest, key, value):
    lines = []
    for line in manifest.splitlines():
        if line.startswith(f"{key} = "):
            lines.append(f'{key} = "{value}"')
        else:
            lines.append(line)
    return "\n".join(lines) + "\n"


def drop_line(manifest, key):
    return "\n".join(l for l in manifest.splitlines() if not l.startswith(f"{key} = ")) + "\n"


# ---------------- 用例组 ----------------

def case_l1_manifest(h):
    """001 至 013 的清单部分（TC-101 至 TC-108、TC-114）。"""
    # TC-101 目录名与清单 id 不一致
    h.expect_manifest_error("TC-101", "Demo-A", "demo-a")

    # TC-102 逐个删必填字段
    for field in ("id", "version", "api", "name"):
        h.expect_manifest_error(
            f"TC-102/{field}", "demo-a", "demo-a", lambda m, f=field: drop_line(m, f)
        )

    # TC-103 非法 id。目录名跟着 id 一起变，才测得到 id 本身的校验。
    for bad in ("Demo", "demo_a", "demo..a", "a" * 65):
        h.expect_manifest_error(
            f"TC-103/{bad[:8]}", bad, "demo-a", lambda m, b=bad: replace_line(m, "id", b)
        )

    # TC-104 非法 version
    for bad in ("1.0", "1.0.0.0", "70000.0.0"):
        h.expect_manifest_error(
            f"TC-104/{bad}", "demo-a", "demo-a", lambda m, b=bad: replace_line(m, "version", b)
        )

    # TC-105 api 主版本与宿主不同
    variant = h.make_variant("demo-a", "demo-a", lambda m: replace_line(m, "api", "99"))
    rc, out, err = h.cli("install", str(variant))
    text = out + err
    h.check("TC-105", rc != 0 and "E_API_MISMATCH" in text, text.strip()[:200])

    # TC-108 同一编号装两次
    rc1, _, _ = h.cli("install", str(h.artifacts / "demo-a"))
    rc2, out2, err2 = h.cli("install", str(h.artifacts / "demo-a"))
    text = out2 + err2
    h.check("TC-108", rc1 == 0 and rc2 != 0 and "E_PLUGIN_EXISTS" in text, text.strip()[:200])

    # TC-114 业务插件占用 core- 前缀。目录名与 id 一致，确保被拦住的是前缀而不是目录名。
    h.expect_manifest_error(
        "TC-114", "core-foo", "demo-a", lambda m: replace_line(m, "id", "core-foo")
    )

    # TC-079 首次启动预装三个内置插件
    rc, out, err = h.cli("plugins", "--json")
    listed = json.loads(out)["plugins"] if rc == 0 and out.strip().startswith("{") else []
    ids = {item["id"] for item in listed}
    expected = {"core-settings", "core-plugin-manager", "core-log"}
    states = {item["id"]: item["state"] for item in listed}
    h.check(
        "TC-079",
        expected <= ids and all(states.get(i) == "activated" for i in expected),
        f"实际 {sorted(ids)} 状态 {states}",
    )


def case_l1_route(h):
    """014 至 016 的调用侧（TC-203 至 TC-205）。"""
    h.cli("install", str(h.artifacts / "demo-a"))
    h.cli("install", str(h.artifacts / "demo-caller"))

    rc, out, err = h.cli("plugins", "--json")
    states = {item["id"]: item["state"] for item in json.loads(out)["plugins"]}
    h.check("TC-202", states.get("demo-a") == "activated", str(states))

    # demo-caller 激活时调 demo-a.ping 成功，日志里能看到 pong
    rc, out, err = h.cli("logs", "demo-caller")
    h.check("TC-202/调用成功", rc == 0 and "pong" in out, out.strip()[:200])

    # TC-203 卸载被调方后再触发一次调用：重装 demo-caller 让它重新激活
    h.cli("uninstall", "demo-a")
    h.cli("uninstall", "demo-caller", "--purge")
    h.cli("install", str(h.artifacts / "demo-caller"))
    rc, out, err = h.cli("logs", "demo-caller")
    h.check(
        "TC-203",
        rc == 0 and ("E_PLUGIN_MISSING" in out or "call failed" in out),
        out.strip()[:200],
    )

    # TC-205 失败不影响其他条目：内置插件仍在且可查
    rc, out, err = h.cli("plugins", "--json")
    ids = {item["id"] for item in json.loads(out)["plugins"]}
    h.check("TC-205", "core-log" in ids, str(sorted(ids)))


def case_l1_event(h):
    """018、019 的广播侧（TC-208、TC-209）。"""
    h.cli("install", str(h.artifacts / "demo-a"))
    h.cli("install", str(h.artifacts / "demo-caller"))

    # TC-208 订阅 demo-a.* 的插件收到了 demo-a.pinged
    rc, out, err = h.cli("logs", "demo-caller")
    h.check("TC-208", rc == 0 and "event demo-a.pinged pong" in out, out.strip()[:200])

    # 内核状态事件也被订阅到（订阅的是精确主题 kernel.plugin.state）
    h.check(
        "TC-208/内核事件",
        "kernel.plugin.state" in out and "demo-caller" in out,
        out.strip()[:200],
    )

    # TC-209 越权广播：需要专用夹具插件，MVP 无此夹具
    h.skip("TC-209", "需要能广播任意主题的夹具插件，MVP 未提供")


def case_l2_uninstall(h):
    """009 至 011（TC-109 至 TC-111）。"""
    h.cli("install", str(h.artifacts / "demo-a"))
    plugin_dir = h.data_root / "plugins" / "demo-a"
    h.check("TC-109/前置", (plugin_dir / "plugin.toml").exists(), str(plugin_dir))

    rc, out, err = h.cli("uninstall", "demo-a")
    remaining = sorted(p.name for p in plugin_dir.iterdir()) if plugin_dir.exists() else []
    h.check("TC-109", rc == 0 and remaining == ["data"], f"剩余 {remaining}")

    # TC-111 重装后 data/ 内容一致
    probe = plugin_dir / "data" / "probe.txt"
    probe.write_text("keep-me", encoding="utf-8")
    h.cli("install", str(h.artifacts / "demo-a"))
    h.check(
        "TC-111",
        probe.exists() and probe.read_text(encoding="utf-8") == "keep-me",
        "重装后数据丢失",
    )

    # TC-110 --purge 连数据一起删
    h.cli("uninstall", "demo-a", "--purge")
    h.check("TC-110", not plugin_dir.exists(), str(plugin_dir))


def case_l1_bench_call(h, p95_limit=1.0, p99_limit=3.0):
    """089：调用延迟。基准在 debug 构建上跑，阈值按 docs/ling/07 §4.7 的口径记录。"""
    h.cli("install", str(h.artifacts / "demo-a"))
    rc, out, err = h.cli("install", str(h.artifacts / "demo-bench"))
    if rc != 0:
        h.check("TC-089", False, (out + err).strip()[:200])
        return
    bench_log = h.data_root / "plugins" / "demo-bench" / "data" / "bench.log"
    if not bench_log.exists():
        h.check("TC-089", False, "基准插件没有写出 bench.log")
        return
    line = bench_log.read_text(encoding="utf-8").strip().splitlines()[-1]
    values = {}
    for part in line.split():
        if "=" in part:
            key, _, value = part.partition("=")
            values[key] = value
    p95 = float(values.get("p95", "inf").replace("us", ""))
    p99 = float(values.get("p99", "inf").replace("us", ""))
    print(f"  基准结果：{line}")

    # 结果按 07 §5.5 追加到 run/<preset>/bench-history.csv。
    history = ROOT / "run" / h.preset / "bench-history.csv"
    stamp = time.strftime("%Y-%m-%dT%H:%M:%S")
    with open(history, "a", encoding="utf-8") as handle:
        handle.write(f"{stamp}, l1_bench_call, {line.replace(' ', ', ')}, build={h.preset}\n")

    if "debug" in h.preset:
        # 调试构建带 ASan，单次调用有十几倍开销，按 07 §5.3 的口径只在 release 上判定。
        h.skip("TC-089", f"debug 构建不判定，实测 p95={p95}us p99={p99}us，请用 --preset win-release")
        return
    # 07 §5.4：超过上限 1.1 倍判失败，1.0 至 1.1 倍只记警告。
    h.check("TC-089/p95", p95 <= p95_limit * 1.1, f"p95={p95}us 上限 {p95_limit}us")
    h.check("TC-089/p99", p99 <= p99_limit * 1.1, f"p99={p99}us 上限 {p99_limit}us")


CASES = {
    "l1_manifest": case_l1_manifest,
    "l1_route": case_l1_route,
    "l1_event": case_l1_event,
    "l2_uninstall": case_l2_uninstall,
    "l1_bench_call": case_l1_bench_call,
}


def run_script(name, preset):
    port = SCRIPTS[name]
    harness = Harness(name, port, preset)
    print(f"== {name}（数据根 {harness.data_root}，端口 {port} 起）")
    if not harness.start():
        print("  环境错误：宿主没起来")
        harness.stop()
        return 2
    try:
        CASES[name](harness)
    finally:
        harness.stop()
    print(
        f"== {name}: 通过 {harness.passed}，失败 {harness.failed}，跳过 {harness.skipped}"
    )
    return 1 if harness.failed else 0


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--script", default="all", choices=["all", *SCRIPTS])
    parser.add_argument("--preset", default="win-debug", help="构建预设名，如 win-debug / win-release")
    args = parser.parse_args()

    names = list(SCRIPTS) if args.script == "all" else [args.script]
    worst = 0
    for name in names:
        worst = max(worst, run_script(name, args.preset))
    if worst == 0:
        print("全部通过")
    return worst


if __name__ == "__main__":
    sys.exit(main())
