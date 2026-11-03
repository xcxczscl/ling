"""清理仓库里的 .gitkeep 占位文件。

默认只列出，不动任何文件；要真的删除必须显式加 --apply。

用法：
    python tools/clean_gitkeep.py           # 只列出，不删除
    python tools/clean_gitkeep.py --apply   # 打印后删除

只清理 plugins/ 与 docs/ 这两棵白名单目录树；build/、dist/、third_party/、
old/、other/、.git/ 以及 run/、data/、archive/ 等用户数据目录在任何层级
出现都会被跳过。
"""

import argparse
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# 只在这两棵目录树里清理，范围写死，避免误删到构建产物或别人放的数据。
CLEAN_DIRS = ("plugins", "docs")
# 任意层级出现这些名字都跳过：构建产物、依赖、历史快照、私有内容与用户数据。
SKIP_PARTS = {"build", "dist", "third_party", "old", "other", ".git",
              "run", "data", "archive", "node_modules"}


def candidates():
    found = []
    for name in CLEAN_DIRS:
        base = ROOT / name
        if not base.is_dir():
            continue
        for path in base.rglob(".gitkeep"):
            if set(path.relative_to(ROOT).parts) & SKIP_PARTS:
                continue
            found.append(path)
    return sorted(found)


def main():
    parser = argparse.ArgumentParser(description="清理 .gitkeep 占位文件（默认只列出）")
    parser.add_argument("--apply", action="store_true", help="真的删除；不加时只列出")
    args = parser.parse_args()

    found = candidates()
    for path in found:
        print(path.relative_to(ROOT).as_posix())
        if args.apply:
            path.unlink()

    if args.apply:
        print(f"共 {len(found)} 个，已删除")
    else:
        print(f"共 {len(found)} 个（未删除；加 --apply 才会删）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
