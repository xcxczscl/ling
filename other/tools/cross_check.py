"""交叉比对：把「同一事实」在各文档里的取值摊出来，供人工核对。

用法：
    python other/tools/cross_check.py            # 全部检查项
    python other/tools/cross_check.py port       # 只查某一项

设计原则：脚本不判断对错，只把取值与出处列出来。判断由人做，
因为「7901 是端口顺延的例子」和「7910 是界面端口」都是合法取值，
只有放在上下文里才知道有没有冲突。
"""

import re
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# 只查设计文档与根级说明；代码块内容一并查（命令行示例里的端口也要一致）
def docs():
    paths = []
    for pat in ("docs/**/*.md", "README.md", "AGENTS.md"):
        paths.extend(ROOT.glob(pat))
    return sorted(p for p in paths if "docs/log" not in p.as_posix())


def rel(p):
    return p.relative_to(ROOT).as_posix()


def lines_of(path):
    return list(enumerate(path.read_text(encoding="utf-8").splitlines(), 1))


# ---------------- 检查项 ----------------

def check_port():
    """端口：管理端口 7900、界面端口 7910、顺延规则。"""
    hits = defaultdict(list)
    for p in docs():
        for i, line in lines_of(p):
            for m in re.finditer(r"\b(79\d\d)\b", line):
                hits[m.group(1)].append(f"{rel(p)}:{i}")
    print("== 端口 ==\n")
    for port in sorted(hits):
        print(f"  {port}  出现 {len(hits[port])} 次")
        for loc in hits[port]:
            print(f"      {loc}")
    print()


def check_errcode():
    """错误码：每个码出现在哪些文档；与 04/02 的登记表比对。"""
    reg = ROOT / "docs/ling/04-详细设计/02-插件装载与接口.md"
    text = reg.read_text(encoding="utf-8")
    # 登记表是一行一个码，形如 | `E_XXX` | 含义 | ...
    registered = set(re.findall(r"^\| `(E_[A-Z_]+)`", text, re.M))

    use = defaultdict(set)
    for p in docs():
        for i, line in lines_of(p):
            for code in re.findall(r"(?<![A-Z_])E_[A-Z][A-Z_]*[A-Z](?![A-Z_])", line):
                use[code].add(rel(p))

    print("== 错误码 ==\n")
    print(f"  04/02 登记表共 {len(registered)} 个\n")
    extra = sorted(set(use) - registered)
    if extra:
        print("  【登记表里没有，但别处在用】")
        for c in extra:
            print(f"    {c}: {sorted(use[c])}")
        print()
    unused = sorted(registered - set(use))
    if unused:
        print("  【登记了但除 04/02 外没人用】")
        for c in unused:
            print(f"    {c}")
        print()
    print("  每个码被引用的文档数（2 个以上属正常：01 附录 + 04/02）")
    for c in sorted(use, key=lambda x: (-len(use[x]), x)):
        print(f"    {c:<26} {len(use[c])} 处")
    print()


def check_numbers():
    """阈值类数字：指标词后面紧跟的数字，各处是否一致。

    只抓「指标词 + 8 字以内 + 数字 + 单位」这种紧邻形态，避免把同一行里
    无关的数字（比如估算依据里的乘数）也算成同一个指标的值。
    """
    # 指标 -> 指标词后的紧邻正则（捕获数字与单位）
    facts = {
        "方法调用超时": r"调用超时[^0-9。；|]{0,8}?(\d+(?:\.\d+)?)\s*(微秒|毫秒|秒)",
        "回调超时": r"回调超时[^0-9。；|]{0,8}?(\d+(?:\.\d+)?)\s*(微秒|毫秒|秒)",
        "载荷上限": r"载荷[^0-9。；|]{0,10}?(\d+(?:\.\d+)?)\s*(KB|MB)",
        "常驻内存": r"常驻内存[^0-9。；|]{0,10}?(\d+(?:\.\d+)?)\s*(MB)",
        "账本上限": r"账本[^0-9。；|]{0,12}?(\d+)\s*(条)",
        "重载增长": r"重载[^0-9。；|]{0,14}?(\d+(?:\.\d+)?)\s*(MB)",
        "空闲 CPU": r"空闲[^0-9。；|]{0,10}?(\d+(?:\.\d+)?)\s*(%)",
        "并发连接": r"并发连接[^0-9。；|]{0,10}?(\d+)\s*(条)",
        "日志轮转": r"轮转[^0-9。；|]{0,10}?(\d+(?:\.\d+)?)\s*(MB)",
        "日志保留": r"保留[^0-9。；|]{0,6}?(\d+)\s*(天)",
        "崩溃刷盘": r"刷盘[^0-9。；|]{0,12}?(\d+(?:\.\d+)?)\s*(毫秒)",
        "调和去抖": r"去抖[^0-9。；|]{0,12}?(\d+(?:\.\d+)?)\s*(毫秒)",
    }
    print("== 阈值类数字：同一指标出现过几种取值 ==\n")
    for name, pat in facts.items():
        rx = re.compile(pat)
        seen = defaultdict(list)
        for p in docs():
            for i, line in lines_of(p):
                for num, u in rx.findall(line):
                    seen[f"{num} {u}"].append(f"{rel(p)}:{i}")
        if not seen:
            print(f"  {name:<14} （未按此形态出现）")
        elif len(seen) == 1:
            v = next(iter(seen))
            print(f"  {name:<14} 单一取值 {v}，{len(seen[v])} 处")
        else:
            print(f"  {name:<14} 【{len(seen)} 种取值，需人工看图】")
            for v, locs in sorted(seen.items(), key=lambda kv: -len(kv[1])):
                print(f"      {v:<10} {len(locs)} 处  例：{locs[0]}")
    print()


def check_subcommand():
    """子命令：各文档列的清单是否一致。"""
    print("== 子命令 ==\n")
    names = ["serve", "shutdown", "plugins", "install", "uninstall", "enable",
             "disable", "reload", "restart", "logs", "doctor"]
    for p in docs():
        text = p.read_text(encoding="utf-8")
        # 只统计同一行里出现 3 个以上子命令的句子，那才是「清单」
        for i, line in enumerate(text.splitlines(), 1):
            present = [n for n in names if re.search(rf"\b{n}\b", line)]
            if len(present) >= 3:
                print(f"  {rel(p)}:{i}  {len(present)} 个：{'、'.join(present)}")
    print()


def check_builtin():
    """内置插件：五个名字在各文档的出现情况。"""
    print("== 内置插件 ==\n")
    names = ["core-ui", "core-settings", "core-plugin-manager", "core-job", "core-log"]
    for p in docs():
        text = p.read_text(encoding="utf-8")
        present = [n for n in names if n in text]
        if present:
            print(f"  {rel(p)}: {len(present)} 个 -> {'、'.join(present)}")
    print()


def check_event():
    """内核事件名与字段。"""
    print("== 内核事件 ==\n")
    for p in docs():
        for i, line in lines_of(p):
            if "kernel.plugin.state" in line or "kernel.plan.step" in line:
                print(f"  {rel(p)}:{i}  {line.strip()[:100]}")
    print()


def check_paths():
    """数据根与权限。"""
    print("== 数据根与权限 ==\n")
    for p in docs():
        for i, line in lines_of(p):
            if re.search(r"APPDATA|\.local/share/ling|0[67]00", line):
                print(f"  {rel(p)}:{i}  {line.strip()[:100]}")
    print()


def check_version():
    """版本号写法。"""
    print("== 版本号 ==\n")
    for p in docs():
        for i, line in lines_of(p):
            if re.search(r"0\.\d+\.\d+-b\d|0\.1\.0\b", line):
                print(f"  {rel(p)}:{i}  {line.strip()[:100]}")
    print()


def check_link():
    """文档内引用的路径是否存在。断链比措辞问题更影响查阅。"""
    print("== 引用路径 ==\n")
    pat = re.compile(r"`(docs/[^`]+?\.md)`|\]\((docs/[^)\s]+\.md)\)")
    bad = []
    total = 0
    for p in docs():
        for i, line in lines_of(p):
            if "未编写" in line or "待编写" in line:
                continue
            for m in pat.finditer(line):
                target = (m.group(1) or m.group(2)).replace("%20", " ")
                total += 1
                if not (ROOT / target).exists():
                    bad.append(f"{rel(p)}:{i}  {target}")
    print(f"  共 {total} 处引用，损坏 {len(bad)} 处")
    for b in bad:
        print("    " + b)
    print()


CHECKS = {
    "port": check_port,
    "err": check_errcode,
    "num": check_numbers,
    "cmd": check_subcommand,
    "builtin": check_builtin,
    "event": check_event,
    "path": check_paths,
    "ver": check_version,
    "link": check_link,
}


def main():
    want = sys.argv[1:] or list(CHECKS)
    for name in want:
        if name not in CHECKS:
            print(f"未知检查项 {name}，可选：{', '.join(CHECKS)}")
            return 1
        CHECKS[name]()
    return 0


if __name__ == "__main__":
    sys.exit(main())
