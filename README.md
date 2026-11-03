# ling

> 零内容的插件宿主：本身不提供任何功能，全部实用功能由插件提供。给自己用的单机工具。

> 当前阶段：**M6 部分完成**（Windows 端完成，Linux/Android 未做；M0–M5 已通过）
>
> 当前版本：**`0.1.0`**（版本规则见 [`docs/common/02-版本号与兼容约定.md`](docs/common/02-版本号与兼容约定.md)）

---

## 这是什么

ling 是一个插件宿主程序。它自己不实现任何业务功能，只提供四件事：把插件装起来、把插件之间的调用转过去、把事件广播出去、在插件卸载或更新时把环境恢复原样。

功能全部由插件提供——资源管理、播放、以后另加的记账，都是插件。想加功能就加一个插件，不用改宿主。

它给自己用：单机、单用户、几十个插件的规模。不是框架，不做生态，不做多人协作。

目标与非目标：

| 类别 | 内容 |
|---|---|
| 目标 | 个人使用的工具；加功能只动插件不动宿主；装插件、改插件都不用重启宿主；单个插件出问题不影响其他插件；多平台可用（Windows 为主、Linux 对齐、Android 受限降级） |
| 非目标 | 不做大型系统（无分布式、无多节点）；不做多人协作与权限分级；不做插件市场与在线分发；不做通用开发框架 |

---

## 现在到哪一步

**M0 至 M5 已全部通过；M6 部分完成：Windows 端已出正式版 `0.1.0`，Linux 与 Android 未做**（设计定稿、最小闭环、可装可卸、能改能换、双轨、Windows 端可发布）。各段的判定记录见 [`docs/ling/log/`](docs/ling/log/)，里程碑划分见 [`docs/ling/main/10-路线图与里程碑.md`](docs/ling/main/10-路线图与里程碑.md)。

M4 的双轨指两条插件轨道：进程内动态库，和独立进程的「侧车」。侧车支持 C++ 与 Python 两种运行时、崩溃重启、按需启停。Windows 上验收 32 项全过；**Linux 那半边代码写了但没在 Linux 上编译过**，第一次上 Linux 要重编并跑一遍同一份验收脚本。

| 项目 | 设计文档 | 代码 |
|---|---|---|
| ling 宿主（`main/`） | 已定稿（`main/` 00–03、05–12 各一份、04-详细设计四份、README 与 M0 各一份，共 18 份；不含 `plugin-support/`） | 装、卸、调用、撤销、事件广播、管理端口、`serve`/`shutdown` 全部可用 |
| 内置插件（`main/builtin-plugins/`） | 四个已定稿，`core-ui` 初定（docs/ling/README.md） | `core-settings`、`core-plugin-manager`、`core-log`、`core-job`、`core-ui` 五个 |
| 侧车轨道（`main/sys/` + `src/sidecar_proto.cpp`） | 已定稿（plugin-support/01 §8） | 协议、握手、代登记、调用、崩溃重启、按需启停；C++ 与 Python 两种运行时 |
| Python SDK（`main/SDK/python/`） | —— | `ling_sidecar.py`，随宿主分发 |
| 界面（`main/builtin-plugins/core-ui/`） | 初定，版本 `1.1.0-b1`（`builtin-plugins/core-ui/`） | `core-ui`：第五个内置插件；`webui`（浏览器）与 `desktop`（WebView2 窗口）两种形态 |
| 第三方插件（`plugins/`） | 各插件自己的 `docs/plugins/<插件名>/` | 目前没有插件；示例插件已删除 |

MVP 能跑通的最短路径：

```bash
ling serve                          # 前台起宿主，管理端口 127.0.0.1:7900
ling plugins                        # 五个内置插件 activated
ling open                           # 浏览器打开 core-ui 界面
ling shutdown
```

示例插件（demo-a、demo-caller、demo-b、demo-bench、demo-sidecar、demo-py）已删除。想完整走一遍装卸与调用，按 [`docs/ling/plugin-support/03-最小插件样板.md`](docs/ling/plugin-support/03-最小插件样板.md) 自己构建一个样板插件，再执行 `ling install <插件目录>`、`ling logs <插件名>`。

验收与基准：

```bash
python tests/acceptance/acceptance.py --script all --preset win-release
python tests/acceptance/l5_ui.py --preset win-release       # 界面（webui 版）
```

界面是 `core-ui`（第五个内置插件），`ling serve` 启动时自动拉起，`ling open` 在浏览器里打开。默认是 `webui` 形态；`desktop` 形态（WebView2 窗口）可选编译（`LING_CORE_UI_DESKTOP=ON`）。

技术栈（见 [`docs/ling/main/02-技术选型.md`](docs/ling/main/02-技术选型.md)）：宿主与进程内插件用 C++17；侧车插件可用任意语言，core-ui 用 C++ 写成侧车。

改文档的规矩见 [`RULES.md`](RULES.md)（必须遵守的硬规则）和 [`AGENTS.md`](AGENTS.md)（协作方式）：需求到设计到测试的引用一律写完整路径，改完跑 `tools/` 下三个校验脚本。

---

## 目录结构

`docs/` 单独在下一节「文档怎么读」讲，这里列除文档外的文件与目录。

### 根目录

| 文件 | 作用 |
|---|---|
| `CMakeLists.txt` | 顶层构建入口：版本号、产物落点 `run/<preset>/`、统一编译选项，引用 third_party / main / plugins / tests |
| `CMakePresets.json` | 构建预设：win-debug、win-release、linux-debug、linux-release、android-release |
| `RULES.md` | 硬规则（红线、改动边界、Git 强制项、依赖、测试、密钥、审核约束） |
| `AGENTS.md` | 协作方式（先问、工作流程、提交与推送、审核、沟通、汇报） |
| `archive/` | 正式版发行归档（7z），不进版本库 |
| `.clang-format` / `.clang-tidy` | 代码格式化与静态检查配置 |
| `.gitignore` | 忽略构建产物（run/、dist/）与大体积第三方包 |
| `README.md` | 本文件 |

### main/ — 宿主程序与内置插件

宿主 `ling.exe` 和五个内置插件都在这里。

| 位置 | 作用 |
|---|---|
| `main/CMakeLists.txt` | 宿主、内核、平台层的构建；复制 Python SDK 与嵌入运行时到产物目录 |
| `main/app/main.cpp` | 命令行入口：serve、open、logs、job、shutdown、plugins、install、uninstall、enable、disable、plan、doctor、reload、restart 共 14 个子命令；`--version`、`--help` 是本地参数 |
| `main/src/` | 内核实现，编成静态库 `ling_core`，按内核层 / 通道层分两个子目录（见下表） |
| `main/include/ling/` | 对外头文件；插件边界只认 `plugin_api.h`，其余是宿主内部契约 |
| `main/sys/` | 平台抽象层：Windows 与 POSIX 各一份实现，业务代码只 include `ling/sys.hpp` |
| `main/SDK/python/` | Python 侧车 SDK（ling_sidecar.py），随宿主分发 |
| `main/builtin-plugins/` | 五个内置插件：core-settings、core-plugin-manager、core-log、core-job、core-ui，每个有自己的 CMakeLists.txt |
| `main/archive/` | 源码归档：`main/<宿主版本>/`、`builtin-plugins/<插件名>/<插件版本>/` |

`main/sys/` 是平台抽象层：所有「和操作系统打交道」的动作（取可执行文件目录、读环境变量、动态库加载、套接字、起子进程、控制台、打开浏览器）都收口在这里，业务代码不再写 `#ifdef _WIN32`。

| 位置 | 作用 |
|---|---|
| `sys/Windows/` | Windows 实现：`sys.cpp` 平台函数 + `sidecar_host.cpp` 侧车进程 |
| `sys/Posix/` | POSIX（Linux）实现，同名文件（还没在 Linux 上编过） |

`main/src/` 按内核层 / 通道层分两个子目录（对应 `docs/ling/main/03-架构设计.md` §2.1 的分层）：

| 文件 | 作用 |
|---|---|
| `kernel/kernel.cpp` | 内核骨架：装载表、路由表、装载/卸载 |
| `kernel/gate.cpp` | 能力门禁：注册方法时校验「能力 ⊆ 清单声明」 |
| `kernel/router.cpp` | 方法路由表：插件间调用转发 |
| `kernel/event.cpp` | 事件订阅表与派发 |
| `kernel/depgraph.cpp` | 依赖图与调和计算的纯函数部分 |
| `kernel/manifest.cpp` | 插件清单解析与校验 |
| `kernel/ledger.cpp` | 效应账本：每次改变宿主状态的动作登记一条，停用时倒序撤销 |
| `kernel/log.cpp` | 内核日志：stderr + `<数据根>/logs/kernel-*.log` |
| `kernel/version.cpp` | 版本信息（编译期常量） |
| `channel/admin_server.cpp` | 管理端口服务端：回环监听、首帧认证、帧转发到 kernel.* |
| `channel/admin_client.cpp` | 管理端口客户端：找端口、认证、发帧读帧 |
| `channel/sidecar_proto.cpp` | 侧车协议：一行一个 JSON 对象 |

界面 HTTP 服务端 `ui_server.cpp` 不属于内核，已归入 `main/builtin-plugins/core-ui/`（它是 core-ui 插件的一部分）。侧车进程的装载实现在 `sys/`（按平台拆分）。

`main/` 目录下的每个文件做什么，见 [`main/README.md`](main/README.md)（比这里更细）。

### plugins/ — 第三方插件

插件源码与安装产物，一个插件一个子目录。源码归档在 `plugins/archive/<插件名>/<插件版本>/`，文档在 `docs/plugins/<插件名>/`。

目前没有插件。之前的示例插件（demo-a、demo-caller、demo-bench、demo-b、demo-sidecar、demo-py）已删除。

### tests/ — 测试

| 位置 | 作用 |
|---|---|
| `unit/` | C++ 单元测试：依赖图、事件、清单、路由、侧车协议 |
| `acceptance/` | Python 验收脚本：`acceptance.py` 总入口，l3/l4/l5 各阶段 |
| `manual/` | 手工探针（sidecar_probe.cpp） |

### tools/ — 校验与打包

| 文件 | 作用 |
|---|---|
| `check_docs.py` / `check_refs.py` / `cross_check.py` | 文档一致性校验，改完文档要跑 |
| `release_archive.py` | 生成正式版发行归档（7z），输出到根 `archive/` |
| `clean_gitkeep.py` | 删除占位用的 `.gitkeep` |
| `embed.py` | 把 `shell.html`、`shell.css`、`shell.js`、`theme.js`、`panels.js`、`views/*.js` 转成 C++ 常量表嵌进 core-ui |
| `package.py` | 打包：便携 zip、Inno Setup 安装包 |
| `cmake/LingOptions.cmake` | 统一编译选项（所有目标共用） |
| `cmake/LingPlugin.cmake` | 进程内插件构建辅助 |

### third_party/ — 第三方依赖（随仓库走，构建时不下载）

| 目录 | 作用 |
|---|---|
| `tomlplusplus/` | TOML 解析（单文件） |
| `nlohmann/` | JSON（单文件） |
| `catch2/` | 测试框架（单文件） |
| `python-embed/` | Python 3.13 嵌入运行时 |
| `webview2/` | WebView2 SDK（仅 Windows desktop 界面） |
| `vc-redist/` | VC++ 运行库 DLL（随包分发，干净机器不装 VC++ 也能跑） |

### run/ 与 dist/ — 产物（不进版本库）

| 目录 | 作用 |
|---|---|
| `run/<preset>/` | 构建产物：ling.exe、五个内置插件、Python 运行时；win-release 是打包用的 Release 版 |
| `dist/` | 打包结果：便携 zip、安装包 exe、Inno 脚本 |

### archive/、other/ 与 old/

| 目录 | 作用 |
|---|---|
| `archive/` | 正式版发行归档（7z），不进版本库 |
| `other/` | 本地杂项（学习材料、临时文件），不进版本库，未经点名不碰 |
| `old/` | 旧版本内容，只作参考，不改不删，不进版本库 |

---

## 文档怎么读

**只想查一件事，不用从头读**：打开 [`docs/README.md`](docs/README.md)，按问题找到该读哪份文档；端口、路径、上限这类常查的取值在 [`docs/common/03-常用取值速查.md`](docs/common/03-常用取值速查.md)。

要从头理解，按这个顺序：

1. [`docs/common/00-文档编写规范.md`](docs/common/00-文档编写规范.md) — 文档怎么组织、怎么评审
2. [`docs/common/01-术语表.md`](docs/common/01-术语表.md) — 统一名词，含项目代号 LING / CUI / RM / PLR
3. [`docs/ling/main/00-总览与术语.md`](docs/ling/main/00-总览与术语.md) — ling 是什么、边界在哪、专属名词什么意思
4. [`docs/ling/main/01-需求规格.md`](docs/ling/main/01-需求规格.md) — 宿主必须做到什么，共 127 条
5. [`docs/ling/main/02-技术选型.md`](docs/ling/main/02-技术选型.md) — 用什么语言、插件怎么装、为什么这么选
6. [`docs/ling/main/03-架构设计.md`](docs/ling/main/03-架构设计.md) — 分成哪几块、状态放哪、谁改谁读
7. `docs/ling/main/04-详细设计/` — 四份实现级设计：
   [01 内核四服务](docs/ling/main/04-详细设计/01-内核四服务.md) · [02 依赖解析与调和](docs/ling/main/04-详细设计/02-依赖解析与调和.md) · [03 热更新](docs/ling/main/04-详细设计/03-热更新.md) · [04 管理端口与命令行](docs/ling/main/04-详细设计/04-管理端口与命令行.md)
8. `docs/ling/plugin-support/` — 写插件的契约：
   [01 插件装载与接口](docs/ling/plugin-support/01-插件装载与接口.md) · [02 上下文与效应账本](docs/ling/plugin-support/02-上下文与效应账本.md) · [03 最小插件样板](docs/ling/plugin-support/03-最小插件样板.md)
9. [`docs/ling/main/05-构造与开发指南.md`](docs/ling/main/05-构造与开发指南.md) — 代码写在哪、按什么顺序写、怎么编怎么跑
10. [`docs/ling/main/06-安全设计.md`](docs/ling/main/06-安全设计.md) — 防什么、不防什么、能力门禁到底是什么
11. [`docs/ling/main/07-测试与验收.md`](docs/ling/main/07-测试与验收.md) — 测什么、怎么测、什么算通过
12. [`docs/ling/main/08-部署与发布.md`](docs/ling/main/08-部署与发布.md) — 怎么打包、怎么装、怎么升级回滚
13. [`docs/ling/main/09-运行时预案.md`](docs/ling/main/09-运行时预案.md) — 出问题了怎么办：判据、自动动作、人该做什么
14. [`docs/ling/main/10-路线图与里程碑.md`](docs/ling/main/10-路线图与里程碑.md) — 分几段走、每段能演示什么
15. [`docs/ling/main/11-MVP 范围.md`](docs/ling/main/11-MVP%20范围.md) — 最早可用版本包含哪 85 条需求、不含哪 42 条
16. [`docs/ling/main/12-用户手册.md`](docs/ling/main/12-用户手册.md) — 最终用户怎么装、怎么用、出问题怎么处置
17. 内置插件的共同规则与职责边界见 [`docs/ling/README.md`](docs/ling/README.md)

除一次性材料（如 M0-提审包）外，每份正式文档都有详细版和简要版，简要版在 `brief/` 子目录，结构和详细版一一对应（纯索引文档除外，判据见 `docs/common/00-文档编写规范.md` §4.6）。赶时间读简要版，要理由读详细版。

---

## 怎么参与

- 硬规则见 [`RULES.md`](RULES.md)，协作方式见 [`AGENTS.md`](AGENTS.md)。
- 提意见直接说，不用顾及措辞；文档在"写-审-问-改"循环里反复打磨，初稿必然有错。

---

## 约定速查

| 事项 | 约定 |
|---|---|
| 文档命名 | 两位序号 + 中文名，如 `01-需求规格.md` |
| 文档状态 | 草案 → 评审中 → 已定稿，只有你确认后才能标定稿 |
| 编码前提 | 对应设计文档已定稿 |
| 输出编码 | 程序内部与日志文件一律 UTF-8；Windows 控制台在进程启动时切到 65001，退出还原 |
| 改动前 | 先说清改哪些文件、怎么改 |

---

## 许可

未采用开源许可。保留所有权利，未授予他人使用、修改、分发的权利。
