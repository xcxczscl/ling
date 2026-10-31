# VC++ 运行库（MSVC 动态 CRT）随包分发

`ling.exe` 与侧车 `core-ui.exe` 用 `/MD` 动态链接 CRT（项目在 `tools/cmake/LingOptions.cmake`
里统一成动态 CRT，不能改 `/MT`，否则进程内插件跨 DLL 传对象会崩）。它们依赖
`MSVCP140.dll`、`VCRUNTIME140.dll`、`VCRUNTIME140_1.dll` 等，这些**不是** Win10/11 系统
自带，干净机器上会报「找不到 MSVCP140.dll」。UCRT 的 `api-ms-win-crt-*.dll` 是系统自带，不用带。

## 来源

- 本目录 10 个 DLL 取自 Visual Studio 的可再分发目录：
  `VC/Redist/MSVC/14.51.36231/x64/Microsoft.VC145.CRT/`（微软许可再分发）。
- 打包时 `tools/package.py` 放**两份**：包根（宿主 `ling.exe` + 进程内插件）、
  `plugins/core-ui/`（侧车是独立进程，Windows DLL 搜索只看它自己所在目录）。

## 升级规则

升级运行库 = 从 VS 的 `VC/Redist/MSVC/<新版本>/x64/Microsoft.VC*.CRT/` 整目录覆盖这里，
然后重跑 `python tools/package.py zip` 与 `iss`。文件名不变，只是内容换版本。
