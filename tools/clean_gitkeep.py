"""删除仓库里的 .gitkeep 占位文件。

用法：
    python tools/clean_gitkeep.py            # 直接删除
    python tools/clean_gitkeep.py --dry-run  # 只列出，不删

跳过 old/ 与 other/：前者只读，后者是本地私有内容。
"""

import argparse
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

SKIP = {"old", "other", ".git"}


def main():
    parser = argparse.ArgumentParser(description="删除 .gitkeep 占位文件")
    parser.add_argument("--dry-run", action="store_true", help="只列出，不删除")
    args = parser.parse_args()

    found = [p for p in ROOT.rglob(".gitkeep")
             if not (set(p.relative_to(ROOT).parts) & SKIP)]
    for p in found:
        print(p.relative_to(ROOT).as_posix())
        if not args.dry_run:
            p.unlink()

    print(f"共 {len(found)} 个" + ("（未删除）" if args.dry_run else "已删除"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
