# M5 — AI 与刮削（resources-manager）

| 项 | 值 |
|---|---|
| 里程碑 | M5（`resources-manager/docs/01` §12.1） |
| 日期 | 2026-10-17 |
| 前置 | 设计封档（L0）、ling L1–L6、rm M1–M4 完成 |
| 状态 | **完成**（编码 + 验收 + 归档） |

## 范围（对照 `docs/01` §7.8 / `docs/03` §5.4）

- **建议表机制**：AI 所有产出进 `ai_suggestion`（schema V5），绝不直接写业务表；状态 `pending/accepted/rejected/stale` 流转。
- **分级确认**：低风险（tag/metadata）置信度 ≥ 0.85 自动采纳（source=ai，可 revoke）；高风险（connection/entry_create/entry_match/version_split）一律 pending 人工。
- **隐私三档**：`local/cloud/hybrid`，按路径前缀存 `ai_privacy` 表，最长匹配优先，默认 hybrid。
- **确定性提取**：扩展名/MIME、文件名模式（年份、剧集 SxxEyy、分辨率、HDR、语言、版本标记）、标题归一化（`docs/03` §5.4.3 第 1、4 步）。
- **Provider 抽象**：`Provider` trait + **MockProvider**（规则关键词，确定性零成本）+ OllamaProvider/HttpCloudProvider 占位（接口冻结，返回 F704）。
- **预算控制**：`ai.run` 每作业一个 Budget，模型调用次数耗尽即 F703。
- **提示词缓存**：`prompt_hash` 指纹（blake3 前缀），同指纹复用。
- **方法**：`ai.suggest.list|apply|reject|revoke`、`ai.run`、`ai.privacy.set|list`。
- **CLI**：`rmgr ai suggest list/apply/reject/revoke`、`rmgr ai run`、`rmgr ai privacy set/list`。

## 产出文件

| 路径 | 说明 |
|---|---|
| `crates/rm-ai/` | AI 领域包：extract/provider/suggest/privacy/cost/cache/codes |
| `crates/rm-catalog/src/repo/suggestion.rs` | 建议表 + 隐私档位 repo 层 |
| `crates/rm-catalog/src/migrations.rs` | SCHEMA_V5：`ai_suggestion` + `ai_privacy` 两表 |
| `crates/rm-plugin/src/methods_m5.rs` | M5 方法注册 + ai.run 后台任务 |
| `crates/rm-plugin/tests/m5_acceptance.rs` | 5 项验收（提取/mock/apply/revoke/隐私预算） |
| `crates/rm-proto/src/{lib,event,msg}.rs` | F701–F706 + AI_SUGGEST_CREATED 事件 + M5 消息 |

## 修改文件

| 路径 | 改动 |
|---|---|
| `Cargo.toml` | workspace members + `rm-ai` |
| `crates/rm-catalog/src/repo/{mod,tag,entry}.rs` | suggestion 模块；entry_tag/version_tag 按 source 撤销；version_tag_list；entry_ext_upsert |
| `crates/rm-plugin/src/{lib,methods,events}.rs` | `pub mod methods_m5` + `register_m5` + emit_ai_suggest_created |
| `apps/cli/src/main.rs` + `Cargo.toml` | ai suggest/run/privacy 子命令 |
| `docs/01-总体设计.md` | v0.15；状态 M1–M5；附录 B 第 14 条；变更记录 |
| `docs/02-数据字典.md` | v0.9；V5 schema 版本行；§5.2 新增 ai_privacy 表 |
| `docs/03-接口与协议.md` | v0.11；§2.3 补 ai.run/privacy；§2.7 F7xx；§3.1 补 ai 命令；§5 全冻结 |
| `docs/04-项目结构.md` | v0.6；rm-ai 目录/依赖；M5 偏差登记表 |
| `README.md` | 进度：M1–M5 完成；附录 B 14 条 |

## 关键决策 / 偏差登记（docs/01 附录 B #14、docs/04 §3）

1. **`ai.run` 实现新增**：03 §2.3 原表无生成入口；接口已补进 03。
2. **`ai.privacy.set/list` 实现新增**：隐私三档需持久化配置；接口已补进 03。
3. **MockProvider 替代真实模型**：环境无外网/未装 Ollama；`Provider` trait + Ollama/HttpCloud 占位接口冻结，返回 F704，环境具备后无痛替换。
4. **资料站刮削协议占位**：`entry_match` 建议种类已支持；真实 scrape-tmdb 插件待有网环境。
5. **metadata 目标的 tag 建议不自动采纳**：无 entry/version 落点，自动采纳会 F301；降级为 pending 人工兜底。
6. **F7xx 错误码族**：F7 空闲正好给 AI（F701 建议不存在 / F702 状态不允许 / F703 预算耗尽 / F704 提供方不可用 / F705 隐私禁止 / F706 目标不存在）。
7. **环回/临时目录**：无真局域网/真模型 → 用临时目录 + MockProvider；协议与代码路径一致。

## 验收结果

| 项 | 结果 |
|---|---|
| `cargo test --workspace -- --test-threads=1` | 全绿（rm-ai 8 单测 + m5_acceptance 5 + 既有全套） |
| `cargo build -p rmgr` / `--workspace` | 绿 |
| `cargo clippy` / `cargo fmt` | 环境未装组件；以 `cargo test`/`build` 为准 |
| m5 领域 | 提取（年份/剧集/分辨率/HDR/语言）、MockProvider 关键词、分级确认阈值、预算耗尽、隐私三档 |
| m5_plugin_suggest_flow | ai.run → 建议入库 + 低风险自动采纳 + list + reject pending |
| m5_plugin_apply_tag | 手动插 pending 建议 → apply → 业务表贴附（source=ai）→ 重复 apply 跳过 |
| m5_plugin_revoke | revoke source=ai → 贴附回滚 + pending→rejected |
| m5_plugin_privacy_budget | privacy set/list + 最长前缀优先 + 默认 hybrid + budget=0 → F703 |

## 复验命令

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cd resources-manager
cargo test --workspace -- --test-threads=1
cargo test -p rm-plugin --test m5_acceptance -- --test-threads=1
cargo test -p rm-ai
cargo build -p rmgr
```

## 校验和

见 [`M5-files.sha256`](M5-files.sha256)；可与 `M4-files.sha256` 比对得 M5 变更集。
