"""章节号引用校验：检查 `§X.Y` 是否真的指向目标文档里的某一节。

用法：
    python tools/check_refs.py            # 只报疑似错误
    python tools/check_refs.py -v         # 连通过的也打印

核心是「一个 § 该指向谁」的判定。按出现位置从左到右扫，
每个 § 绑定到它**左边最近的**那一个文档指代：

| 写法 | 例 | 解析为 |
|---|---|---|
| 完整路径（反引号内，允许空格） | `` `docs/ling/11-MVP 范围.md` §2.2 `` | 该文件 |
| 简称，反引号内 | `` `04/02` §5.2 `` | `04-详细设计/02-*.md` |
| 简称，裸写且紧跟 § | `06 §3.6` | `docs/ling/06-*.md` |
| 「本文」 | `本文 §3.6` | 当前文件 |
| 都没有 | `（见 §4.4）` | 当前文件 |
| 写了「同上」 | 表格里 `同上 §3.2` | 本表上一行出现过的路径 |

**不查的两类**（末尾会统计数量，不假装覆盖）：
- 「第 7 节」这种按标题引用；
- 指向尚未编写的文档（`（待编写）`）。

退出码：有疑似错误时为 1。
"""

import re
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

HEAD = re.compile(r"^(#{1,6})\s+(\d+(?:\.\d+)*)\.?\s+(.*)$")

# 行内的「文档指代」与「章节号」，按出现位置排序后逐个绑定
TOKENS = [
    # 完整路径：允许带空格（如 `11-MVP 范围.md`），允许不在反引号内
    ("full",   re.compile(r"`?(docs/[^`\n]*?\.md)`?")),
    # 简称带标题，如 `04/01 内核四服务` §6.3
    ("shortq", re.compile(r"`(0\d/0[1-9]|0\d|1[0-2])[^`§]*?`(?=\s*§)")),
    # 简称恰好占满反引号，如 `04/02` §5.2
    ("shortq", re.compile(r"`(0\d/0[1-9]|0\d|1[0-2])`")),
    # 「本文」必须紧跟章节号，否则不算指代
    ("self",   re.compile(r"本文\s*(?=§)")),
    # 裸写简称，如 06 §3.6、01 需求 §3.6
    ("shortb", re.compile(r"(?<![\w`/])(0\d/0[1-9]|0\d|1[0-2])(?:\s*需求)?(?=\s*§)")),
    ("ref",    re.compile(r"§\s*(\d+(?:\.\d+)*)")),
    # 「第 7 节」：只认一级章号
    ("jie",    re.compile(r"第\s*(\d+)\s*节")),
]

# 句子边界：路径与章节号之间出现这些符号，说明不是同一句，不跨句绑定
BOUNDARY = re.compile(r"[。；]")


def docs():
    paths = []
    for pat in ("docs/**/*.md", "README.md"):
        paths.extend(ROOT.glob(pat))
    return sorted(p for p in paths if "docs/log" not in p.as_posix())


def rel(p):
    return p.relative_to(ROOT).as_posix()


def resolve_short(tag):
    if "/" in tag:
        sub, num = tag.split("/")
        hits = list((ROOT / "docs" / "ling" / f"{sub}-详细设计").glob(f"{num}-*.md"))
    else:
        hits = list((ROOT / "docs" / "ling").glob(f"{tag}-*.md"))
    return hits[0] if hits else None


def sections(path):
    out = set()
    for line in path.read_text(encoding="utf-8").splitlines():
        m = HEAD.match(line)
        if m:
            out.add(m.group(2))
    return out


def scan(line, current):
    """返回 [(位置, 类型, 值)]，按位置排序。"""
    found = []
    for kind, rx in TOKENS:
        for m in rx.finditer(line):
            val = None if kind == "self" else m.group(1)
            found.append((m.start(), kind, val))
    found.sort()
    return found


def main():
    verbose = "-v" in sys.argv
    cache = {}
    total = ok = 0
    bad = []
    skipped = defaultdict(int)

    for p in docs():
        lines = p.read_text(encoding="utf-8").splitlines()
        last_path = None
        in_code = False
        for i, line in enumerate(lines, 1):
            if line.lstrip().startswith("```"):
                in_code = not in_code
                continue
            if in_code:
                continue

            if "（待编写）" in line or "（未编写）" in line:
                skipped["目标文档未编写"] += 1
                continue

            toks = scan(line, p)
            # 先更新「最近出现的路径」，供「同上」使用
            for _, kind, val in toks:
                if kind == "full":
                    last_path = ROOT / val
                elif kind in ("shortq", "shortb"):
                    hit = resolve_short(val)
                    if hit:
                        last_path = hit

            target = None      # 当前 § 绑定的目标
            anchor = -1        # 上一次绑定发生的位置
            for pos, kind, val in toks:
                if kind == "full":
                    target, anchor = ROOT / val, pos
                elif kind in ("shortq", "shortb"):
                    hit = resolve_short(val)
                    if hit:
                        target, anchor = hit, pos
                elif kind == "self":
                    target, anchor = p, pos
                elif kind == "jie":
                    tgt = target if target is not None else p
                    total += 1
                    if not tgt.exists():
                        skipped["目标文件不存在"] += 1
                        continue
                    if tgt not in cache:
                        cache[tgt] = sections(tgt)
                    if val in cache[tgt]:
                        ok += 1
                        if verbose:
                            print(f"  OK   {rel(p)}:{i}  第 {val} 节 -> {rel(tgt)}")
                    else:
                        bad.append((rel(p), i, f"第{val}节", rel(tgt), line.strip()[:70]))
                elif kind == "ref":
                    # 跨句则不继承：中间有句号或分号，说明换了一句话
                    if target is not None and BOUNDARY.search(line[anchor:pos]):
                        target = None
                    tgt = target
                    if tgt is None and "同上" in line:
                        tgt = last_path
                    if tgt is None:
                        tgt = p
                    total += 1
                    if not tgt.exists():
                        skipped["目标文件不存在"] += 1
                        continue
                    if tgt not in cache:
                        cache[tgt] = sections(tgt)
                    if val in cache[tgt]:
                        ok += 1
                        if verbose:
                            print(f"  OK   {rel(p)}:{i}  §{val} -> {rel(tgt)}")
                    else:
                        bad.append((rel(p), i, val, rel(tgt), line.strip()[:70]))

    print(f"\n共校验 {total} 处：通过 {ok}，疑似错误 {len(bad)}\n")
    for f, i, r, t, ctx in bad:
        print(f"  ✗ {f}:{i}  §{r}  ->  {t}")
        print(f"      {ctx}")
    if bad:
        print()
    if skipped:
        print("未参与校验：")
        for k, v in skipped.items():
            print(f"  {k}: {v}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
