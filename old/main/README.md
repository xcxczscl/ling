# ling — 一切皆插件的宿主

ling 是一个**零内容**的宿主程序：它自己不提供任何功能（没有界面、没有设置、没有业务概念），
只提供加载插件、转发插件间调用、广播事件、检查权限、解析依赖、热更新的机制。
所有你能看到的功能——界面、设置、插件管理、资源管理、记账、网站管理——**全部是插件**。

## 文档（先读 00）

| 顺序 | 文档 | 回答什么 |
|---|---|---|
| ① | [../docs/ling/00-总览与术语.md](../docs/ling/00-总览与术语.md) | ling 是什么、专属名词解释、贯穿示例 |
| ② | [../docs/ling/01-宿主设计.md](../docs/ling/01-宿主设计.md) | 目标原则、内核四服务、进程线程模型、决策记录、评审区 |
| ③ | [../docs/ling/02-上下文与可逆效应.md](../docs/ling/02-上下文与可逆效应.md) | Context 对象、效应登记表、卸载倒序还原 |
| ④ | [../docs/ling/03-协效应与依赖解析.md](../docs/ling/03-协效应与依赖解析.md) | requires 声明、依赖图、激活/停用状态机、配置调和 |
| ⑤ | [../docs/ling/04-插件接口规范.md](../docs/ling/04-插件接口规范.md) | 清单格式、Rust trait、双轨加载、侧车协议、管理端口协议 |
| ⑥ | [../docs/ling/05-热更新.md](../docs/ling/05-热更新.md) | 热更新流程、动态库装卸载的坑与对策、持久文件纪律 |
| ⑦ | [../docs/ling/06-内置插件设计.md](../docs/ling/06-内置插件设计.md) | 内置插件目录结构、默认安装三件套、core-job / core-log |
| ⑧ | [../docs/ling/07-路线图与验收.md](../docs/ling/07-路线图与验收.md) | 阶段划分与验收标准 |
| ⑨ | [../docs/ling/08-运行时预案.md](../docs/ling/08-运行时预案.md) | 运行时故障的处置步骤（运维手册） |

共用内容见 [`../docs/common/`](../docs/common/)：文档编写规范、公共术语、论文机制说明。

## 当前状态

**L6 侧车双轨完成（2026-10-16），ling L1–L6 全部完成。** 设计经评审轮、终审修正、最后补充、终审封档后冻结；封档后改动必须走各文档变更记录与 `docs/ling/01` 附录 B。

### L1 交付物（验收 5/5 通过，见 `docs/ling/07` §2）

| 部分 | 内容 |
|---|---|
| `crates/ling-sdk` | 插件开发套件：`Plugin` trait、`Context`（注册/调用/订阅/广播/data_dir）、能力与错误码类型 |
| `crates/ling-kernel` | 内核：`manifest` 清单校验、`broker` 路由、`bus` 事件、`caps` 门禁、`context`/`ledger` 机制、`loader` 安装（目录/zip）与 dlopen 装载 |
| `plugins/demo/demo-a` `demo-b` | 验收示例插件：互调、事件广播、能力标注、命名空间探针、data/ 落盘 |
| `crates/ling-kernel/tests/l1_acceptance.rs` | 5 条验收集成测试（安装/路由/事件/门禁/命名空间） |

### L2 交付物（验收 4/4 通过，见 `docs/ling/07` §2）

| 部分 | 内容 |
|---|---|
| `crates/ling-kernel/src/kernel.rs` | `Kernel::uninstall(id, purge)` + `UninstallReport`（撤销失败清单、残留核对、"卸载完整/不完整"汇总） |
| `crates/ling-kernel/src/ledger.rs` | `run_reverse` 单条失败/panic 隔离继续执行 + 账本条目数与注册数一致、倒序执行的单元测试 |
| `crates/ling-kernel/src/loader.rs` | 卸载文件语义：非 purge 保留 `data/`、已安装判定 = `plugin.toml` 存在、重装合并数据 |
| `crates/ling-kernel/tests/l2_uninstall.rs` | 4 条验收集成测试 + 故障注入单元测试 |

### L3 交付物（验收 5/5 通过，见 `docs/ling/07` §2）

| 部分 | 内容 |
|---|---|
| `crates/ling-kernel/src/deps.rs` | 依赖图（安装判定、版本按段前缀匹配 `@1`/`@1.2`/`@1.2.3`）、环检测带路径、拓扑序激活/逆序停用、`reconcile` 逐步执行 |
| `crates/ling-kernel/src/kernel.rs` | 安装接线调和、卸载先级联停用已激活依赖方、`enable`/`disable`（停用名单持久化）、`subscribe`/`unsubscribe`、`stop_plugin` |
| `crates/ling-kernel/src/registry.rs` | `registry.json` 宿主级登记：`{management_port?, disabled, revoked}`，原子写 |
| `crates/ling-kernel/tests/l3_reconcile.rs` | 6 条验收集成测试 + deps 单元测试 |

### L4 交付物（验收 5/5 通过，见 `docs/ling/07` §2）

| 部分 | 内容 |
|---|---|
| `crates/ling-kernel/src/kernel.rs` | `reload`（校验新清单 → 备份运行版本 → 停下游与自身 → 装新版 → 调和恢复下游）、`restart`、`shutdown` |
| `crates/ling-kernel/src/loader.rs` | 装载用 `{id}.so` 的**私有副本** `{id}.{序号}.so`（防 SIGBUS） |
| `crates/ling-kernel/src/admin.rs` | 管理端口：仅绑定 `127.0.0.1`，行式 JSON 协议，`kernel.plugins.*`/`kernel.shutdown` 方法 |
| `crates/ling-cli` | `ling` 命令行：serve / plugins / install / uninstall / enable / disable / reload / restart / shutdown |
| `crates/ling-kernel/tests/l4_reload.rs` | 5 条验收集成测试（版本替换 + 下游无感/100 次压力/失败回滚/管理端口认证/逆向拓扑停机） |
| `crates/ling-cli/tests/cli_e2e.rs` | CLI 端到端：真实 `ling serve` 子进程 + install→自动激活→disable/enable→reload→shutdown 全链路 |

### L5 交付物（验收 6/6 通过，见 `docs/ling/07` §2）

| 部分 | 内容 |
|---|---|
| `../plugins/builtin-plugins/core-ui/` | 界面壳：`127.0.0.1:7910` 起，`/rpc` 网关，贡献注册/撤下/列表 |
| `../plugins/builtin-plugins/core-settings/` | 设置存储（SQLite，rusqlite bundled）+ schema 校验 + 设置页 |
| `../plugins/builtin-plugins/core-plugin-manager/` | 插件管理：经管理端口转发内核 + 管理页 |
| `../plugins/builtin-plugins/core-job/` | 作业调度（可选安装）：register/progress/cancel/list/get + 作业页 |
| `../plugins/builtin-plugins/core-log/` | 日志收集（可选安装）：write/read/rotate + 日志页 |
| `crates/ling-kernel/src/builtin.rs` | 内置插件目录发现、首启三件套（`ensure_default_trio`）、按需安装 |
| `crates/ling-kernel/tests/l5_builtin.rs` | 6 条验收集成测试 |
| `crates/ling-cli/tests/cli_l5.rs` | CLI 端到端：真实 `ling serve` 子进程首启三件套 + UI 可达 + 命令行可管理 + 优雅停机 |

### L6 交付物（验收 4/4 通过，见 `docs/ling/07` §2）

| 部分 | 内容 |
|---|---|
| `crates/ling-kernel/src/sidecar.rs` | 侧车运行时：TCP 控制面、`LING_SOCKET`/`LING_PLUGIN` 传参、30s 超时、优雅停用、崩溃善后 |
| `crates/ling-kernel/src/state.rs` | `Callback` 签名升级为带主题；`Shared.sidecars` 注册表 |
| `crates/ling-kernel/src/bus.rs` | `wrap_plugin_callback` 适配层 |
| `crates/ling-kernel/src/loader.rs` | 装载分支：侧车插件走 `sidecar::start` |
| `../plugins/demo/demo-sidecar/` | Python 侧车示例 |
| `crates/ling-kernel/tests/l6_sidecar.rs` | 4 条验收集成测试 |

评审意见记录见 `docs/ling/01-宿主设计.md` **附录 B**。

## 已确认的关键决策（详见 docs/ling/01 附录 A）

- 宿主用 Rust；插件双轨：Rust 插件进程内动态库，其他语言插件进程外侧车
- 内核做 4 件事（模块化组织，编译时可替换、运行时固定）：接口方法路由、事件广播、能力门禁、依赖解析与自动激活
- 配置存储、作业调度、日志做成**内置插件**（core-settings / core-job / core-log），不进内核
- 内置插件放 `plugins/builtin-plugins/`（从属关系）：首次默认装三件套（core-ui / core-settings / core-plugin-manager，均可卸载），其余按需安装
- 采纳论文机制：可逆效应 + 响应式协效应 + 上下文范式，**首轮即含热更新**
- 插件状态放自己的持久文件，内存里不留状态
- 内核更新 = 手动替换二进制 + 重启；api 主版本兼容策略见 docs/ling/01 §9
