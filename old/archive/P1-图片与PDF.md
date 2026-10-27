# P1 — 图片与 PDF（player）

| 项 | 值 |
|---|---|
| 里程碑 | P1（`player/docs/01` §9） |
| 日期 | 2026-10-17 |
| 前置 | P0 完成（libmpv 管道 + 控制面 + 流契约）、设计 00/01 v0.3 封档态 |
| 状态 | **完成**（编码 + 验收 + 归档）；下接 P2（输出抽象 + 本机设备枚举；UI 风格统一） |

## 范围（对照 `docs/01` §9 P1）

- **分流（§4.2 落地）**：`player.play` 按 rm browse 行 `asset_type` 路由——video/audio → libmpv（P0 路径不变）；image → 图片渲染；doc（仅 `.pdf`）→ PDF 渲染；app/game/other/doc 非 PDF → **P103** 拒绝且不开流不留副作用。placement_id 快捷入参改为同扫 browse 行取 `asset_type/rel_path`（未命中 P101，错误码不变）。
- **图片渲染（Capability）**：`image` crate 解码任意支持格式 → RGBA 帧常驻会话；`player.frame` 输出 PNG base64（长边 >2048 等比缩小），UI `<img>` 展示。
- **PDF 渲染（Capability）**：`pdfinfo` 计页 + `pdftoppm -singlefile -r 96` 渲染单页（poppler 子进程，不链接 AGPL/GPL 库；源字节落临时文件）；打开即渲染第 1 页；`player.goto_page {session_id, page}`（1-based）翻页重渲染。
- **查看器会话（Session）**：`Session` 增 `viewer/asset_type/tracks` 字段与 `new_viewer` 构造；音视频控制方法（pause/resume/seek/set_volume/set_subtitle）对查看器 → **P103**（守卫先于 phase 门禁，错误稳定）；`goto_page`/`frame` 仅查看器（mpv 会话 → P103）；越界/零页 → **P100**，渲染/解码失败 → **P104**。
- **字幕轨（§5.10/§5.14）**：`ffprobe -show_streams` 枚举（libmpv2 无 node 访问，`track-list` 读不出）→ `status` 出 `audio_tracks/subtitle_tracks`（`{id, lang, title}`，按类型 1-based——实证 vid=1 与 sid=1 并存）；`player.set_subtitle {session_id, track}`（-1=关，`sid=no` 后属性不可读即成功；set 后回读校验、不匹配 **P100**）；ffprobe 缺失/超时 → 降级空表不阻塞。
- **status/事件扩展（§5.12）**：会话行增 `asset_type/kind/width/height/page/pages/audio_tracks/subtitle_tracks/subtitle_track`；`playback.started` 增 `asset_type`；play 响应按类型附 `width/height` 或 `page/pages`。
- **UI**：新增 `/player/viewer` 查看器页（icon eye、order 301，core-ui 贡献 + `player.ui.page` 直出）；控制页加字幕轨下拉（`set_subtitle`，空轨禁用）并修 P0 遗留 bug——单会话 status 是 `{sessions:[…]}` 包装但页面直读 `st.phase`（恒 undefined），改取 `sessions[0]`。
- **能力声明**：`plugin.toml` `capabilities = ["network", "shell"]`（shell 供 pdftoppm/pdfinfo/ffprobe 子进程，P4 还将用于 ffmpeg）。

## 产出文件

| 路径 | 说明 |
|---|---|
| `player/plugins/player/src/asset.rs` | 资产分流 `route()`（video/audio/image/doc-PDF + P103 拒绝面）+ PDF 魔数 `is_pdf` + 3 单测 |
| `player/plugins/player/src/shcmd.rs` | 带超时的子进程执行（spawn/轮询杀进程/stderr 尾部入错），2 单测 |
| `player/plugins/player/src/image_cap.rs` | `ImageFrame`（解码/2048 缩边/PNG base64/FNV 校验和/尺寸-only Debug）+ 4 单测 |
| `player/plugins/player/src/pdf_cap.rs` | `count_pages`/`render_page`（poppler 子进程 + 临时文件）+ **`fixture_pdf`**（手写 xref 夹具，unit/e2e 共用）+ 3 单测 |
| `player/plugins/player/src/tracks.rs` | ffprobe 轨枚举 `probe/parse`（id=按类型 1-based、降级 None）+ 3 单测 |
| `player/plugins/player/assets/viewer.html` | 查看器页（打开/取帧/翻页/停止/元信息轮询） |
| `player/plugins/player/tests/p1_acceptance.rs` | P1 e2e 5 项（夹具：ffmpeg PNG、手写 2 页 PDF、双 srt mkv、txt/xyz/exe 拒绝面） |

## 修改文件

| 路径 | 变化 | 摘要 |
|---|---|---|
| `player/plugins/player/Cargo.toml` | — | deps 增 image/base64/reqwest(blocking 无 TLS)/tempfile（dev 晋升）；`crate-type` 增 `rlib`（e2e 复用 `fixture_pdf` 等） |
| `player/plugins/player/plugin.toml` | — | `capabilities = ["network", "shell"]`（附录 B #28） |
| `player/plugins/player/src/lib.rs` | — | play 按 route 分流（play_mpv/play_image/play_pdf + fetch_bytes 403→P102）、`resolve_target`/`scan_browse`/`target_from_row`、goto_page/frame/set_subtitle 注册、contribute 带 order、模块声明 |
| `player/plugins/player/src/session.rs` | — | `ViewerState`、viewer/tracks 字段与构造器、查看器守卫（ensure_player/ensure_viewer）、goto_page/frame/set_subtitle、status 扩展、单测 3→6（新 t4 查看器守卫/t5 frame 门禁/t6 PDF 翻页） |
| `player/plugins/player/src/backend.rs` | — | `set_subtitle_track`（回读校验）/`subtitle_track`/`new_headless_for_test` |
| `player/plugins/player/src/ui.rs` | — | 页面清单 1→2 页并带 order（control 300 / viewer 301） |
| `player/plugins/player/assets/player.html` | — | 字幕轨下拉 + `fillSubs` + 单会话 status 解包修正（P0 遗留 `st.phase` 恒 undefined）+ viewer 会话提示 |
| `player/docs/01-总体设计.md` | v0.3 → **v0.4** | 状态头；§5.10 轨形状示例改写（idx→id/title）；§5.12 方法表增 goto_page/frame + set_subtitle 响应 + P1 实装备注；§7.2 capabilities 行；§9 P1 行完成备注；附录 B **#28–#37**；变更记录 |
| `player/docs/00-总览与术语.md` | v0.3 → **v0.4** | 状态头、§5 编码行（P1 完成）、运行环境行（ffmpeg/poppler/ffprobe）、推进序；变更记录 |
| `player/README.md` | — | 当前状态（P1 完成 + 环境说明）、里程碑表 P1 ✅ |
| `README.md`（工作区根） | — | player 行 → P0+P1；新增「player P1 完成」结论段 |

## 关键决策与变更登记（01 附录 B 行号）

1. **#28 PDF 选型**：pdftoppm/pdfinfo 子进程（系统工具调用不构成链接，避 mupdf AGPL/poppler GPL 传染）；副作用 = `shell` 能力 + 环境需 poppler-utils；pdftoppm 无 stdin → 临时文件。
2. **#29 图片与显示通道**：`image` crate（MPL-2.0）；图片/PDF 统一走 `player.frame` PNG base64 → UI `<img>`（mpv 仍自渲直出）；2048 长边缩边；真正 Sink 统一留 P2。
3. **#30 取字节**：reqwest blocking（无 TLS，仅 http 回环）；403→P102、其余→P101。
4. **#31 轨枚举与 sid 契约**：libmpv2 `GetData` 仅标量、无 node → ffprobe（§5.10 本列 FFmpeg 来源）；sid **按类型 1-based**（实证 vid=1 与 sid=1 并存）、`sid=no` 属性不可读 = 关闭态；set 后回读、不匹配 P100；缺 ffprobe 降级空表；轨形状改 `{id, lang, title}`。
5. **#32 新方法**：`goto_page`/`frame` 入 §5.12；`set_subtitle` 响应 `{subtitle_track}`。
6. **#33 status/事件扩展**：增量字段，P0 消费方不破坏。
7. **#34/#35 守卫与 P103 面**：查看器控制→P103、viewer 方法→P103；doc 非 PDF/app（P5 前）/other→P103；P100/P104 分域明确。
8. **#36 placement 扫 browse**：分流所需 `asset_type/rel_path` 来源；未命中前置 P101（原 rm F501 兜底，码不变）。
9. **#37 UI 两页**：viewer 页新增 + 控制页 status 解包 bug 修复 + 字幕下拉；页面风格统一归 P2。
10. **夹具工程**：PDF 手写 xref（`fixture_pdf` 置于 pdf_cap，rlib 链接供 e2e 复用，不引 pdf-writer 依赖）；PNG/双字幕 mkv 由 ffmpeg 生成（srt 放扫描目录外防入库）。

## 验收结果

| 套件 | 结果 | 说明 |
|---|---|---|
| `player` 库内单测 | **26/26**（1.2s） | aspect 5 + asset 3 + image 4 + pdf 3 + session 6 + shcmd 2 + tracks 3 |
| `p0_acceptance`（回归） | **4/4** | P0 路径不破坏（分流入参改造后仍绿） |
| `p1_acceptance` | **5/5**（22.7s，`--test-threads=1`） | e1 图片帧红像素+控制 P103+stop P401；e2 PDF 翻页帧变化/复原/越界 P100/控制 P103；e3 双字幕轨枚举+选/关/无效轨 P100+viewer 方法 P103；e4 三类 P103 拒绝且不建会话；e5 双 UI 页直出 |
| rm 全量回归 | **83/83** | `cargo test --workspace -- --test-threads=1` |
| ling 全量回归 | **45/45** | 同上（先 `cargo build --workspace`） |

**复验命令**：

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cd /root/workspace/player && cargo build --workspace && \
  cargo test -p player --lib && \
  cargo test -p player --test p0_acceptance -- --test-threads=1 && \
  cargo test -p player --test p1_acceptance -- --test-threads=1
cd /root/workspace/resources-manager && cargo build --workspace && \
  cargo test --workspace -- --test-threads=1
cd /root/workspace/ling && cargo build --workspace && \
  cargo test --workspace -- --test-threads=1
```

环境依赖：ffmpeg（e2e 夹具）、poppler-utils（pdftoppm/pdfinfo，PDF 渲染必需）、ffprobe（轨枚举，缺失降级空表）。

## 校验和

全量 sha256 清单见 `P1-files.sha256`（218 项；排除 `*/target/`、`archive/` 自身、`*/dist/`、根 `catalog.db`、core dumps）。与 `P0-files.sha256`（211 项）比对：新增 7 个文件（asset/shcmd/image_cap/pdf_cap/tracks 模块、viewer.html、p1_acceptance.rs），其余为本里程碑内容变更；`sha256sum -cs archive/P1-files.sha256` 验证通过。
