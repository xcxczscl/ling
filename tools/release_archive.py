"""生成正式版发行归档（7z）。

用法：
    python tools/release_archive.py ling                    # 宿主，版本从 CMakeLists.txt 读
    python tools/release_archive.py ling --version 0.1.0 --docs docs/ling/archive/0.1.0
    python tools/release_archive.py plugin <插件名> --version 1.0.0
    python tools/release_archive.py ... --sevenzip "C:/Program Files/7-Zip/7z.exe"

约定：
  - 只做正式版：版本号必须是三段式，不能带 -bN。
  - 输出到仓库根 archive/，该目录不进版本库。
  - 宿主包内容：docs/、main/（含 builtin-plugins/，内置插件随宿主任装）、dist/、包内 README.md。
  - 插件包内容：该插件的文档、源码、发行文件、包内 README.md。
"""

import argparse
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT_DIR = ROOT / "archive"

VERSION_RE = re.compile(r'version\s*=\s*"([^"]+)"')


def fail(msg):
    print(f"错误：{msg}")
    return 1


def host_version():
    text = (ROOT / "CMakeLists.txt").read_text(encoding="utf-8")
    m = re.search(r'set\(LING_VERSION_MAJOR (\d+)\)\s*'
                  r'set\(LING_VERSION_MINOR (\d+)\)\s*'
                  r'set\(LING_VERSION_PATCH (\d+)\)', text)
    if not m:
        return None
    return f"{m.group(1)}.{m.group(2)}.{m.group(3)}"


def builtin_versions():
    """返回 [(插件名, 版本)]，读取各内置插件的 plugin.toml。"""
    base = ROOT / "main" / "builtin-plugins"
    out = []
    for manifest in sorted(base.glob("*/plugin.toml")):
        m = VERSION_RE.search(manifest.read_text(encoding="utf-8"))
        out.append((manifest.parent.name, m.group(1) if m else "未知"))
    return out


def check_version(name, version):
    if "-" in version or not re.fullmatch(r"\d+\.\d+\.\d+", version):
        return fail(f"{name} 版本 {version!r} 不是三段式正式版号（不能带 -bN）")
    return 0


def find_7z(explicit):
    if explicit:
        p = Path(explicit)
        return p if p.exists() else None
    found = shutil.which("7z") or shutil.which("7za")
    if found:
        return Path(found)
    for p in (Path(r"C:\Program Files\7-Zip\7z.exe"),
              Path(r"C:\Program Files (x86)\7-Zip\7z.exe")):
        if p.exists():
            return p
    return None


def ignore_gitkeep(directory, names):
    return {n for n in names if n == ".gitkeep"}


def ignore_archive(directory, names):
    """复制源码或文档时跳过 archive/ 与占位文件。"""
    return {n for n in names if n in ("archive", ".gitkeep")}


def copy(src, dst, ignore=None):
    if not src.exists():
        return
    shutil.copytree(src, dst, ignore=ignore, dirs_exist_ok=True)


def copy_docs(src, dst):
    """docs/ 取 docs/ling 的内容，跳过 archive/ 与 log/。"""
    dst.mkdir(parents=True, exist_ok=True)
    for child in sorted(src.iterdir()):
        if child.name in ("archive", "log", ".gitkeep"):
            continue
        if child.is_dir():
            copy(child, dst / child.name, ignore_gitkeep)
        else:
            shutil.copy2(child, dst / child.name)


def copy_common(dst):
    """公共文档与每个文档都相关，每个发行包都带一份。"""
    copy(ROOT / "docs" / "common", dst, ignore_gitkeep)


def copy_dist(version, dst):
    """dist/ 只取本版本的发行文件；不带版本号的辅助文件一并带上。"""
    src = ROOT / "dist"
    if not src.exists():
        return
    dst.mkdir(parents=True, exist_ok=True)
    for f in sorted(src.rglob("*")):
        if not f.is_file():
            continue
        if re.search(r"\d+\.\d+\.\d+", f.name) and version not in f.name:
            continue
        shutil.copy2(f, dst / f.relative_to(src))


def write_ling_readme(staging, version):
    rows = "\n".join(f"| `{name}` | {ver} |" for name, ver in builtin_versions())
    text = f"""# ling {version} 发行归档

本包是 ling 宿主 {version} 的正式版归档，包含文档、源码与发行文件。源码含五个内置插件。

## 包内结构

| 路径 | 内容 |
|---|---|
| `docs/` | ling 的文档，对应仓库的 `docs/ling/`（不含 `archive/` 与 `log/`），另带 `docs/common/` |
| `main/` | 宿主与内置插件源码，不含 `archive/` |
| `dist/` | 本版本的发行文件 |
| `README.md` | 本文件 |

## 内置插件版本

内置插件不随宿主版本号发布，各自独立。版本写在 `main/builtin-plugins/<插件名>/plugin.toml`。

| 内置插件 | 版本 |
|---|---|
{rows}

## 还原方式

1. 解压后得到 `docs/`、`main/`、`dist/` 三个目录。
2. 用源码构建：构建步骤见 `docs/ling/main/05-构造与开发指南.md` §7。
3. 不想构建就直接用 `dist/` 里的便携 zip 或安装包。

## 说明

- 宿主版本写在根 `CMakeLists.txt` 的 `LING_VERSION_MAJOR` / `LING_VERSION_MINOR` / `LING_VERSION_PATCH`。
- 插件只需要在清单里写 `min_host` 声明最低宿主版本；宿主不需要声明它支持哪些插件。
- 源码归档在仓库的 `main/archive/`，本包不是归档，是可直接使用的发行包。
"""
    (staging / "README.md").write_text(text, encoding="utf-8")


def write_plugin_readme(staging, name, version):
    text = f"""# {name} {version} 发行归档

本包是插件 {name} 的正式版归档，包含文档、源码与发行文件。

## 包内结构

| 路径 | 内容 |
|---|---|
| `docs/` | 该插件的文档，对应仓库的 `docs/plugins/{name}/`；另带 `docs/common/` |
| `source/` | 该插件的源码，对应仓库的 `plugins/{name}/` |
| `dist/` | 当时的发行文件（存在时才有） |
| `README.md` | 本文件 |

## 说明

插件版本写在插件的 `plugin.toml`。插件与哪个宿主兼容由清单里的 `api` 与 `min_host` 决定，不由宿主版本声明。

发行包由 `tools/release_archive.py` 生成。
"""
    (staging / "README.md").write_text(text, encoding="utf-8")


def pack(sevenzip, staging, output):
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    if output.exists():
        output.unlink()
    cmd = [str(sevenzip), "a", "-t7z", "-mx=9", "-y", str(output), "."]
    result = subprocess.run(cmd, cwd=staging, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, text=True)
    if result.returncode != 0:
        print(result.stdout)
        return fail(f"7z 返回 {result.returncode}")
    print(f"已生成 {output}（{output.stat().st_size / 1024 / 1024:.1f} MB）")
    return 0


def build_ling(args, sevenzip):
    version = args.version or host_version()
    if not version:
        return fail("读不到宿主版本，请用 --version 指定")
    rc = check_version("宿主", version)
    if rc:
        return rc

    staging = Path(tempfile.mkdtemp(prefix="ling-release-"))
    try:
        docs = Path(args.docs) if args.docs else ROOT / "docs" / "ling"
        copy_docs(docs, staging / "docs")
        copy_common(staging / "docs" / "common")
        copy(ROOT / "main", staging / "main", ignore_archive)
        copy_dist(version, staging / "dist")
        write_ling_readme(staging, version)
        return pack(sevenzip, staging, OUT_DIR / f"ling-{version}-archive.7z")
    finally:
        shutil.rmtree(staging, ignore_errors=True)


def build_plugin(args, sevenzip):
    name = args.name
    docs_dir = ROOT / "docs" / "plugins" / name
    source_dir = ROOT / "plugins" / name
    if not docs_dir.exists() and not source_dir.exists():
        return fail(f"docs/plugins/{name}/ 与 plugins/{name}/ 都不存在")
    if not args.version:
        manifest = source_dir / "plugin.toml"
        m = VERSION_RE.search(manifest.read_text(encoding="utf-8")) if manifest.exists() else None
        if not m:
            return fail("读不到插件版本，请用 --version 指定")
        version = m.group(1)
    else:
        version = args.version
    rc = check_version(name, version)
    if rc:
        return rc

    staging = Path(tempfile.mkdtemp(prefix="plugin-release-"))
    try:
        copy(docs_dir, staging / "docs",
             ignore=lambda d, n: {x for x in n if x in ("archive", "log", ".gitkeep")})
        copy_common(staging / "docs" / "common")
        copy(source_dir, staging / "source",
             ignore=lambda d, n: {x for x in n if x in ("archive", ".gitkeep")})
        plugin_dist = ROOT / "dist" / name
        if plugin_dist.exists():
            copy(plugin_dist, staging / "dist")
        write_plugin_readme(staging, name, version)
        return pack(sevenzip, staging, OUT_DIR / f"{name}-{version}-archive.7z")
    finally:
        shutil.rmtree(staging, ignore_errors=True)


def main():
    parser = argparse.ArgumentParser(description="生成正式版发行归档（7z）")
    sub = parser.add_subparsers(dest="target", required=True)

    ling = sub.add_parser("ling", help="宿主发行归档")
    ling.add_argument("--version", help="宿主版本，默认读 CMakeLists.txt")
    ling.add_argument("--docs", help="文档目录，默认 docs/ling；回填旧版时指定旧快照里的 ling 目录")
    ling.add_argument("--sevenzip", help="7z 可执行文件路径")

    plugin = sub.add_parser("plugin", help="插件发行归档")
    plugin.add_argument("name", help="插件名，对应 docs/plugins/ 与 plugins/ 下的子目录")
    plugin.add_argument("--version", help="插件版本，默认读 plugin.toml")
    plugin.add_argument("--sevenzip", help="7z 可执行文件路径")

    args = parser.parse_args()

    sevenzip = find_7z(args.sevenzip)
    if not sevenzip:
        return fail("找不到 7z，请用 --sevenzip 指定")

    if args.target == "ling":
        return build_ling(args, sevenzip)
    return build_plugin(args, sevenzip)


if __name__ == "__main__":
    sys.exit(main())
