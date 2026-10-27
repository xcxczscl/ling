# ling 用户手册

> 面向**使用者**（而非插件开发者）的操作手册：如何构建、启动、访问界面、管理插件、排查问题。
> 开发者请另见 [CodeWiki.md](CodeWiki.md) 与 [docs/](docs/) 设计文档。

---

## 目录

1. [ling 是什么](#1-ling-是什么)
2. [准备环境](#2-准备环境)
3. [构建](#3-构建)
4. [首次启动](#4-首次启动)
5. [访问界面](#5-访问界面)
6. [命令行参考](#6-命令行参考)
7. [插件管理](#7-插件管理)
8. [数据与文件](#8-数据与文件)
9. [内置插件说明](#9-内置插件说明)
10. [常见问题（FAQ）](#10-常见问题faq)

---

## 1. ling 是什么

ling 是一个**零内容**的插件宿主程序：它自身不提供任何功能（没有界面、没有设置、没有业务概念），只提供加载插件、转发插件间调用、广播事件、检查权限、解析依赖、热更新的机制。

所有你能看到的功能——**界面、设置、插件管理、资源管理、记账**——全部由**插件**提供。

## 2. 准备环境

| 依赖 | 说明 |
|---|---|
| Rust 工具链 | `rustup` + `cargo`（stable，edition 2021） |
| 构建目标 | 构建插件动态库需对应平台的 MSVC/GNU 工具链链路 |

无 Rust 环境的安装方式：

```bash
# Windows：下载并运行 rustup-init.exe（https://rustup.rs）
# 或使用 winget：
winget install Rustlang.Rustup
```

安装后确认：

```bash
cargo --version
rustc --version
```

## 3. 构建

```bash
# 进入仓库根目录
cd ling/ling

# 构建整个 workspace（内核 + CLI + SDK + demo 插件 + 内置插件）
cargo build

# 发布构建（推荐用于正式使用）
cargo build --release
```

产物位置：

| 产物 | 路径 |
|---|---|
| 命令行 | `target/{debug,release}/ling`（`ling.exe`） |
| 插件动态库 | `target/{debug,release}/*.so` / `*.dll` / `*.dylib` |

> 内置插件需先构建出动态库，内核才能发现并首启安装三件套；缺失会被自动跳过。

## 4. 首次启动

```bash
# 前台启动宿主（首启自动安装内置三件套）
cargo run -p ling-cli -- serve

# 或直接用编译出的二进制
./target/release/ling serve
```

启动后输出类似：

```
ling 已启动：管理端口 127.0.0.1:7900（界面 127.0.0.1:7910，数据根 C:\Users\...\.local\share\ling）
```

要点：

- **管理端口**：默认 `7900`，被占用会自动顺延（`0` 表示随机）。
- **界面端口**：`core-ui` 装好后默认 `7910` 起，占用顺延。
- **首启自动装三件套**：core-ui、core-settings、core-plugin-manager（均可卸载）。

### 指定内置插件目录

```bash
ling serve --builtin-dir E:\path\to\builtin-plugins
```

缺省时从当前目录向上自动发现 `builtin-plugins/`。

### 自定义数据目录

数据根目录解析优先级：`LING_HOME` > `$XDG_DATA_HOME/ling` > `~/.local/share/ling`。

```bash
# Windows（PowerShell）
$env:LING_HOME = "D:\ling-data"; ling serve
```

## 5. 访问界面

启动后浏览器打开界面地址（默认 `http://127.0.0.1:7910`）：

- **插件管理页**：查看已装插件、安装/卸载/启停/升级。
- **设置页**：插件暴露的设置项。
- **作业页**（安装 core-job 后）：查看长任务进度。
- **日志页**（安装 core-log 后）：查看插件日志。

界面认证：`/rpc` 网关支持 `X-Ling-Token` 头、`?token=` 查询参数、cookie 三种方式，token 取自 `session.json`。

## 6. 命令行参考

除 `serve` 外，所有命令都要**先启动宿主**（`ling serve`），再经管理端口连接操作：

```bash
ling serve [--builtin-dir <路径>]      前台启动宿主（首启装三件套）
ling plugins                          列出已安装插件及状态
ling install <目录|zip>               安装插件并触发调和
ling uninstall <编号> [--purge]       卸载（--purge 连数据删除）
ling enable  <编号>                   启用插件
ling disable <编号>                   停用插件
ling reload  <编号>                   热更新（新版本已在插件目录）
ling restart <编号>                   重启插件
ling logs <编号> [条数]               读插件日志（需 core-log）
ling shutdown                         优雅停止宿主
ling --version                        输出版本号
ling --help                           显示帮助
```

### 示例

```bash
# 安装 demo 插件（目录或 zip）
ling install plugins/demo-a

# 查看已装插件
ling plugins

# 停用 / 启用
ling disable demo-a
ling enable demo-a

# 读取 core-ui 最近 100 条日志
ling logs core-ui 100

# 优雅停止
ling shutdown
```

## 7. 插件管理

### 7.1 安装

支持**目录**或 **zip 包**。安装会：暂存 → 校验清单 → 拷贝进数据目录 → 环检测 → 触发调和（依赖齐备自动激活）。

```bash
ling install /path/to/my-plugin
ling install /path/to/my-plugin.zip
```

### 7.2 卸载

```bash
ling uninstall my-plugin          # 保留插件数据
ling uninstall my-plugin --purge  # 连数据一并删除
```

卸载会级联停用激活中的依赖方插件（依赖方工件保留）。

### 7.3 热更新

新版本先放到插件数据目录（`.../ling/plugins/<编号>/`），再执行：

```bash
ling reload my-plugin
```

失败会自动回滚到旧版本，旧版本继续服务。

### 7.4 能力授权

插件声明能力（`fs`/`network`/`ui`/`shell`），宿主可撤销/恢复（经管理端口或插件管理页）反向管控某插件的权限。

## 8. 数据与文件

数据根目录（`LING_HOME` 覆盖）下的关键内容：

| 路径 | 内容 |
|---|---|
| `registry.json` | 管理端口、界面端口、停用名单、能力撤销名单 |
| `session.json` | 会话口令（界面/CLI 认证用，0600） |
| `plugins/<编号>/plugin.toml` | 已安装插件的清单 |
| `plugins/<编号>/data/` | 插件私有数据（卸载非 purge 保留） |
| `plugins/<编号>/logs/` | core-log 收集的日志 |

## 9. 内置插件说明

| 插件 | 是否默认装 | 作用 |
|---|---|---|
| core-ui | 是（三件套） | 界面壳：本机网页 + `/rpc` 网关 |
| core-settings | 是（三件套） | 设置存储（SQLite）+ 设置页 |
| core-plugin-manager | 是（三件套） | 插件管理页 + 管理端口转发 |
| core-job | 否（可选） | 长任务登记与进度广播 |
| core-log | 否（可选） | 插件日志统一写入/查询/轮转 |

## 10. 常见问题（FAQ）

**Q：`ling plugins` 报"读不到管理端口，宿主是否在运行？"**
A：先运行 `ling serve` 启动宿主，再执行管理命令。

**Q：启动后界面打不开？**
A：确认 core-ui 已安装且已激活（`ling plugins` 查看）；端口被占用会自动顺延，以启动输出为准。

**Q：某个插件显示"未激活"？**
A：通常是依赖未满足或版本不匹配。安装/启用其依赖插件后会自动调和激活。

**Q：`ling logs` 无输出？**
A：需先安装 core-log 插件；未安装时该命令不可用。

**Q：想彻底重置 ling？**
A：停止宿主后删除数据根目录（默认 `~/.local/share/ling`）。