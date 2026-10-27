# 工作区总索引 — ling + resources-manager + player

本工作区是一个**一切皆插件**的软件系统：宿主程序 + 两个业务插件 + 一套公共设计文档。

| 目录 | 是什么 | 状态 |
|---|---|---|
| [main/](main/) | **宿主程序**（ling）：零内容，只提供插件加载、调用路由、事件广播、能力门禁、依赖解析、热更新的机制 | 设计已封档；**L1–L6 全部完成**（验收 5/5 + 4/4 + 5/5 + 5/5 + 6/6 + 4/4） |
| [plugins/resources-manager/](plugins/resources-manager/) | **资源管理插件**：多设备文件的归纳、整理、分类、按需同步、去重、压缩、备份 | 设计已封档；**M1–M6 全部完成** |
| [plugins/player/](plugins/player/) | **内容执行器插件**：串流播放、图片/PDF 查看、沙箱运行、远程执行与接力 | 设计已封档；**P0–P7 全部完成** |
| [docs/common/](docs/common/) | 三项目共用的说明：文档编写规范、公共术语与命名约定、论文机制白话解读 | v1.4 |
| [archive/](archive/) | 里程碑归档：每个里程碑完成后的快照记录（产出/修改文件、校验和） | L0–L6、M1–M6、P0–P7 全部归档 |

**关系一句话**：main 是"能装插件的空壳"，resources-manager 是"第一个业务插件"，player 是"把内容用起来"的执行器；以后的记账、网站管理等，都是新的插件。

## 工作区目录约定

工作区根目录下的 `run/` 文件夹专门存放**编译产物**和**可下载工具链**，源码与设计文档中不得包含以下内容：

| 应放入 `run/` 的内容 | 示例 |
|---|---|
| Rust 编译缓存 | 各子项目的 `target/` 目录 |
| 预编译发布包 | 各子项目的 `dist/` 目录（.dll / .so / .exe） |
| 外部工具链 | Android NDK、交叉编译工具链、ffmpeg、libmpv 等 |
| 构建中间文件 | `.cargo-lock`、`.cargo-build-lock`、`.d` 依赖文件等 |

**理由**：以上内容合计可占数 GB 空间，均可由 `cargo build` 重新生成或从官方渠道重新下载。
源码仓库只需保留 `Cargo.toml` / `Cargo.lock` / `plugin.toml` 等声明文件，以及 `docs/`、`archive/` 中的设计文档。

## 推荐阅读顺序

1. [docs/common/00-文档编写规范.md](docs/common/00-文档编写规范.md) —— 文档怎么写（10 条规矩）
2. [docs/common/01-公共术语与约定.md](docs/common/01-公共术语与约定.md) —— 公共名词、rm-/ling- 命名、端口与目录约定
3. [docs/common/02-时空可组合性机制说明.md](docs/common/02-时空可组合性机制说明.md) —— ling 采纳的论文机制（可逆效应、协效应、上下文、热更新）
4. [main/README.md](main/README.md) —— 宿主设计文档索引（9 份，从 docs/ling/00 开始）
5. [plugins/resources-manager/README.md](plugins/resources-manager/README.md) —— 资源管理插件设计文档索引（5 份，从 docs/resources-manager/00 开始）
6. [plugins/player/README.md](plugins/player/README.md) —— player 设计文档索引（2 份，从 docs/player/00 开始）

## 关键决策速览

| 决策 | 内容 | 出处 |
|---|---|---|
| 项目结构 | 拆成宿主（main/）、插件（plugins/*）、文档（docs/）、归档（archive/）四部分 | 本次拆分 |
| 宿主技术栈 | Rust；内核 4 服务模块化（路由/事件/能力/依赖），配置、作业、日志为内置插件（不进内核） | docs/ling/01 附录 A-2 |
| 插件形态 | 双轨：Rust 插件进程内动态库，非 Rust 插件进程外侧车 | docs/ling/01 附录 A-1 |
| 机制采纳 | 论文的可逆效应 + 响应式协效应 + 上下文范式，**首轮即含热更新** | docs/ling/01 附录 A-4、A-7 |
| 插件状态 | 状态放插件自己的持久文件，内存不留（热更新即干净重来） | docs/ling/01 附录 A-5 |
| 内置插件 | 放 plugins/builtin-plugins/（从属）；首次默认装三件套、可卸载 | docs/ling/06 |
| resources-manager 技术栈 | Rust + Tauri 2 + SQLite(WAL) + React | docs/resources-manager/01 §3 |
| 去重 | 文件级（分块留扩展位）；删除前四道检查写死 | docs/resources-manager/01 附录 A-3 |
| AI | 混合形态 + 分级确认；永不参与删除/去重/备份决策 | docs/resources-manager/01 附录 A-6 |

## 终审封档结论（2026-10-16）

**流程**：最后补充（结构分析、缺漏补齐，12 处）→ 最终审查（问题发现与纠错、可行性评估）→ 终审封档。

**可行性评估**：
- **机制层（ling）**：可行。四个已知风险点全部有对策与验收兜底——Rust 动态库装卸载坑（docs/ling/05 §3 四坑对策 + L4 连续 100 次 reload 压测）、内核不可运行时替换（A-2 编译时可替换，个人工具够用）、卸载不干净（账本逐条容错 + doctor）、api 升级不兼容（N/N-1 策略 + docs/ling/08 §2.8 预案）。
- **业务层（resources-manager）**：可行。最高风险"归纳删源"与"多设备合并"分别有删源铁律（校验前零删除、文件先行、三断点恢复）与收敛证明（resolve=更晚新 op）兜底；性能有 p95 预算 + EXPLAIN 断言进 CI；数据安全有属性测试三条不变式。
- **规模匹配**：单用户、几十插件、TB 级数据、SQLite + 回环 HTTP + Rust dlopen——全部在成熟技术的舒适区内，无未验证的新技术依赖。
- **剩余风险（已登记，不阻塞）**：dlclose 坑需 L4 实测（退路：侧车轨道）；安卓 SAF 限制（已接受，docs/resources-manager/01 §13 #1）；冲突 UI 为 M2 工作量增项（评审已记）；性能预算未经实测（bench 进 CI 兜底）。

**结论：批准封档。** 两项目文档自本日起冻结为设计基线。

### player 设计终审封档结论（2026-10-17）

**流程**：设计问答（Q1–Q7 用户拍板）→ 起草 00/01 → 最后补充（16 处）→ 问题修复 → 可行性评估 → 终审封档。

**可行性评估**：
- **核心播放（P0）**：可行。libmpv（LGPL，动态链接）成熟封装解码/seek/字幕/音画同步。
- **串流（P3/P4）**：可行。模式 A 本地代理绕过 rm 流 127.0.0.1 限制；重编码用 FFmpeg 生态成熟。
- **沙箱运行（P5）**：可行。OS 级隔离（seccomp/Job Object）是成熟方案。
- **远程执行 + 会话接力（P7）**：可行但复杂度最高。依赖 P3–P6 全部就绪。
- **剩余风险（已登记，不阻塞）**：Android Surface 集成待壳层就绪后验证；重编码延迟需硬件编码优化（P4）；沙箱可能不兼容个别安装程序（权限授权兜底）。

**结论：批准封档。** player 文档自本日起冻结为设计基线。

### 全部里程碑收官（2026-10-18）

三项目所有里程碑编码完成，详见各子目录 README 与 archive/ 归档：

| 里程碑 | 完成日期 | 归档 |
|---|---|---|
| ling L1–L6（宿主内核六阶段） | 2026-10-16 | archive/L1 ~ archive/L6 |
| resources-manager M1–M6（文件管理六阶段） | 2026-10-16 ~ 17 | archive/M1 ~ archive/M6 |
| player P0–P7（内容执行八阶段） | 2026-10-17 ~ 18 | archive/P0 ~ archive/P7 |

## 阅读与评审

- 宿主评审意见区：`docs/ling/01-宿主设计.md` 附录 B
- resources-manager 评审意见区：`docs/resources-manager/01-总体设计.md` 附录 B
- player 评审意见区：`docs/player/01-总体设计.md` 附录 B
- 文档与代码冲突时：以对应项目的 `docs/01` 为准；先改文档再改代码。
