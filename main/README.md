# main/ —— 宿主程序与内置插件

`ling` 宿主的全部代码：内核、命令行入口、平台抽象层、Python 侧车 SDK，以及五个内置插件。

## 目录总览

| 位置 | 是什么 |
|---|---|
| `app/` | 命令行入口（`ling.exe`），只有 `main.cpp` 一个文件 |
| `src/` | 内核实现，编成静态库 `ling_core`，一个职责一个 `.cpp` |
| `include/ling/` | 对外头文件；插件边界只认 `plugin_api.h` |
| `Sys/` | 平台抽象层：Windows 与 POSIX 各一份实现 |
| `SDK/python/` | Python 侧车 SDK（`ling_sidecar.py`） |
| `builtin-plugins/` | 五个内置插件（含 `core-ui`） |
| `CMakeLists.txt` | 本目录的构建脚本 |

## app/ —— 命令行入口

| 文件 | 职责 |
|---|---|
| `main.cpp` | 所有子命令的入口：serve、install、uninstall、plugins、logs、job、open、doctor、reload、restart、shutdown。除了 serve，其余子命令都经管理端口走 `kernel.*` 方法 |

## src/ —— 内核实现（静态库 ling_core）

一个职责一个源文件，头文件在 `include/ling/`。平台相关的系统调用不在这里，在 `Sys/`。

| 文件 | 职责 |
|---|---|
| `kernel.cpp` | 内核骨架：装载表、路由表、装载/卸载、侧车调度、事件心跳 |
| `gate.cpp` | 能力门禁：注册方法时校验「方法声明的能力 ⊆ 清单声明」 |
| `manifest.cpp` | 插件清单解析与校验 |
| `depgraph.cpp` | 依赖图、版本匹配、环检测、计划计算 |
| `router.cpp` | 方法路由表：注册、注销、调用转发 |
| `event.cpp` | 事件订阅表与派发 |
| `ledger.cpp` | 效应账本：每次改变宿主状态的动作登记一条，卸载时倒序撤销 |
| `sidecar_proto.cpp` | 侧车协议：一行一个 JSON 对象（jsonl） |
| `admin_server.cpp` | 管理端口服务端：回环监听、首帧认证、帧转发到 `kernel.*` |
| `admin_client.cpp` | 管理端口客户端：命令行用，找端口、认证、发帧读帧 |
| `ui_server.cpp` | 界面 HTTP 服务端：core-ui 用，回环监听、口令认证、`/api/*` 转管理端口 |
| `log.cpp` | 内核日志：stderr + `<数据根>/logs/kernel-*.log` |
| `version.cpp` | 版本信息（编译期常量） |

关于 `ui_server.cpp`：它逻辑上是 **core-ui 插件的一部分**（界面服务端），但物理上放在 `ling_core` 里——因为 core-ui 是侧车插件、链接 `ling_core`，服务端代码放内核里能被它直接复用，不用单独编译一份。同理 `sidecar_host.cpp` 的进程实现在 `Sys/`（按平台拆分），声明在 `include/ling/sidecar_host.hpp`。

## include/ling/ —— 对外头文件

| 头文件 | 对应职责 |
|---|---|
| `plugin_api.h` | **插件边界的唯一契约**（纯 C），插件只认这一个 |
| `gate.hpp` | 能力门禁 |
| `kernel.hpp` | 内核骨架 |
| `manifest.hpp` | 清单 |
| `depgraph.hpp` | 依赖图 |
| `router.hpp` | 路由 |
| `event.hpp` | 事件 |
| `ledger.hpp` | 账本 |
| `sidecar_host.hpp` | 侧车进程（声明，实现在 `Sys/`） |
| `sidecar_proto.hpp` | 侧车协议 |
| `admin.hpp` | 管理端口 |
| `ui_server.hpp` | 界面 HTTP 服务端 |
| `log.hpp` | 日志 |
| `errors.hpp` | 错误码常量 |
| `version.hpp` | 版本 |
| `sys.hpp` | 平台抽象层统一接口 |

## Sys/ —— 平台抽象层

所有「和操作系统打交道」的动作都收口在这里，业务代码只 include `ling/sys.hpp`，不写 `#ifdef _WIN32`。

| 位置 | 内容 |
|---|---|
| `Windows/` | Windows 实现：`sys.cpp`（平台函数）+ `sidecar_host.cpp`（侧车进程） |
| `Posix/` | POSIX（Linux 等）实现，同名文件（还没在 Linux 上编过） |

## SDK/python/ —— Python 侧车 SDK

`ling_sidecar.py`：给 Python 写的侧车插件用的 SDK，随宿主分发，插件经 `LING_SDK_DIR` 找到它。

## builtin-plugins/ —— 内置插件

| 插件 | 类型 | 职责 |
|---|---|---|
| `core-settings` | 进程内动态库 | 设置存取（含界面口令开关） |
| `core-plugin-manager` | 进程内动态库 | 插件列表查询 |
| `core-log` | 进程内动态库 | 收集插件日志 |
| `core-job` | 进程内动态库 | 五段 cron 定时任务 |
| `core-ui` | 侧车（可执行文件） | 界面：webui（浏览器）+ desktop（WebView2 窗口） |
