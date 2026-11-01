"""把构建产物打成可分发的东西。

目前支持两种：
  1. 便携 zip —— 解压即用，双击 ling.exe 打开（或 lingw.exe 桌面启动器）
  2. Inno Setup 安装包 —— 生成 .iss 脚本，交给 ISCC.exe 编译（如果机器上装了）

用法：
    python tools/package.py zip   [--preset win-release] [--desktop]
    python tools/package.py iss   [--preset win-release]

desktop 标志：zip 里带上 desktop 版 core-ui（默认构建是 webui）。
"""

import argparse
import os
import pathlib
import shutil
import subprocess
import sys
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[1]

# 随包分发、但只属于内置插件的，demo 示例不打包。
BUILTIN_PLUGINS = ["core-settings", "core-plugin-manager", "core-log", "core-job", "core-ui"]

# 便携标记：exe 同目录有这个空文件，宿主就把数据存到 <程序目录>/data（跟程序走），
# 否则存 %APPDATA%\ling。便携 zip 固定带它，安装包靠「便携模式」选项决定带不带。
PORTABLE_MARK = "ling.portable"

# VC++ 运行库（MSVC 动态 CRT）。ling.exe 与侧车 core-ui.exe 都是 /MD 编译，
# 干净机器上没有这些 DLL 会启动失败（报「找不到 MSVCP140.dll」）。UCRT 的
# api-ms-win-crt-*.dll 是 Win10/11 系统自带，不用打包；这里只带非系统组件。
# 来源：VS 的 VC/Redist/MSVC/<版本>/x64/Microsoft.VC145.CRT（微软许可再分发）。
CRT_DIR = ROOT / "third_party" / "vc-redist"


def crt_dlls():
    """返回要随包分发的 CRT DLL（按文件名排序，稳定顺序）。"""
    return sorted(CRT_DIR.glob("*.dll"))


def version_string(exe):
    out = subprocess.run([str(exe), "--version"], capture_output=True, text=True).stdout.strip()
    # 形如 "ling 0.1.0-b1"，取后半段。
    return out.split()[-1] if out else "0.0.0"


def build_zip(preset, desktop):
    run = ROOT / "run" / preset
    exe = run / "ling.exe"
    if not exe.exists():
        print(f"找不到 {exe}，先构建 {preset}")
        return 1

    version = version_string(exe)
    dist = ROOT / "dist"
    dist.mkdir(exist_ok=True)
    archive = dist / f"ling-{version}-win64.zip"
    if desktop:
        archive = dist / f"ling-{version}-win64-desktop.zip"

    print(f"打包便携 zip → {archive.name}")
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.write(exe, "ling.exe")
        # 便携标记：解压即用版本数据跟程序走（存 <程序目录>/data）。
        zf.writestr(PORTABLE_MARK, "")
        # VC++ 运行库：放两份——根（宿主 ling.exe + 进程内插件）、
        # plugins/core-ui（侧车 core-ui.exe 是独立进程，DLL 搜索只看自己目录）。
        for dll in crt_dlls():
            zf.write(dll, dll.name)
            zf.write(dll, "plugins/core-ui/" + dll.name)
        # 内置插件
        for pid in BUILTIN_PLUGINS:
            src = run / "plugins" / pid
            if not src.exists():
                continue
            for file in src.rglob("*"):
                if file.is_file():
                    zf.write(file, "plugins/" + pid + "/" + str(file.relative_to(src)).replace("\\", "/"))
        # Python SDK
        sdk = run / "python"
        if sdk.exists():
            for file in sdk.rglob("*"):
                if file.is_file():
                    zf.write(file, "python/" + str(file.relative_to(sdk)).replace("\\", "/"))
        # Python 嵌入运行时
        rt = run / "runtime" / "python"
        if rt.exists():
            for file in rt.rglob("*"):
                if file.is_file():
                    zf.write(file, "runtime/python/" + str(file.relative_to(rt)).replace("\\", "/"))

    print(f"完成：{archive}（{archive.stat().st_size // 1024} KB）")
    return 0


def build_iss(preset):
    """生成 Inno Setup 脚本，交给机器上的 ISCC.exe 编译。"""
    run = ROOT / "preset" if False else ROOT / "run" / preset
    exe = run / "ling.exe"
    if not exe.exists():
        print(f"找不到 {exe}，先构建 {preset}")
        return 1

    version = version_string(exe)
    # 便携标记的空文件，作为 Inno 的源；勾选「便携模式」任务时才装。
    portable_stub = ROOT / "dist" / (PORTABLE_MARK + ".stub")
    portable_stub.write_bytes(b"")
    # 收集要装的文件，用 Inno 的 [Files] 语法表达。
    files = []
    files.append(("ling.exe", str(exe)))
    # VC++ 运行库：同 zip，根 + plugins/core-ui 两份。
    for dll in crt_dlls():
        files.append((dll.name, str(dll)))
        files.append((f"plugins/core-ui/{dll.name}", str(dll)))
    for pid in BUILTIN_PLUGINS:
        src = run / "plugins" / pid
        if not src.exists():
            continue
        for file in src.rglob("*"):
            if file.is_file():
                rel = str(file.relative_to(src)).replace("\\", "/")
                files.append((f"plugins/{pid}/{rel}", str(file)))
    for sub in ("python", "runtime/python"):
        src = run / sub.replace("/", "/")
        if not src.exists():
            continue
        for file in src.rglob("*"):
            if file.is_file():
                rel = str(file.relative_to(src)).replace("\\", "/")
                files.append((f"{sub}/{rel}", str(file)))

    lines = ['[Setup]',
             f'AppName=ling',
             f'AppVersion={version}',
             # 用户级安装、免管理员：数据能写进程序目录，「便携模式」才成立。
             'DefaultDirName={localappdata}\\Programs\\ling',
             'PrivilegesRequired=lowest',
             'DefaultGroupName=ling',
             'ArchitecturesAllowed=x64compatible',
             'ArchitecturesInstallIn64BitMode=x64compatible',
             'OutputDir=' + str(ROOT / 'dist'),
             f'OutputBaseFilename=ling-{version}-win64-setup',
             'Compression=lzma2',
             'SolidCompression=yes',
             '',
             '[Tasks]',
             'Name: "portable"; Description: "便携模式（数据存程序目录）"; GroupDescription: "数据存放位置："; Flags: unchecked',
             '',
             '[Files]']
    for dest, src in files:
        lines.append(f'Source: "{src}"; DestDir: "{{app}}\\{pathlib.Path(dest).parent.as_posix()}"; Flags: ignoreversion')
    # 便携标记：勾选「便携模式」任务才写入，装成 ling.portable 本名。
    lines.append(f'Source: "{portable_stub}"; DestDir: "{{app}}"; DestName: "{PORTABLE_MARK}"; Flags: ignoreversion; Tasks: portable')
    lines += ['', '[Icons]',
              'Name: "{group}\\ling"; Filename: "{app}\\ling.exe"',
              'Name: "{group}\\卸载 ling"; Filename: "{uninstallexe}"',
              '']

    iss = ROOT / "dist" / "ling.iss"
    iss.write_text("\n".join(lines), encoding="utf-8")
    print(f"已生成 Inno Setup 脚本：{iss}")

    candidates = [
        pathlib.Path(r"C:\Program Files (x86)\Inno Setup 6\ISCC.exe"),
        pathlib.Path(r"C:\Program Files\Inno Setup 6\ISCC.exe"),
        pathlib.Path(os.path.expandvars(r"%LOCALAPPDATA%\Programs\Inno Setup 6\ISCC.exe")),
    ]
    iscc = next((c for c in candidates if c.exists()), None)
    if iscc:
        subprocess.run([str(iscc), str(iss)], check=True)
        print("安装包编译完成。")
    else:
        print("机器上没有 Inno Setup，跳过了安装包编译（脚本已生成，装了之后直接跑 ISCC 即可）。")
    return 0


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("kind", choices=["zip", "iss"])
    parser.add_argument("--preset", default="win-release")
    parser.add_argument("--desktop", action="store_true")
    args = parser.parse_args()

    if args.kind == "zip":
        return build_zip(args.preset, args.desktop)
    return build_iss(args.preset)


if __name__ == "__main__":
    sys.exit(main())
