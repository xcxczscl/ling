#!/usr/bin/env python3
"""文档一致性校验：两版标题对应、表格列数、禁用词、乱码。

用法：
    python other/tools/check_docs.py                 # 校验全部 docs/
    python other/tools/check_docs.py <详细版> <简版>   # 只校验指定两份

退出码 0 表示全部通过，1 表示有问题。
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DOCS = ROOT / "docs"

# 禁用词：只禁「列举未尽」的「等」，等待/等于/相等/等级不算
BANNED = re.compile(
    r"尽量|可能|大概|适当|必要时|原则上|快速|高效|友好|合理|优化|较快|足够|若干"
)

# 「等」只在列举未尽的形态下算违规，例如「扫描、哈希等操作」。
# 相等、等价、等待、等级、等于等词不含此形态，不报。
BANNED_DENG = re.compile(r"、[^，。；]{1,20}等[，。、）\s]")

HEADING = re.compile(r"^#{1,4} ")


def rel(path):
    try:
        return str(Path(path).resolve().relative_to(ROOT)).replace("\\", "/")
    except ValueError:
        return str(path)


def content_lines(path):
    """返回 (行号, 行内容)；代码块内的行内容为 None。

    代码块里的 `# 注释` 不是 Markdown 标题，也不该参与措辞检查。
    """
    out = []
    in_code = False
    for i, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if line.lstrip().startswith("```"):
            in_code = not in_code
            out.append((i, None))
            continue
        out.append((i, None if in_code else line))
    return out


def headings(path):
    return [line.rstrip() for _, line in content_lines(path)
            if line is not None and HEADING.match(line)]


def check_tables(path):
    """同一表格块内每行的竖线数必须一致。"""
    problems = []
    lines = content_lines(path)
    block, start = [], 0
    for i, line in lines + [(0, "")]:
        if line is not None and line.lstrip().startswith("|"):
            if not block:
                start = i
            block.append((i, line))
            continue
        if len(block) >= 2:
            width = block[0][1].count("|")
            for ln, row in block:
                if row.count("|") != width:
                    problems.append(f"{rel(path)}:{ln} 表格列数不一致（块首 {start} 行有 {width} 列）")
        block = []
    return problems


def check_banned(path):
    """docs/log/ 下的历史记录不检查：日志只增不改，回头改措辞会破坏记录。"""
    if "log" in Path(path).parts:
        return []
    problems = []
    for i, line in content_lines(path):
        if line is None:
            continue
        # 命中「禁用词清单」这一行本身是元引用，不算违规
        if "禁用词" in line or "禁令" in line or "替换办法" in line:
            continue
        # 禁用词清单本身：一行里出现四个以上禁用词，视为清单，不报
        if len(BANNED.findall(line)) >= 4:
            continue
        for m in BANNED.finditer(line):
            problems.append(f"{rel(path)}:{i} 禁用词 {m.group()!r} {line.strip()[:40]}")
        # 「幂等」是术语，不算列举未尽
        for m in BANNED_DENG.finditer(line.replace("幂等", "幂X")):
            problems.append(f"{rel(path)}:{i} 禁用词（列举未尽）{m.group()!r}")
    return problems


def check_blank_before_table(path):
    """表格前必须是空行或另一个表格行。

    GFM 的表格不能打断段落：紧跟在文字后面的 `| ... |` 会被当成普通文本，
    在 GitHub 上整张表渲染不出来。这类错误肉眼扫不出来，只能由脚本查。
    """
    problems = []
    lines = [None if line is None else line
             for _, line in content_lines(path)]
    for i in range(len(lines)):
        line = lines[i]
        if line is None or not line.lstrip().startswith("|"):
            continue
        prev = lines[i - 1] if i else None
        # 代码块内的行 content_lines 已置 None，跳过
        if prev is None:
            continue
        if prev.strip() and not prev.lstrip().startswith("|"):
            problems.append(f"{rel(path)}:{i + 1} 表格前缺空行，GitHub 上不会渲染")
    return problems


def check_mojibake(path):
    text = path.read_text(encoding="utf-8")
    if "\ufffd" in text:
        return [f"{rel(path)} 含替换字符（乱码）"]
    return []


def check_pair(detailed, brief):
    a, b = headings(detailed), headings(brief)
    a = a[1:] if a else a          # 跳过首行 H1，标题文字本就不同
    b = b[1:] if b else b
    if a == b:
        return []
    problems = [f"{rel(detailed)} 与 {rel(brief)} 标题不对应"]
    for i in range(max(len(a), len(b))):
        x = a[i] if i < len(a) else "<缺>"
        y = b[i] if i < len(b) else "<缺>"
        if x != y:
            problems.append(f"  第 {i + 1} 个标题：详细版 {x!r} / 简版 {y!r}")
    return problems


def main(argv):
    problems = []
    if len(argv) == 3:
        problems += check_pair(Path(argv[1]), Path(argv[2]))
        files = [Path(argv[1]), Path(argv[2])]
    else:
        files = sorted(DOCS.rglob("*.md"))
        for f in files:
            rel = f.relative_to(DOCS)
            if "brief" in rel.parts:
                detailed = DOCS / Path(*[p for p in rel.parts if p != "brief"])
                if detailed.exists():
                    problems += check_pair(detailed, f)
    for f in files:
        problems += check_tables(f)
        problems += check_blank_before_table(f)
        problems += check_banned(f)
        problems += check_mojibake(f)

    if problems:
        print("\n".join(problems))
        print(f"\n共 {len(problems)} 个问题")
        return 1
    print(f"通过：{len(files)} 个文件")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
