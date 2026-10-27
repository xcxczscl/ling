# resources-manager — 多设备文件资源管理

个人用的文件资源管理程序：**归纳、整理、分类、按需同步、去重、压缩、备份**，
并且在设备离线、硬盘不插的时候，仍然记得这些数据的存在。

它以**插件**形式运行在 [ling 宿主](../../main/README.md) 上——ling 提供加载、互调、事件、依赖、热更新；
resources-manager 只提供文件管理的业务能力。

## 进度

- **M1（扫描、查重、词条）已完成**：catalog/scan/dedup/semantic/classify/plugin/rmgr + 集成测试；验收 A1–A5、A8、A9 后端 + A12 部分（Q1–Q3 EXPLAIN）。
- **M2（关系与记录同步）已完成**：schema v2（connection/op/peer/sync_conflict）+ rm-sync（导出/合并/墓碑/冲突）+ plugin M2 方法 + rmgr connection/sync/device pair + devices 页；A6/A7/A11 领域级测试绿。
- **M3（局域网数据同步）已完成**：Ed25519 真握手 + LAN 监听 + 断点续传 + 按需策略（schema V3）；`m3_acceptance` A14–A20 全绿。
- **M4（备份与压缩）已完成**：段式备份/恢复/校验（`rm-backup`）+ zstd 阈值压缩与占位加密（`rm-store`）+ 池归纳/迁移/删除（`pool.ingest/move/remove`）；schema V4（`snapshot` 表）；`m4_acceptance` 5 项全绿。
- **M5（AI 与刮削）已完成**：建议表（`ai_suggestion`）+ 隐私档位表（`ai_privacy`）+ 分级确认（低风险自动采纳/高风险人工）+ MockProvider + `rm-ai` 领域包；schema V5；`m5_acceptance` 5 项全绿。
- **M6（三端打包）已完成**：发行包组装（`scripts/package.sh`）+ Linux systemd / Windows 服务脚本 + rm-fs 跨平台补齐（Windows `\\?\` 长路径、安卓 `SafRegistry` 接口占位）；`m6_acceptance` 3 项全绿。

**M1–M6 全部里程碑完成（2026-10-17）**。

## 文档（先读 00）

| 顺序 | 文档 | 回答什么 |
|---|---|---|
| ① | [../../docs/resources-manager/00-总览与术语.md](../../docs/resources-manager/00-总览与术语.md) | 名词解释、贯穿示例（用户与三台设备）、路线图 |
| ② | [../../docs/resources-manager/01-总体设计.md](../../docs/resources-manager/01-总体设计.md) | 问题、决策、四层数据模型、业务流程、里程碑与验收、**评审意见登记区** |
| ③ | [../../docs/resources-manager/02-数据字典.md](../../docs/resources-manager/02-数据字典.md) | 每张表、每一列的含义与示例数据 |
| ④ | [../../docs/resources-manager/03-接口与协议.md](../../docs/resources-manager/03-接口与协议.md) | RPC 接口、`rmgr` 命令行、同步/备份协议、代码与测试规矩 |
| ⑤ | [../../docs/resources-manager/04-项目结构.md](../../docs/resources-manager/04-项目结构.md) | `rm-*` 包划分、目录与依赖规则 |

共用内容见 [`../../docs/common/`](../../docs/common/)：文档编写规范、公共术语、论文机制说明。
宿主设计见 [`../../docs/ling/`](../../docs/ling/)。

## 当前状态

**已封档（终审通过）；M1–M6 编码完成（全部里程碑）。** 设计经评审轮、四方案落盘、最后补充与终审后冻结；封档后改动必须走变更记录与 docs/resources-manager/01 附录 B。

> **命名说明**：本项目曾用名 file-manager，已全量改名 resources-manager（插件编号 `resources-manager`、命令行 `rmgr`、crate 前缀 `rm-`、事件/方法命名空间 `resources-manager.*`）。

## 命令行

- `rmgr …` —— 本项目业务命令（`scan` / `dup` / `entry` / `sync`…），见 docs/resources-manager/03 §3
- `ling …` —— 宿主管理命令（`serve` / `plugins` / `reload`），见 docs/ling/04 §7

## 关键决策速览（详见 docs/resources-manager/01 附录 A）

- 语义层（词条/版本/标签分域/关系）排进首批交付
- 去重只做**文件级**（分块去重留扩展位）；删除前四道检查写死不可绕过
- 记录同步与数据同步分离：默认同步记录，传输文件必须用户显式发起
- AI 只出建议、经分级确认入库；永不参与删除/去重/备份决策
- 首次交付 = M1–M2（M0 宿主已移交 ling）
