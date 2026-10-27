# 里程碑归档（Traceability Archive）

按用户工作流要求：**每完成一个里程碑就归档一次**，记录该阶段产出的文件与对既有文件的修改，供以后溯源。设计阶段同样归档（本仓库无 git，归档即阶段快照）。

## 归档制度

- **时机**：里程碑验收通过、状态更新完成之后，进入下一里程碑之前，立即归档。
- **位置**：本目录 `archive/`，每个里程碑一个子记录文件，命名 `阶段ID-名称.md`。
- **内容**（每份记录固定包含）：
  1. 里程碑 ID、名称、日期、前置依赖、状态；
  2. **产出文件**——本阶段从零创建的文件（路径 + 一句话说明）；
  3. **修改文件**——本阶段对既有文件的改动（路径 + 改动摘要 + 版本号变化）；
  4. 过程关键决策与变更登记（附录 B 行号、审查轮次等）；
  5. 验收结果与复验命令；
  6. **校验和**——归档时全量文件 sha256 清单（除 `run/target/`、`run/dist/`、`archive/` 自身、core dumps），用于事后比对文件是否被改动。
- **模板**：新记录沿用 `L1-内核骨架.md` 的结构。

## 记录索引

| 记录 | 里程碑 | 日期 | 说明 |
|---|---|---|---|
| [L0-设计评审与封档.md](L0-设计评审与封档.md) | L0（全工作区设计阶段） | 2026-10-15 ~ 2026-10-16 | 四项目文档创建、最后补充、终审封档；含 resources-manager 设计线与 docs/common |
| [L1-内核骨架.md](L1-内核骨架.md) | L1 | 2026-10-16 | Rust 工作区、SDK、内核、demo-a/demo-b、L1 五项验收，含全量 sha256 |
| [L2-上下文与效应账本.md](L2-上下文与效应账本.md) | L2 | 2026-10-16 | 卸载还原、UninstallReport、故障隔离、文件语义登记，含全量 sha256 |
| [L3-依赖与配置调和.md](L3-依赖与配置调和.md) | L3 | 2026-10-16 | 依赖图、版本前缀匹配、环检测带路径、自动激活/停用、registry.json，含全量 sha256 |
| [L4-热更新与命令行.md](L4-热更新与命令行.md) | L4 | 2026-10-16 | reload 回滚、私有副本装载防 SIGBUS、管理端口 + 会话口令、`ling` CLI e2e，含全量 sha256 |
| [L5-内置插件.md](L5-内置插件.md) | L5 | 2026-10-16 | 三件套首启自动安装、core-ui 网页壳与 /rpc、设置 schema、可选内置插件，含全量 sha256 |
| [L6-侧车双轨.md](L6-侧车双轨.md) | L6 | 2026-10-16 | 侧车 jsonl 控制面、Python 示例、崩溃善后与 on-crash 重启，含全量 sha256。ling 编码线 L1–L6 至此完成 |
| [M1-扫描查重词条.md](M1-扫描查重词条.md) | resources-manager M1 | 2026-10-16 | catalog/scan/dedup/semantic/classify/plugin/rmgr；A1–A5/A8/A9 后端 + A12 部分；含全量 sha256 |
| [M2-关系与记录同步.md](M2-关系与记录同步.md) | resources-manager M2 | 2026-10-16 | schema v2 + rm-sync + plugin M2 方法 + devices 页；A6/A7/A11 绿；含全量 sha256 |
| [M3-局域网数据同步.md](M3-局域网数据同步.md) | resources-manager M3 | 2026-10-17 | Ed25519 真握手 + LAN 监听 + 断点续传 + 按需策略；A14–A20 绿；含全量 sha256 |
| [M4-备份与压缩.md](M4-备份与压缩.md) | resources-manager M4 | 2026-10-17 | 段式备份/恢复/校验 + zstd + 池归纳；m4_acceptance 5 项绿；含全量 sha256 |
| [M5-AI与刮削.md](M5-AI与刮削.md) | resources-manager M5 | 2026-10-17 | AI 建议表 + 分级确认 + 隐私三档 + MockProvider；m5_acceptance 5 项绿；含全量 sha256 |
| [M6-三端打包.md](M6-三端打包.md) | resources-manager M6 | 2026-10-17 | 发行包组装 + systemd/Windows 服务 + rm-fs 跨平台；m6_acceptance 3 项绿。**ling + resources-manager 全部里程碑完成** |
| [P0-本地最小播放.md](P0-本地最小播放.md) | player P0 | 2026-10-17 | 前置补齐 rm 流交付契约 + player 插件本体（libmpv 管道/状态机/控制页/P 族错误）；p0_acceptance 4 绿 + 单测 8 绿；含全量 sha256 |
| [P1-图片与PDF.md](P1-图片与PDF.md) | player P1 | 2026-10-17 | 图片/PDF 查看（asset 分流、查看器会话、goto_page/frame/set_subtitle）；p1_acceptance 5 绿 + 单测 26 绿；含全量 sha256 |
| [P2-输出与UI统一.md](P2-输出与UI统一.md) | player P2 | 2026-10-17 | 输出设备枚举/切换 + UI 共享 shell 模板；p2_acceptance 3 绿 + 单测 33 绿；含全量 sha256 |
| [P3-串流输出v1.md](P3-串流输出v1.md) | player P3 | 2026-10-17 | 模式 A 本地代理 + UDP 心跳发现 + player.cast + player-lite bin；p3_acceptance 3 绿 + 单测 49 绿；含全量 sha256 |
| [P4-串流输出v2.md](P4-串流输出v2.md) | player P4 | 2026-10-17 | 自动/手动路由 + 模式 B/C 现场转码 + tv.rs 占位；p4_acceptance 3 绿 + 单测 58 绿；含全量 sha256 |
| [P5-沙箱运行.md](P5-沙箱运行.md) | player P5 | 2026-10-18 | Landlock + seccomp + 方法面 run/list/stop；p5_acceptance e1–e3 绿 + 单测 65 绿；含全量 sha256 |
| [P6-会话持久化.md](P6-会话持久化.md) | player P6 | 2026-10-18 | player.db WAL + 写穿钩子 + 跨重启续播；p6_acceptance e1–e3 绿 + 单测 71 绿；含全量 sha256 |
| [P7-远程执行与接力.md](P7-远程执行与接力.md) | player P7 | 2026-10-18 | HTTP 控制平面 7917 + 会话同步 LWW + stdin JSONL 输入；p7_acceptance e1–e3 绿 + 单测 82 绿。**ling + resources-manager + player 全部里程碑收官** |

## 待归档队列

- （空——ling L0–L6、resources-manager M1–M6、player P0–P7 **全部归档完毕**；三项目所有里程碑收官，无后续队列。）
