# 02 SDK 与插件 API 参考

> **插件只需要依赖 `ling-sdk` 这一个 crate**。本文档列出 SDK 全部公开 API、`plugin.toml` 清单格式、以及常见用法示例。
> 
> 想知道"某个 API 内部怎么实现"，请看 [01-内核模块详解](01-内核模块详解.md)；想知道"为什么这样设计"，请看 `docs/ling/04-插件接口规范.md`。

---

## 1. 插件最小模板

### 1.1 Cargo.toml

```toml
[package]
name = "my-plugin"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]    # ★ 必须是 cdylib，产出 .dll/.so

[dependencies]
ling-sdk = { path = "../../main/crates/ling-sdk" }
serde_json = "1"
```

### 1.2 src/lib.rs

```rust
use ling_sdk::*;

pub struct MyPlugin { ctx: Option<Context> }

impl Plugin for MyPlugin {
    fn id(&self) -> &str { "my-plugin" }

    fn activate(&mut self, ctx: Context) -> Result<(), ActivateError> {
        // ★ 所有注册/订阅都经 ctx，内核自动记账
        ctx.register_method(
            "my-plugin.hello",
            |args| Ok(json!({"message": "hi", "args": args})),
            || { /* unregister: 空操作（内核已经帮我们从方法表移除了） */ },
            None,  // 这个方法不需要特殊能力
        ).map_err(|e| ActivateError::new(e.message()))?;

        ctx.subscribe(
            "kernel.plugin.state",
            |payload| { println!("plugin state changed: {:?}", payload); },
            || { /* unsubscribe 回调 */ },
        ).map_err(|e| ActivateError::new(e.message()))?;

        self.ctx = Some(ctx);
        Ok(())
    }

    fn deactivate(&mut self) {
        // ★ 在这里收敛线程、落盘
        // 撤销账本由内核自动执行，这里只做插件私有状态的清理
    }
}

// ★ 必须导出这个函数，内核通过它拿到 Plugin 实例
#[no_mangle]
pub extern "C" fn ling_plugin_create() -> *mut dyn Plugin {
    Box::into_raw(Box::new(MyPlugin { ctx: None }))
}
```

### 1.3 plugin.toml

```toml
[plugin]
id = "my-plugin"                    # 必须与 Plugin::id() 一致
name = "我的插件"
version = "0.1.0"                   # X.Y.Z 三段式
api = "1"                           # 必须等于 HOST_API

[requires]
capabilities = ["fs", "network"]    # 声明需要的能力
plugins = ["core-ui@1"]             # 声明依赖的其他插件（可选）
# sidecar = { ... }                 # 侧车插件才需要
```

### 1.4 构建产物

```powershell
cargo build --release
# 产出：target/release/libmy_plugin.dll  (Windows) 或 libmy_plugin.so (Linux)
# 拷贝到：ling/plugins/my-plugin/my-plugin.dll + plugin.toml
```

---

## 2. Plugin trait — 插件必须实现

```rust
pub trait Plugin: Send + Sync {
    fn id(&self) -> &str;
    fn activate(&mut self, ctx: Context) -> Result<(), ActivateError>;
    fn deactivate(&mut self);
}
```

### 2.1 三个生命周期回调

| 回调 | 何时调用 | 应该做什么 | 不能做什么 |
|---|---|---|---|
| `id()` | **装载前**，内核核对清单 id 时 | 返回与 `plugin.toml` 完全一致的字符串 | 返回不一样的 id → 装载失败 |
| `activate(ctx)` | **装载中**，动态库已打开、私有库已复制 | 用 ctx 注册方法、订阅事件、写设置 | 直接 `std::fs::write`（绕过上下文 → 卸载不干净）；启动不收敛的线程 |
| `deactivate()` | **停用/卸载/热更新前** | 收敛自己的线程、落盘 | 访问 ctx（此时上下文可能已半销毁） |

**`activate` 失败怎么办**：返回 `Err(ActivateError::new("..."))` → 内核倒序执行已记账本的撤销闭包 → dlclose 动态库 → 插件保持"未安装"状态。

---

## 3. Context — 插件的全部能力

`Context` 是插件和内核之间的**唯一桥梁**。它内部持有 `Arc<dyn ContextCore>`，内核实现 `ContextCore` trait。

```rust
#[derive(Clone)]
pub struct Context { /* 内部字段不暴露 */ }
```

### 3.1 注册方法

```rust
pub fn register_method(
    &self,
    name: &str,                              // 如 "my-plugin.scan.start"
    handler: MethodHandler,                   // Box<dyn Fn(Value) -> Result<Value, CallError>>
    unregister: UndoFn,                      // Box<dyn FnOnce()> —— ★ 必须提供
    requires: Option<Capability>,            // 这个方法执行时需要什么能力
) -> Result<(), ContextError>
```

| 参数 | 说明 |
|---|---|
| `name` | **必须**以插件编号开头。`full_method_name` 校验，否则 `E_NAMESPACE_VIOLATION` |
| `handler` | 入参和返回全走 `serde_json::Value` 序列化边界 |
| `unregister` | 撤销闭包。内核会把它**也**作为"从方法表移除"的配套动作执行。多数插件传空闭包就行，因为移除方法表是内核自己的事；但如果 handler 里注册了插件私有的内部回调，就要在这里撤掉 |
| `requires` | 声明调用这个方法需要什么能力。调用瞬间 broker 会再次校验目标插件的**生效**能力 |

**错误**：
- `E_NAMESPACE_VIOLATION` — name 没带插件前缀
- `E_CAPABILITY_DENIED` — requires 里声明的能力不在清单里

### 3.2 跨插件调用

```rust
// 同步调用
pub fn call(
    &self,
    plugin: &str,       // 目标插件编号，如 "resources-manager"
    method: &str,       // 方法名，如 "scan.start"（不用带目标插件前缀）
    args: Value,        // 参数，JSON Value
) -> Result<Value, CallError>

// 异步调用（返回 Future，可在 async 上下文 await）
pub fn call_async(
    &self,
    plugin: &str,
    method: &str,
    args: Value,
) -> BoxFuture<'static, Result<Value, CallError>>
```

**调用流程**（同进程 Rust 插件）：
```
ctx.call("core-settings", "get", json!({"key": "theme"}))
  → broker::call: full_name = "core-settings.get"
  → 查方法表 → 存在
  → 校验 core-settings.state == Activated
  → 校验 core-settings 的生效能力足够
  → handler(json!({"key": "theme"})) → Result<Value, CallError>
```

**错误**：
- `E_PLUGIN_MISSING` — 目标插件未激活/未装载
- `E_METHOD_MISSING` — 方法未注册
- `E_CAPABILITY_DENIED` — 目标插件能力已被撤销

### 3.3 订阅事件

```rust
pub fn subscribe(
    &self,
    topic_pattern: &str,    // 如 "kernel.plugin.state" 或 "resources-manager.*"
    callback: EventCallback,
    unsubscribe: UndoFn,
) -> Result<(), ContextError>
```

- `topic_pattern` 支持 `*` 通配（子串匹配）。`"kernel.*"` 匹配所有 `kernel.` 开头的主题。
- 订阅**不限命名空间**——任何插件可以订阅任何主题。
- 广播时同步回调，单个订阅者 panic 不阻断其他订阅者。

### 3.4 广播事件

```rust
pub fn emit(
    &self,
    topic: &str,       // 如 "my-plugin.scan.progress"
    payload: Value,    // JSON Value
) -> Result<(), ContextError>
```

**必须**以自己插件编号开头。越权广播返回 `E_NAMESPACE_VIOLATION` 并记录 violation（供 doctor 检查）。

### 3.5 能力查询

```rust
pub fn has_capability(&self, cap: Capability) -> bool
```

读自己清单声明的能力（只看声明，不看撤销状态）。

### 3.6 数据目录

```rust
pub fn data_dir(&self) -> PathBuf
```

返回 `ling/plugins/<编号>/data/`，内核保证目录已创建。插件的所有持久状态都应该存这里。

### 3.7 停用回调

```rust
pub fn on_deactivate(&self, callback: UndoFn)
```

在停用/卸载/热更新前调用，用于**落盘收尾**（不是状态保存的替代——状态应始终持久化在 data/ 里）。

### 3.8 查看账本

```rust
pub fn ledger(&self) -> Vec<LedgerEntryInfo>
```

返回账本快照（seq + kind + desc），撤销闭包不外泄。主要用于调试。

---

## 4. Capability 枚举

```rust
pub enum Capability {
    Fs,        // 文件系统：读写文件、扫描目录
    Network,   // 网络：HTTP 请求、监听端口
    Ui,        // 界面：启动 HTTP 服务、贡献页面
    Shell,     // Shell：启动子进程、执行命令
}
```

| 方法 | 说明 |
|---|---|
| `as_str(self) -> &'static str` | `"fs"` / `"network"` / `"ui"` / `"shell"` |
| `parse(s: &str) -> Option<Capability>` | 从字符串解析 |
| `ALL: [Capability; 4]` | 全部四个能力的数组 |

---

## 5. 错误类型

### 5.1 CallError — 跨插件调用错误

```rust
pub struct CallError { code: String, message: String }
```

| 工厂方法 | code | 场景 |
|---|---|---|
| `CallError::plugin_missing(plugin)` | `E_PLUGIN_MISSING` | 目标插件未激活 |
| `CallError::method_missing(method)` | `E_METHOD_MISSING` | 方法未注册 |
| `CallError::capability_denied(detail)` | `E_CAPABILITY_DENIED` | 能力被门禁拒绝 |

### 5.2 ContextError — 上下文操作错误

```rust
pub struct ContextError { code: String, message: String }
```

| 工厂方法 | code | 场景 |
|---|---|---|
| `ContextError::namespace_violation(detail)` | `E_NAMESPACE_VIOLATION` | 方法名/事件主题没带插件前缀 |
| `ContextError::capability_denied(detail)` | `E_CAPABILITY_DENIED` | 注册时声明了清单外的能力 |

### 5.3 ActivateError — activate 失败

```rust
pub struct ActivateError { message: String }
```

```rust
ActivateError::new("数据库初始化失败")
```

---

## 6. EffectKind — 效应种类

账本条目的分类，供 `ctx.ledger()` 查看：

```rust
pub enum EffectKind {
    MethodRegistered,      // 注册了方法
    EventSubscribed,       // 订阅了事件
    ConfigWritten,         // 写了配置（目前内核级）
    SidecarStarted,        // 启动了侧车
    DeactivateCallback,    // 登记了停用回调
}
```

---

## 7. 错误码速查表

| 常量 | 字符串 | 含义 |
|---|---|---|
| `E_MANIFEST_INVALID` | `"E_MANIFEST_INVALID"` | 清单非法 |
| `E_API_MISMATCH` | `"E_API_MISMATCH"` | 插件 api ≠ HOST_API |
| `E_CAPABILITY_UNKNOWN` | `"E_CAPABILITY_UNKNOWN"` | 清单声明了宿主不认识的能力 |
| `E_CAPABILITY_DENIED` | `"E_CAPABILITY_DENIED"` | 能力被门禁拒绝（未声明或已撤销） |
| `E_NAMESPACE_VIOLATION` | `"E_NAMESPACE_VIOLATION"` | 方法名/事件主题越权 |
| `E_PLUGIN_MISSING` | `"E_PLUGIN_MISSING"` | 目标插件未激活 |
| `E_METHOD_MISSING` | `"E_METHOD_MISSING"` | 方法未注册 |
| `E_DEPENDENCY_UNMET` | `"E_DEPENDENCY_UNMET"` | 依赖未满足 |
| `E_DEPENDENCY_CYCLE` | `"E_DEPENDENCY_CYCLE"` | 依赖成环 |
| `E_ACTIVATE_FAILED` | `"E_ACTIVATE_FAILED"` | activate 返回 Err |
| `E_UNDO_FAILED` | `"E_UNDO_FAILED"` | 撤销闭包 panic |
| `E_RELOAD_FAILED` | `"E_RELOAD_FAILED"` | 热更新失败 |
| `E_TIMEOUT` | `"E_TIMEOUT"` | 侧车调用超时（30s） |
| `E_SIDE_CRASHED` | `"E_SIDE_CRASHED"` | 侧车连接断开 |
| `E_AUTH` | `"E_AUTH"` | 管理端口认证失败 |

---

## 8. plugin.toml 完整格式

```toml
[plugin]
id = "my-plugin"                  # 必填，小写字母/数字/短横线
name = "我的插件"                  # 必填，人类可读
version = "0.1.0"                 # 必填，X.Y.Z
api = "1"                         # 必填，必须等于 HOST_API

[requires]
capabilities = ["fs", "network"]  # 可选，声明需要的能力
plugins = ["core-ui@1", "resources-manager@1.0"]  # 可选，声明依赖的插件

# --- 侧车插件才需要 ---
[sidecar]
executable = "sidecar/my_plugin.py"   # 相对路径，不能包含 ..
protocol = "jsonl"                      # 必填，固定值
restart = "on-crash"                    # 或 "never"
```

### 版本约束规则（`requires.plugins`）

| 写法 | 含义 |
|---|---|
| `resources-manager@1` | 主版本=1（所有 1.x.y） |
| `resources-manager@1.2` | 主版本=1 且次版本≥2（1.2.y, 1.3.y, ...） |
| `resources-manager@1.2.3` | 精确版本=1.2.3 |
| `resources-manager` | 任何版本（不推荐） |

按**段前缀相等**判定：声明 `"@1"` 匹配 version `"1.0.0"` 和 `"1.2.3"`，但不匹配 `"2.0.0"`。

---

## 9. 核心事件主题约定

### 9.1 内核事件

| 主题 | 载荷 | 广播时机 |
|---|---|---|
| `kernel.plugin.state` | `{"plugin": "xxx", "state": "activated"\|"deactivated"\|"crashed"\|"uninstalled"}` | 每次插件状态变化 |
| `kernel.plan.step` | `{"step": 0, "plugin": "xxx", "action": "activate"\|"deactivate", "result": "ok"\|...}` | 调和每步完成 |

### 9.2 命名空间规则

- 插件自己广播的事件主题**必须**以自己编号开头，如 `my-plugin.scan.progress`
- 订阅不限命名空间，可订阅任何前缀的事件

---

## 10. 类型别名速查

```rust
pub type MethodHandler = Box<dyn Fn(Value) -> Result<Value, CallError> + Send + Sync>;
pub type EventCallback = Box<dyn Fn(Value) + Send + Sync>;
pub type UndoFn        = Box<dyn FnOnce() + Send>;
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
```

### serde_json::Value — 跨插件数据的唯一格式

```rust
use ling_sdk::json;

let empty = json!({});
let data = json!({
    "name": "hello",
    "count": 42,
    "list": [1, 2, 3]
});
```

插件之间传递的所有数据（调用参数、返回值、事件载荷）都必须能序列化/反序列化为 `Value`。不要在跨插件边界传 Rust 原生类型（`&str`、`HashMap<String, Vec<u8>>` 等）。
