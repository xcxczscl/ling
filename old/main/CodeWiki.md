# ling 项目 Code Wiki

> 面向开发者的代码级知识库：架构、模块职责、关键类与函数、依赖关系、运行方式。
> 与设计文档（[docs/](docs/)）互补——本文按**代码结构**组织，设计文档按**设计决策**组织。
> 生成基准：README 截至 2026-10-16 的 L1–L6 全部完成状态。

---

## 目录

1. [项目概述](#1-项目概述)
2. [整体架构](#2-整体架构)
3. [代码库布局](#3-代码库布局)
4. [核心模块职责](#4-核心模块职责)
5. [关键类与函数说明](#5-关键类与函数说明)
6. [依赖关系](#6-依赖关系)
7. [错误码与能力表](#7-错误码与能力表)
8. [项目运行方式](#8-项目运行方式)
9. [核心流程与数据流](#9-核心流程与数据流)

---

## 1. 项目概述

**ling** 是一个"零内容"的插件宿主程序：宿主本身不提供任何业务功能（无界面、无设置、无业务概念），只提供插件加载、插件间调用转发、事件广播、能力门禁、依赖解析与热更新的机制。所有可见功能（界面、设置、插件管理、资源管理、记账……）**全部是插件**。

### 1.1 技术栈

| 项 | 值 |
|---|---|
| 语言 | Rust（edition 2021） |
| 构建 | Cargo workspace（resolver=2） |
| 序列化 | `serde` / `serde_json`（跨插件数据统一走 `Value`） |
| 清单格式 | TOML（`plugin.toml`） |
| 动态加载 | `libloading`（Rust 插件进程内 dlopen） |
| 打包 | `zip`（zip 包安装） |
| 嵌入式存储 | `rusqlite`（bundled，仅 core-settings 用） |
| 侧车协议 | 行式 JSONL over TCP（其他语言插件进程外运行） |

### 1.2 核心设计原则

- **一切皆插件**：宿主零内容；配置存储、作业调度、日志都做成内置插件（不进内核）。
- **双轨插件**：Rust 插件在进程内动态库运行；其他语言插件经"侧车"（sidecar）在进程外通过 JSONL 控制面互通。
- **内核只做 4 件事**：接口方法路由、事件广播、能力门禁、依赖解析与自动激活。
- **三项机制**：`Context` 上下文句柄、效应账本（可逆效应）、配置调和（响应式协效应）。
- **无内存状态**：插件状态存自己的持久文件；内存只留装载态。
- **可逆效应**：`activate` 里的每个注册/订阅都登记一条撤销闭包，停用/卸载时倒序还原。

---

## 2. 整体架构

### 2.1 分层结构

```
┌────────────────────────────────────────────────────────┐
│  ling-cli  (命令行入口：serve / plugins / install ...)   │
│            └─ 经管理端口 TCP + session.json 口令连接宿主  │
├────────────────────────────────────────────────────────┤
│              ling-kernel  (宿主内核)                      │
│   四服务：broker(路由) bus(广播) caps(门禁) deps(依赖)      │
│   三机制：context(上下文) ledger(账本) reconcile(调和)      │
│   装载：loader(dlopen) / sidecar(进程外侧车)               │
│   管理面：admin(管理端口) builtin(内置插件目录)             │
├────────────────────────────────────────────────────────┤
│  ling-sdk  (插件开发套件：Plugin trait / Context / 类型)   │
├────────────────────────────────────────────────────────┤
│  插件（进程内 Rust cdylib）       插件（进程外侧车）         │
│  demo-a / demo-b / core-*        demo-sidecar(Python)    │
└────────────────────────────────────────────────────────┘
```

### 2.2 双轨装载

| 轨 | 载体 | 入口 | 说明 |
|---|---|---|---|
| 进程内（Rust） | `cdylib` 动态库 | 导出 `ling_plugin_create() -> *mut dyn Plugin` | `loader::load` 用 `libloading` 装载、取符号、核对 `id`、调 `activate` |
| 进程外（侧车） | 任意可执行文件 | 运行时启动进程，经 `LING_SOCKET` 环境变量传 TCP 端点 | `sidecar::start` 起进程并完成握手；JSONL 控制面 |

### 2.3 数据根目录与持久文件

数据根目录解析优先级（[ling-cli/src/main.rs](crates/ling-cli/src/main.rs) `ling_data_root`）：

1. 环境变量 `LING_HOME`
2. `$XDG_DATA_HOME/ling`
3. `~/.local/share/ling`

关键持久文件（`data_root` 下）：

| 路径 | 内容 | 维护方 |
|---|---|---|
| `registry.json` | `management_port`、`ui_port`、`disabled`（停用名单）、`revoked`（能力撤销黑名单） | 内核 registry |
| `session.json` | 会话口令 `token`（unix 下 0600） | 内核 admin |
| `plugins/<编号>/plugin.toml` | 已安装插件的清单 | loader 安装 |
| `plugins/<编号>/data/` | 插件私有数据（卸载非 purge 保留） | 插件自己 |
| `plugins/<编号>/{id}.so` | 动态库磁盘原件（保留以便重载） | loader |

---

## 3. 代码库布局

工作区成员（[Cargo.toml](Cargo.toml)）：

```
ling/
├── Cargo.toml                     # workspace 根
├── crates/
│   ├── ling-sdk/                  # 插件开发套件（插件唯一需要依赖的 crate）
│   │   └── src/lib.rs
│   ├── ling-kernel/               # 宿主内核
│   │   ├── src/{kernel,state,broker,bus,caps,context,ledger,deps,
│   │   │        loader,manifest,registry,error,admin,builtin,sidecar}.rs
│   │   └── tests/{l1..l6}*.rs + common/mod.rs
│   └── ling-cli/                  # ling 命令行
│       ├── src/main.rs
│       └── tests/{cli_e2e,cli_l5}.rs
├── builtin-plugins/               # 内置插件（发行包子目录，从属关系）
│   ├── core-ui/                   # 界面壳（默认三件套）
│   ├── core-settings/             # 设置存储（默认三件套）
│   ├── core-plugin-manager/       # 插件管理（默认三件套）
│   ├── core-job/                  # 作业调度（可选）
│   └── core-log/                  # 日志收集（可选）
├── plugins/                       # 示例/验收插件
│   ├── demo-a/                    # Rust 插件
│   ├── demo-b/                    # Rust 插件
│   └── demo-sidecar/              # Python 侧车插件（非 cargo crate）
└── docs/                          # 设计文档 00–08
```

> 注意：`plugins/demo-sidecar/` 不在 workspace 成员列表里——它是纯侧车插件（只有 `plugin.toml` + `sidecar/demo_sidecar.py`，无 Rust 代码）。

---

## 4. 核心模块职责

### 4.1 `crates/ling-sdk` — 插件开发套件

插件**只需要依赖这一个 crate**。定义插件与宿主的全部公共契约。

| 职责 | 说明 |
|---|---|
| `Plugin` trait | 插件必须实现的唯一 trait（`id` / `activate` / `deactivate`） |
| `Context` | 注入给插件的句柄：注册方法、调用、订阅、广播、读能力、数据目录 |
| `ContextCore` | 上下文后端 trait（内核实现，插件不直接实现） |
| 类型 | `Value`（re-export serde_json）、`MethodHandler`、`EventCallback`、`UndoFn`、`BoxFuture` |
| 错误 | `CallError`、`ContextError`、`ActivateError` |
| 常量/枚举 | `HOST_API`、`Capability`（Fs/Network/Ui/Shell）、`EffectKind`、`codes` 错误码 |

文件：[crates/ling-sdk/src/lib.rs](crates/ling-sdk/src/lib.rs)

### 4.2 `crates/ling-kernel` — 宿主内核

内核四服务 + 三机制的实现，每个子模块单一职责：

| 文件 | 职责 |
|---|---|
| [kernel.rs](crates/ling-kernel/src/kernel.rs) | `Kernel` 门面类：install/uninstall/load/reload/enable/disable/restart/shutdown/reconcile/call/emit 等公开 API |
| [state.rs](crates/ling-kernel/src/state.rs) | 共享状态 `Shared` 与数据结构（方法表、订阅表、能力表、插件记录、停用名单、撤销黑名单、侧车句柄表） |
| [broker.rs](crates/ling-kernel/src/broker.rs) | **服务①** 接口方法路由：`register` / `call` |
| [bus.rs](crates/ling-kernel/src/bus.rs) | **服务②** 事件广播：`subscribe` / `emit` / 通配匹配 / 宿主订阅 |
| [caps.rs](crates/ling-kernel/src/caps.rs) | **服务③** 能力门禁：声明、撤销、授权、生效判定 |
| [deps.rs](crates/ling-kernel/src/deps.rs) | **服务④** 依赖图：环检测、版本匹配、拓扑序、`reconcile` 配置调和 |
| [context.rs](crates/ling-kernel/src/context.rs) | **机制①** `KernelCore`：`ContextCore` 的内核实现（每步 = 查表 + 写表 + 记账） |
| [ledger.rs](crates/ling-kernel/src/ledger.rs) | **机制②** 效应账本：`take_ledger` / `run_reverse`（倒序撤销、失败隔离、panic 隔离） |
| [loader.rs](crates/ling-kernel/src/loader.rs) | 装载通道：目录/zip 安装、dlopen 装载、私有动态库副本、热更新备份/还原、卸载文件语义 |
| [manifest.rs](crates/ling-kernel/src/manifest.rs) | 清单解析与校验（`plugin.toml`） |
| [registry.rs](crates/ling-kernel/src/registry.rs) | 宿主级登记 `registry.json` 读写（原子写） |
| [error.rs](crates/ling-kernel/src/error.rs) | `KernelError` 统一错误与协议错误码映射 |
| [admin.rs](crates/ling-kernel/src/admin.rs) | 管理端口：TCP 127.0.0.1、口令认证、行式 JSON 协议、`kernel.*` 方法分发 |
| [builtin.rs](crates/ling-kernel/src/builtin.rs) | 内置插件目录发现、暂存、首启三件套安装、可安装列表 |
| [sidecar.rs](crates/ling-kernel/src/sidecar.rs) | 侧车运行时：启动进程、JSONL 控制面、调用/事件转发、优雅停用、崩溃善后 |

入口/导出：[crates/ling-kernel/src/lib.rs](crates/ling-kernel/src/lib.rs)

### 4.3 `crates/ling-cli` — 命令行

| 职责 | 说明 |
|---|---|
| `serve` | 前台启动宿主：起管理端口 → 发现内置插件目录 → 首启装三件套 → 调和 → 主循环等停机 |
| `plugins` / `install` / `uninstall` / `enable` / `disable` / `reload` / `restart` / `shutdown` / `logs` | 除 `serve` 外全部子命令经**管理端口**连接已运行宿主 |
| `--version` / `--help` | 版本与帮助 |

文件：[crates/ling-cli/src/main.rs](crates/ling-cli/src/main.rs)

### 4.4 内置插件（builtin-plugins，均为 Rust cdylib 插件）

| 插件 | 能力声明 | 依赖 | 职责 | 关键方法 |
|---|---|---|---|---|
| `core-ui` | network, fs, ui | — | 界面壳：本机网页服务（7910 起）、`/rpc` 网关、界面贡献注册/撤下/列表、监听插件状态自动撤下 | `core-ui.ui.contribute/.retract/.list` |
| `core-settings` | fs, ui | core-ui@1 | 设置存储（SQLite）+ 模式校验 + 设置页 | `core-settings.define/.get/.set/.list` |
| `core-plugin-manager` | network, fs, ui | core-ui@1 | 插件管理：经管理端口转发内核操作 + 管理页 + 调和步骤订阅 | `core-plugin-manager.install/.uninstall/.enable/.disable/.reload/.restart/.list/.builtin.available/.builtin.install/.caps.*/.plan.recent` |
| `core-job` | ui | — | 作业调度：登记/进度/取消/查询 + 事件 + 作业页 | `core-job.register/.progress/.cancel/.list/.get` |
| `core-log` | fs, ui | — | 日志收集：写入/读取/轮转（10MB×5）+ 日志页 | `core-log.write/.read/.rotate` |

默认安装三件套：`core-ui`、`core-settings`、`core-plugin-manager`（常量 `DEFAULT_TRIO`）。`core-job`、`core-log` 可选安装。

### 4.5 示例插件（plugins）

| 插件 | 说明 |
|---|---|
| [demo-a](plugins/demo-a/src/lib.rs) | 转发调用（`demo-a.forward`）、广播事件（`demo-a.emit`）、命名空间/能力探针（`demo-a.probe`）、贡献测试页 |
| [demo-b](plugins/demo-b/src/lib.rs) | 方法（`demo-b.add`）、带能力标注方法（`demo-b.dup.delete` 需 fs）、订阅 `demo-a.*` 落盘（`demo-b.record`/`read_events`） |
| [demo-sidecar](plugins/demo-sidecar/) | Python 侧车：注册方法、广播事件、订阅、崩溃注入，验证 L6 侧车链路 |

---

## 5. 关键类与函数说明

### 5.1 ling-sdk

#### `Plugin` trait（插件入口契约）

```rust
pub trait Plugin: Send + Sync {
    fn id(&self) -> &str;
    fn activate(&mut self, ctx: Context) -> Result<(), ActivateError>;
    fn deactivate(&mut self);
}
```

- `id()`：插件编号，必须与清单 `id` 一致（装载时核对）。
- `activate()`：装载时调用；所有注册/订阅经 `ctx` 且记入账本；失败则内核倒序撤销并卸载动态库。
- `deactivate()`：停用/卸载/热更新前调用，用于收敛线程、落盘。

#### `Context`（插件句柄）

| 方法 | 说明 |
|---|---|
| `register_method(name, handler, unregister, requires)` | 注册方法；`name` 必须带插件编号前缀；`requires` 注册时校验 ⊆ 清单能力 |
| `call(plugin, method, args)` | 跨插件调用（同进程直调，走路由 + 门禁） |
| `call_async(...)` | 异步调用，返回可组合 future |
| `subscribe(pattern, callback, unsubscribe)` | 订阅事件；`pattern` 支持 `*` 通配 |
| `emit(topic, payload)` | 广播事件；主题必须以本插件编号开头（否则 `E_NAMESPACE_VIOLATION`） |
| `has_capability(cap)` | 读自己清单声明的能力 |
| `data_dir()` | 自己的数据目录 `ling/plugins/<编号>/data/`（内核保证存在） |
| `on_deactivate(callback)` | 登记停用收尾回调 |
| `ledger()` | 查看自己的效应账本 |

#### 类型别名

```rust
type MethodHandler = Box<dyn Fn(Value) -> Result<Value, CallError> + Send + Sync>;
type EventCallback = Box<dyn Fn(Value) + Send + Sync>;
type UndoFn = Box<dyn FnOnce() + Send>;
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
```

#### `Capability` 枚举

`Fs`/`Network`/`Ui`/`Shell`，方法 `as_str`/`parse`/`Display`，常量 `ALL`。

#### 错误类型

| 类型 | 用途 |
|---|---|
| `CallError { code, message }` | 跨插件调用错误（`plugin_missing`/`method_missing`/`capability_denied` 工厂） |
| `ContextError { code, message }` | 上下文操作错误（`namespace_violation`/`capability_denied`） |
| `ActivateError { message }` | 装载阶段 `activate` 失败 |

#### 常量

- `HOST_API = 1`：宿主接口主版本，清单 `api` 字段必须等于它。
- `codes` 模块：全部错误码字符串（见 [§7](#7-错误码与能力表)）。
- `EffectKind` 枚举：`MethodRegistered`/`EventSubscribed`/`ConfigWritten`/`SidecarStarted`/`DeactivateCallback`。

### 5.2 ling-kernel

#### `Kernel`（内核门面，[kernel.rs](crates/ling-kernel/src/kernel.rs)）

| 方法 | 说明 |
|---|---|
| `new(data_root)` | 创建内核、准备数据根、载入 `registry.json`（停用名单 + 撤销黑名单） |
| `set_builtin_catalog(entries)` | 注入发行包内置插件目录 |
| `install(source)` | 安装目录/zip：暂存 → 校验清单 → 拷贝 → 环拒绝并清理 → 调和（`config_gate` 串行化） |
| `load(id)` / `load_all()` | 装载单个/全部已安装插件（原语，不校验依赖） |
| `call(plugin, method, args)` | 跨插件调用（服务①+③） |
| `emit(topic, payload)` | 内核侧广播（`kernel.*` 主题） |
| `subscribe/subscribe_unsubscribe` | 宿主侧订阅（不记账本） |
| `revoke_capability/grant_capability` | 撤销/恢复能力（宿主级黑名单持久化） |
| `enable/disable(id)` | 启用/停用并调和（停用名单持久化） |
| `reload(id)` | 热更新：校验 → 备份 → 停下游和自己 → 装新版 → 调和；失败回滚旧版 |
| `restart(id)` / `shutdown()` | 重启 / 优雅停机（逆向拓扑序） |
| `reconcile()` / `last_reconcile()` | 触发调和 / 取最近调和报告 |
| `uninstall(id, purge)` | 卸载：级联停依赖方 → 停用 → 清理工件（非 purge 保留 data）→ 调和 |
| 查询类 | `installed_plugins`/`activated_plugins`/`plugin_list`/`ledger_of`/`manifest_of`/`namespace_violations`/`builtin_available`/`install_builtin`/`ensure_default_trio` |

`Drop` 实现：析构即优雅停机（幂等兜底）。

#### `Shared`（共享状态，[state.rs](crates/ling-kernel/src/state.rs)）

```rust
pub struct Shared {
    pub data_root: PathBuf,
    pub methods: RwLock<HashMap<String, MethodEntry>>,   // 方法表
    pub subs: RwLock<Vec<SubEntry>>,                     // 订阅表
    pub caps: RwLock<HashMap<String, CapEntry>>,         // 能力表
    pub plugins: RwLock<HashMap<String, PluginRecord>>,  // 插件记录表
    pub violations: Mutex<Vec<Violation>>,               // 命名空间越权记录
    pub sub_seq: AtomicU64,                              // 订阅序号
    pub config_gate: Mutex<()>,                          // 配置变更串行闸
    pub disabled: RwLock<BTreeSet<String>>,              // 停用名单
    pub revoked: RwLock<BTreeMap<String, BTreeSet<Capability>>>, // 撤销黑名单
    pub plan_seq: AtomicU64,                             // 调和计划序号
    pub lib_seq: AtomicU64,                              // 私有库文件序号
    pub sidecars: Mutex<BTreeMap<String, Arc<SidecarHandle>>>, // 侧车句柄表
}
```

关键方法：`plugins_dir`/`plugin_dir`（路径）、`full_method_name`（拼接/校验前缀）、`record_violation`（记越权）、`dispatch`（分发事件到匹配订阅）。

#### 关键数据类型

- `PluginRecord`：装载记录（manifest、manifest_text、state、instance、library、library_file、ledger、next_seq、deactivate_cbs）。方法 `push_effect`（记账）、`push_deactivate_cb`、`take_deactivate_cbs`。
- `PluginState`：`Activating` / `Activated`。
- `MethodEntry { owner, requires, handler }`、`SubEntry { id, owner, pattern, callback }`、`CapEntry { declared }`、`LedgerEntry { seq, kind, desc, undo }`、`Violation { plugin, kind, name }`。

#### 四服务核心函数

| 模块 | 函数 | 说明 |
|---|---|---|
| broker | `register` | 校验命名空间前缀 + `requires ⊆ declared`；写方法表 + 记账（UNDO 移除方法 + 调用插件撤销闭包） |
| broker | `call` | 拼全名 → 查方法 → 校验 `state == Activated` → 校验生效能力 → 调 handler |
| bus | `subscribe` / `emit` | 订阅不限命名空间；广播必须以自己编号开头；`wrap_plugin_callback` 把 SDK 回调适配为带主题回调 |
| bus | `kernel_subscribe` / `kernel_unsubscribe` | 宿主侧订阅（owner=`kernel`，不进插件账本） |
| caps | `effective` | `declared && !revoked` 判定生效能力 |
| caps | `revoke` / `grant` | 撤销/恢复（拒绝未声明能力），持久化 registry |
| deps | `version_satisfied` | 版本按段前缀匹配（`@1`/`@1.2`/`@1.2.3`） |
| deps | `find_cycle` | DFS 环检测，返回带路径的环 |
| deps | `reconcile` | 调和主流程（见 §9.6） |

#### 账本（ledger，[ledger.rs](crates/ling-kernel/src/ledger.rs)）

- `take_ledger(record)`：取出账本（`mem::take`）。
- `run_reverse(entries)`：倒序执行撤销；每条隔离（`catch_unwind`）；返回失败清单（含 seq/desc/reason），单条失败不阻断后续。

#### 装载（loader，[loader.rs](crates/ling-kernel/src/loader.rs)）

- `install`：目录或 zip → `install_from_dir`/`install_from_zip`。
- `load`：校验目录与清单 → 侧车走 `sidecar::start`，否则 dlopen 私有副本 → `create_instance` → `caps::set_declared` → 插入记录 → `activate` → 成功置 `Activated`+广播，失败倒序撤销并清理。
- `create_instance`：取 `ling_plugin_create` 符号，核对 `id()`。
- 私有库副本：装载 `{id}.{序号}.so`（`lib_seq` 递增），原件 `{id}.so` 保留在盘。
- `remove_install_files`：非 purge 删工件保留 `data/`；purge 连 `data/` 删。
- `backup_install_artifacts` / `restore_install_artifacts` / `discard_backup`：热更新备份/还原。

#### 清单（manifest，[manifest.rs](crates/ling-kernel/src/manifest.rs)）

`Manifest { plugin: PluginMeta, requires: Requires, sidecar: Option<SidecarMeta> }`。`parse_and_validate` 完成全部字段校验（id/version/api/capabilities/plugins/sidecar）。关键校验：

- id：小写字母/数字/短横线，首字符不能是短横线，长度 ≤64。
- version：三段式，各段 ≤65535。
- api：`ApiField`（接受字符串或整数）必须等于 `HOST_API`。
- capabilities：必须在 `Capability::parse` 认得。
- plugins：`id[@版本约束]`，版本约束 1–3 段。
- sidecar：`executable` 相对路径不含 `..` 且存在；`protocol == "jsonl"`；`restart ∈ {on-crash, never}`。

#### 管理端口（admin，[admin.rs](crates/ling-kernel/src/admin.rs)）

- `start(kernel, port)`：绑定 127.0.0.1（占用顺延/0=随机），写实际端口入 `registry.json`，生成口令写 `session.json`（0600）。
- 协议：`{"op":"auth","token":…}` → 应答；之后每行 `{"id","method","params"}` → 应答 `{"id","ok","result"/"error"}`。
- `dispatch` 支持的方法：`kernel.plugins.{list,install,uninstall,enable,disable,reload,restart,revoke_capability,grant_capability,builtin.available,builtin.install}`、`kernel.logs.read`、`kernel.shutdown`。

#### 侧车（sidecar，[sidecar.rs](crates/ling-kernel/src/sidecar.rs)）

- `start`：起进程（env `LING_SOCKET`/`LING_PLUGIN`）→ 握手（连接控制面，5s 超时）→ 发 `activate` → 起 `monitor` 线程。
- `SidecarHandle`：endpoint/restart/stopping/deactivated_ack/child/writer/next_req/pending。`invoke` 有 30s 超时（`E_TIMEOUT`）。
- `monitor` 循环处理消息：`register_method`/`subscribe`/`call`/`emit`/`result`/`error`/`deactivated`。
- `graceful_stop`：`deactivate` → 等 `deactivated`（5s）→ `shutdown` → 等退出（5s）→ 强杀。
- `crash_cleanup`：注销效应 → 广播 `crashed` → `restart == "on-crash"` 时自动重装。

---

## 6. 依赖关系

### 6.1 Crate/模块依赖（Rust 内部）

```
ling-cli ──> ling-kernel ──> ling-sdk ──> serde, serde_json
                │  └─> toml, libloading, zip
                └─> (rusqlite 仅 core-settings)

插件 (demo-a/demo-b, core-*) ──> ling-sdk (唯一必依赖)
core-settings ──> ling-sdk + rusqlite(bundled)
```

### 6.2 外部依赖（[workspace.dependencies](Cargo.toml)）

| 依赖 | 版本 | 用途 |
|---|---|---|
| serde | 1（derive） | 序列化/反序列化 |
| serde_json | 1 | JSON、跨插件 `Value` |
| toml | 0.8 | 解析 `plugin.toml` |
| libloading | 0.8 | 装载 Rust 插件动态库 |
| zip | 9 | zip 包安装 |

各 crate 额外依赖：

| crate | 额外依赖 |
|---|---|
| ling-kernel | （全部来自 workspace）+ dev `tempfile` |
| ling-cli | + dev `tempfile` |
| core-settings | + `rusqlite = 0.32`（features `bundled`） |
| demo-a / demo-b | + serde_json |

### 6.3 插件声明依赖（`requires.plugins`）

| 插件 | 依赖 |
|---|---|
| demo-a | `demo-b@0.1.0` |
| demo-b | （无） |
| core-settings | `core-ui@1` |
| core-plugin-manager | `core-ui@1` |
| core-ui / core-job / core-log | （无） |

版本约束规则：`@1` 主版本、`@1.2` 主+次、`@1.2.3` 精确，按段前缀匹配（[deps.rs](crates/ling-kernel/src/deps.rs) `version_satisfied`）。

---

## 7. 错误码与能力表

### 7.1 错误码（[ling-sdk/src/lib.rs](crates/ling-sdk/src/lib.rs) `codes`）

| 码 | 含义 |
|---|---|
| `E_MANIFEST_INVALID` | 清单非法 |
| `E_API_MISMATCH` | 插件 api 与宿主不一致 |
| `E_CAPABILITY_UNKNOWN` | 声明了宿主不认识的能力 |
| `E_CAPABILITY_DENIED` | 能力被拒（未声明或已撤销） |
| `E_NAMESPACE_VIOLATION` | 方法名/事件主题未带插件前缀 |
| `E_PLUGIN_MISSING` | 插件未激活/未装载 |
| `E_METHOD_MISSING` | 方法未注册 |
| `E_DEPENDENCY_UNMET` | 依赖未满足 |
| `E_DEPENDENCY_CYCLE` | 依赖成环 |
| `E_ACTIVATE_FAILED` | activate 失败 |
| `E_UNDO_FAILED` | 撤销失败 |
| `E_RELOAD_FAILED` | 热更新失败 |
| `E_TIMEOUT` | 调用超时（侧车） |
| `E_SIDE_CRASHED` | 侧车连接断开/崩溃 |
| `E_AUTH` | 认证失败 |

### 7.2 能力（`Capability`）

| 能力 | 字符串 |
|---|---|
| 文件系统 | `fs` |
| 网络 | `network` |
| 界面 | `ui` |
| 命令行/进程 | `shell` |

---

## 8. 项目运行方式

### 8.1 构建

```bash
# 构建整个 workspace（内核、CLI、SDK、demo 与内置插件）
cargo build

# 发布构建
cargo build --release
```

> 内置插件需已构建出动态库（[builtin.rs](crates/ling-kernel/src/builtin.rs) 从 `target/{debug,release}/lib<编号下划线化>.<后缀>` 解析，缺失会被跳过）。

### 8.2 运行

```bash
# 前台启动宿主（首启自动安装内置三件套；自动向上发现 builtin-plugins/）
cargo run -p ling-cli -- serve

# 或指定内置插件目录
cargo run -p ling-cli -- serve --builtin-dir <绝对路径>

# 直接用编译出的二进制
./target/debug/ling serve
```

启动后输出：管理端口（默认 7900，占用顺延）、界面端口（若 core-ui 已装）、数据根目录。

### 8.3 CLI 子命令

| 命令 | 说明 |
|---|---|
| `ling serve` | 前台启动宿主 + 管理端口 + 首启装三件套 + 调和 |
| `ling plugins` | 列出已安装插件及状态 |
| `ling install <目录\|zip>` | 安装插件并触发调和 |
| `ling uninstall <编号> [--purge]` | 卸载（`--purge` 连数据删除） |
| `ling enable\|disable <编号>` | 启用/停用 |
| `ling reload <编号>` | 热更新（新版本已在插件目录） |
| `ling restart <编号>` | 重启插件 |
| `ling logs <编号> [条数]` | 读插件日志（需 core-log） |
| `ling shutdown` | 优雅停止宿主 |
| `ling --version` / `--help` | 版本 / 帮助 |

### 8.4 数据根目录

`LING_HOME` > `$XDG_DATA_HOME/ling` > `~/.local/share/ling`。

### 8.5 测试

```bash
# 全部 workspace 测试（含 L1–L6 内核集成测试与 CLI 端到端）
cargo test

# 仅内核（含 6 组验收集成测试）
cargo test -p ling-kernel

# 仅 CLI 端到端
cargo test -p ling-cli

# 单元测试
cargo test --lib
```

集成测试覆盖（`crates/ling-kernel/tests/`）：

| 文件 | 覆盖 |
|---|---|
| l1_acceptance.rs | 安装/路由/事件/门禁/命名空间（5 条） |
| l2_uninstall.rs | 卸载（4 条）+ 故障注入单元测试 |
| l3_reconcile.rs | 依赖/调和（6 条） |
| l4_reload.rs | 热更新（5 条） |
| l5_builtin.rs | 内置插件（6 条） |
| l6_sidecar.rs | 侧车（4 条） |

### 8.6 验证插件的交互（demo）

安装 demo 插件后，可通过界面 `/rpc` 网关或命令行调用：

```json
// 调用 demo-a.ping
{ "id": "1", "plugin": "demo-a", "method": "demo-a.ping", "params": {} }

// 转发调用：demo-a.forward 调 demo-b.add
{ "id": "2", "plugin": "demo-a", "method": "demo-a.forward",
  "params": { "plugin": "demo-b", "method": "demo-b.add", "args": { "a": 1, "b": 2 } } }
```

---

## 9. 核心流程与数据流

### 9.1 安装流程

```
install(source)
 ├─ loader::install：目录或 zip → 校验清单 → 拷贝进 ling/plugins/<id>/
 ├─ deps::check_no_cycle：检测依赖环（有环→ remove_install_files 清理→ E_DEPENDENCY_CYCLE）
 └─ reconcile_inner：调和（依赖齐备→ activate；激活失败不否决安装）
```

### 9.2 装载/激活流程（进程内 Rust 插件）

```
loader::load(shared, core, id)
 ├─ 读取并校验清单
 ├─ 拷贝 {id}.so → {id}.{序号}.so（私有副本）
 ├─ libloading 打开 → ling_plugin_create 取实例 → 核对 id()
 ├─ caps::set_declared → 插入 PluginRecord(Activating)
 ├─ instance.activate(ctx)        // 插件在此注册方法/订阅/...
 │    └─ 每步都记入账本（ledger）
 ├─ 成功 → state=Activated → 广播 kernel.plugin.state: activated
 └─ 失败 → 倒序撤销账本 → 清理 → E_ACTIVATE_FAILED
```

### 9.3 调用路由流程

```
ctx.call(plugin, method, args) / Kernel::call
 └─ broker::call
      ├─ full = Shared::full_method_name(plugin, method)
      ├─ 查方法表（缺失→ E_METHOD_MISSING）
      ├─ 校验 owner.state == Activated（否则→ E_PLUGIN_MISSING）
      ├─ requires 能力校验（caps::effective，否则→ E_CAPABILITY_DENIED）
      └─ handler(args)
```

### 9.4 事件广播流程

```
ctx.emit(topic, payload) / Kernel::emit
 └─ bus::emit：校验 topic 以插件编号开头
      └─ Shared::dispatch
           ├─ 遍历订阅表，topic_matches(pattern, topic)（支持 * 通配）
           └─ 逐回调调用（调 SDK 回调只见载荷，内核回调带主题）
```

### 9.5 卸载流程

```
uninstall(id, purge)
 ├─ 停用激活中的传递依赖方（工件保留）
 ├─ stop_plugin(id)：侧车先优雅停进程 → 移除记录 → deactivate() → on_deactivate 回调
 │    → 账本倒序撤销（run_reverse） → 核对残留（methods/subs 无残留）
 ├─ remove_install_files：非 purge 保留 data/；purge 全删
 ├─ 广播 kernel.plugin.state: uninstalled
 └─ reconcile
```

### 9.6 配置调和流程

```
deps::reconcile(shared, core)
 ├─ 重建依赖图（installed_nodes）
 ├─ find_cycle：有环→ E_DEPENDENCY_CYCLE
 ├─ 判定各插件 satisfiable（停用名单 ∩ 依赖满足）
 ├─ 拓扑序 → 先停用逆序、再激活正序的 step_list
 └─ 逐步执行：每步广播 kernel.plan.step；失败→回滚已执行步骤→报告
```

### 9.7 热更新流程（[kernel.rs](crates/ling-kernel/src/kernel.rs) `reload`）

```
reload(id)
 ├─ 校验新清单（目录名 ↔ id 一致）
 ├─ backup_install_artifacts：备份运行版本（清单原文 + 私有库）
 ├─ 停用下游 + 自身
 ├─ loader::load 装新版
 │    ├─ 成功 → discard_backup → reconcile
 │    └─ 失败 → restore_install_artifacts 回滚旧版 → 重新装载 → E_RELOAD_FAILED（旧版继续服务）
```

### 9.8 侧车生命周期

```
sidecar::start
 ├─ TcpListener bind 127.0.0.1:0 → endpoint "tcp:127.0.0.1:<port>"
 ├─ Command 启动进程（env LING_SOCKET + LING_PLUGIN）
 ├─ 接受连接（5s 超时；进程提前退出→失败）
 ├─ 发 {"op":"activate"} → 侧车注册方法/订阅 → 起 monitor 线程
 └─ monitor 循环：
      ├─ register_method → broker::register（handler 转发回侧车 invoke）
      ├─ subscribe → bus::subscribe（回调回发 {"op":"event"}）
      ├─ call/emit → 经 broker/bus 转内核
      ├─ result/error → 返回给 pending 调用方
      └─ 连接断开：
           ├─ 正常停用（stopping）→ 仅 reap
           └─ 崩溃 → crash_cleanup：注销效应 → 广播 crashed → restart=on-crash 时自动重装
```

---

## 附录：文档导航

| 文档 | 内容 |
|---|---|
| [docs/00-总览与术语.md](docs/00-总览与术语.md) | 术语、贯穿示例 |
| [docs/01-宿主设计.md](docs/01-宿主设计.md) | 目标原则、内核四服务、进程线程模型、决策记录 |
| [docs/02-上下文与可逆效应.md](docs/02-上下文与可逆效应.md) | Context、效应登记表、卸载倒序还原 |
| [docs/03-协效应与依赖解析.md](docs/03-协效应与依赖解析.md) | requires、依赖图、激活/停用状态机、配置调和 |
| [docs/04-插件接口规范.md](docs/04-插件接口规范.md) | 清单格式、Rust trait、双轨加载、侧车协议、管理端口协议 |
| [docs/05-热更新.md](docs/05-热更新.md) | 热更新流程、动态库装卸载坑与对策、持久文件纪律 |
| [docs/06-内置插件设计.md](docs/06-内置插件设计.md) | 内置插件目录、三件套、core-job/core-log |
| [docs/07-路线图与验收.md](docs/07-路线图与验收.md) | 阶段划分与验收标准 |
| [docs/08-运行时预案.md](docs/08-运行时预案.md) | 运行时故障处置（运维手册） |