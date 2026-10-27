# 开发手册（docs/dev/）

> **本目录与 `docs/ling/`、`docs/player/`、`docs/resources-manager/` 下的设计文档互为补充**：
> - **设计文档**（`docs/<组件>/00-xxx.md` 等）回答 **"为什么这么设计、整体结构是什么"**——面向架构评审、决策记录。
> - **开发手册**（本目录）回答 **"怎么构建、API 长什么样、模块怎么协作、调试跑起来"**——面向实际编码、调试、扩展。
>
> 设计文档封档后不再大改；开发手册随代码迭代持续更新。

---

## 文档索引

### [main + 内置插件](main/)

宿主内核、CLI、SDK 以及五个内置插件的开发手册。

| 文档 | 内容 |
|---|---|
| [00-构建指南](main/00-构建指南.md) | Rust 工具链准备、workspace 结构、`build.ps1` 使用、Windows/Linux 差异 |
| [01-内核模块详解](main/01-内核模块详解.md) | `ling-kernel` 的 14 个源文件：每个文件做什么、关键 struct、公开 API、模块协作关系 |
| [02-SDK与插件API参考](main/02-SDK与插件API参考.md) | `Plugin` trait、`Context` 全部方法、`Capability`、错误码、`plugin.toml` 清单格式 |
| [03-内置插件开发模式](main/03-内置插件开发模式.md) | 五个内置插件（core-ui / core-settings / core-plugin-manager / core-job / core-log）各自做了什么、注册了什么方法、贡献了什么界面、如何依赖 |
| [04-扩展与调试](main/04-扩展与调试.md) | 写一个新插件的最小步骤、端到端调试方法、常见坑与排查 |

### [resources-manager](rmgr/)

文件资源管理插件（`rmgr` 命令行 + `resources-manager` 插件）的开发手册。

| 文档 | 内容 |
|---|---|
| [00-构建指南](rmgr/00-构建指南.md) | rm 工作区结构、`rm-*` 子 crate 划分、编译与独立运行 |
| [01-模块详解](rmgr/01-模块详解.md) | 每个 `rm-*` crate 做什么、插件壳如何把它们组装起来、方法注册与事件广播 |
| [02-数据模型与持久化](rmgr/02-数据模型与持久化.md) | SQLite 目录库（catalog）、放置/词条/版本/标签/关系表、扫描与分层哈希、存储池与压缩 |
| [03-同步与传输](rmgr/03-同步与传输.md) | 记录同步 op-log、冲突解决、数据同步传输协议、LAN 发现与配对 |

### [player](player/)

内容执行器插件（播放器、查看器、沙箱、串流、远程执行）的开发手册。

| 文档 | 内容 |
|---|---|
| [00-构建指南](player/00-构建指南.md) | player 工作区结构、MPV SDK 准备、`libmpv2-sys` 链接配置 |
| [01-模块详解](player/01-模块详解.md) | 五区模型（Source / Capability / Sink / Session / Discovery）对应的源码文件、`MpvBackend` 封装、会话管理 |
| [02-播放与输出](player/02-播放与输出.md) | 管道模型、自动/手动路由选择、串流模式 A/B/C 详解、输出设备枚举 |
| [03-沙箱与远程执行](player/03-沙箱与远程执行.md) | P5 跨平台沙箱（Unix Landlock/seccomp → Windows tasklist/taskkill 降级）、P7 控制平面 HTTP 服务、远程运行流程 |

---

## 快速导航

**第一次上手？建议阅读顺序**：

1. `docs/dev/main/00-构建指南.md` — 把环境搭起来，让 `ling serve` 跑通
2. `docs/dev/main/02-SDK与插件API参考.md` — 了解插件怎么写
3. `docs/dev/main/03-内置插件开发模式.md` — 看五个内置插件作为范例
4. 根据兴趣选 rmgr 或 player 深入

**想知道某个函数/模块在哪？**

各组件的 `01-模块详解` 都有完整的"源码文件 → 职责 → 关键结构"映射表，可当索引用。

---

## 与设计文档的交叉引用

开发手册里涉及"为什么"的决策，会标注对应的设计文档章节，例如：

> 配置调和触发时机详见 `docs/ling/01-宿主设计.md` §6

这样既避免重复，也让两套文档保持可追溯性。
