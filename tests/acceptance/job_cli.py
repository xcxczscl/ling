"""`ling job` 三个命令的验收。

任务定义存在 core-settings、由 core-job 解释，命令行只是把参数整理成 kernel.call。
所以这里验的是「参数整理对不对、回显是不是人能看懂」，以及失败路径的提示。
"""

import argparse
import json
import pathlib
import shutil
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
    # 系统临时目录：每次从空白开始，也不碰仓库里的目录。
    data = pathlib.Path(tempfile.mkdtemp(prefix="ling-test-job-cli-"))
    # mkdtemp 已经建好了

    results = []

    def check(name, ok, detail=""):
        results.append(ok)
        print(("通过  " if ok else "失败  ") + name + ("  " + detail if detail else ""))

    def ling(*extra):
        done = subprocess.run([str(exe), *extra, "--data-dir", str(data)],
                              capture_output=True, text=True, encoding="utf-8",
                              errors="replace")
        return done.returncode, (done.stdout or "") + (done.stderr or "")

    host = subprocess.Popen([str(exe), "serve", "--data-dir", str(data), "--port", "7900"],
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
                            encoding="utf-8", errors="replace")
    time.sleep(2)

    code, out = ling("job", "list")
    check("空列表给人话", code == 0 and "现在没有定时任务" in out, out.strip()[:80])

    code, out = ling("job", "add", "probe", "--call", "core-log.write")
    check("缺时间表时退出码 2 并说明格式", code == 2 and "--cron" in out, f"{code} {out.strip()[:60]}")

    code, out = ling("job", "add", "probe", "--cron", "0 3 * * *")
    check("缺目标方法时退出码 2", code == 2 and "--call" in out, f"{code} {out.strip()[:60]}")

    code, out = ling("job", "add", "probe", "--cron", "0 3 * * *",
                     "--call", "core-log.write",
                     "--args", '{"plugin":"core-job","level":"info","message":"到点了"}')
    check("建任务成功", code == 0 and "已建任务 probe" in out, out.strip()[:80])

    code, out = ling("job", "list")
    check("列表有表头与任务", "任务名" in out and "probe" in out and "0 3 * * *" in out,
          out.strip()[:120])

    code, out = ling("job", "list", "--json")
    parsed = json.loads(out) if code == 0 else {}
    names = [item["name"] for item in parsed.get("jobs", [])]
    check("--json 输出可解析", code == 0 and names == ["probe"], str(names))

    code, out = ling("job", "add", "bad", "--cron", "0 25 * * *", "--call", "core-log.write")
    check("非法 cron 当场指出哪一段超范围", code == 2 and "时" in out and "25" in out,
          f"{code} {out.strip()[:90]}")

    code, out = ling("job", "add", "x", "--cron", "0 3 * * *", "--call", "core-log.write",
                     "--args", "not-json")
    check("--args 不是对象时退出码 2", code == 2 and "JSON" in out, f"{code} {out.strip()[:60]}")

    code, out = ling("job", "remove", "probe")
    check("删任务", code == 0 and "已删任务 probe" in out, out.strip()[:60])

    code, out = ling("job", "list")
    check("删完列表为空", "现在没有定时任务" in out, out.strip()[:60])

    code, out = ling("job", "nope")
    check("不认识的动作用法提示", code == 2 and "list" in out, f"{code} {out.strip()[:60]}")

    code, out = ling("job")
    check("不带动作时打用法", code == 2 and "ling job add" in out, f"{code} {out.strip()[:60]}")

    # 帮助里要有 job 三行，否则用户找不到入口
    code, out = ling("--help")
    check("帮助里有 job 入口", code == 0 and "job list" in out and "job add" in out, "")

    subprocess.run([str(exe), "shutdown", "--data-dir", str(data)], capture_output=True, text=True)
    host.wait(timeout=10)

    passed = sum(1 for ok in results if ok)
    print(f"\n{passed}/{len(results)} 通过")
    return 0 if passed == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())
