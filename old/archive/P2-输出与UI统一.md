# P2 — 输出与 UI 统一（player）

| 项 | 值 |
|---|---|
| 里程碑 | P2（`player/docs/01` §9） |
| 日期 | 2026-10-17 |
| 前置 | P1 完成（asset 分流/图片/PDF/轨枚举/双 UI 页）、设计 00/01 v0.4 封档态 |
| 状态 | **完成**（编码 + 验收 + 归档）；下接 P3（串流输出 v1：字节转发（本地代理）+ 精简接收器） |

## 范围（对照 `docs/01` §9 P2）

- **输出设备发现（§4.5 落地）**：`src/outputs.rs`——显示器读 `/sys/class/drm/card*-*` 连接器 status（真实 sysfs；无连接器/无 DRM → 合成 `local-0` 兜底行，list 恒有 display 行）；声卡读 libmpv `audio-device-list`（mpv ao 枚举，临时 libmpv 实例取字符串 JSON 解析，初始化失败 → 空表）；条目 `{id, kind: display|audio, name, description, status}`。蓝牙无适配器/bluetoothctl → 留后续（#38）。
- **`player.outputs.list`**：`{rows, selected}`——`selected` 为实装增补（原表仅 `{rows[]}`）；选中行 `active=true`，初始 `selected=null`。
- **`player.outputs.set`（新增，#39）**：`{output_id}` → `{output_id, kind, applied}`；对照枚举校验（未知/缺参 → **P100**）后记 `PlayerState.selected_output`（内存态，P6 随设置持久化）。audio 类即时应用到全部 mpv 会话（backend `set_audio_device`：mpv `audio-device` set + String 回读校验，失败 P100；单会话失败不回滚），`applied`=实际应用会话数，新会话创建时继承（play_mpv 尾部）；display 类 headless 无窗口可迁 → 仅记录选择（`applied=0`），真实迁窗留有窗口环境。
- **UI 三端一致（§5.7，#41）**：共享 shell 模板——`shell.html`（占位符 `{title}{style}{body}{shared_js}{script}`，`ui.rs` 用 `.replace()` 组装避开 `format!` 花括号转义）+ `shell.css`（control/viewer 全部样式合并）+ `shell.js`（`SHELL='p2'` 标记 + `rpc/msg/fmt` 单源网关）；页面退化为 body/script 片段（`control.html/js`、`viewer.html/js`），`ui.rs` 重构为 `PageDef{path,title,icon,order,body,script}` + `page_html(path)`；控制页新增输出设备卡（`[显示]/[音频]` 文本前缀、active 高亮、点击 `outputs.set` 后刷新）。「三端一致」口径 = 同一套服务端渲染页面在浏览器/壳层 WebView/远程访问同源复用；React+Vite 栈与 #27 静态 HTML 决策一致留壳层就绪后演进。
- **Sink trait 缓引（#40）**：P2 不落空 trait——本机面仍由 mpv 直渲，frame 写入面服务网络 Sink（P3/P4 才有消费者）；输出抽象先落地为设备模型/选择态/应用通道（`outputs.rs`）。
- **单会话 UI 遗留**：无（P0 解包 bug 与字幕下拉已在 P1 修入）。

## 产出文件

| 路径 | 说明 |
|---|---|
| `player/plugins/player/src/outputs.rs` | `OutputRow/Selected`、`drm_outputs`（sysfs 解析 + 兜底）、`parse_audio_device_list`、`audio_outputs`（libmpv 枚举）、`list_value/validate` + 5 单测（drm 夹具/兜底/解析/枚举防 panic/list+validate JSON） |
| `player/plugins/player/assets/shell.html` | 共享页面壳（占位符模板） |
| `player/plugins/player/assets/shell.css` | 合并样式单源（含 `.out-list`/`.active`/`select` 等） |
| `player/plugins/player/assets/shell.js` | 共享 rpc 网关（core-ui `/rpc`，`player.` 前缀）+ `msg/fmt` + `data-shell` 标记 |
| `player/plugins/player/assets/control.html` | 控制页 body 片段（原 player.html 主体 + 输出设备卡，`data-shell="p2"`） |
| `player/plugins/player/assets/control.js` | 控制页脚本（播放控制/进度/音量/字幕轮询 + `loadOutputs` 输出卡） |
| `player/plugins/player/assets/viewer.js` | 查看器页脚本（打开/取帧/翻页/停止，rpc 改走共享网关） |
| `player/plugins/player/tests/p2_acceptance.rs` | P2 e2e 3 项（list 恒定字段 / set 校验+切换+在播应用 / 两页 shell 同源断言） |

## 修改文件

| 路径 | 变化 | 摘要 |
|---|---|---|
| `player/plugins/player/assets/player.html` | **删除** | 主体拆分为 shell + control/viewer 片段 |
| `player/plugins/player/assets/viewer.html` | — | 整页（内联 style + script）改为 body 片段，样式/rpc 归并共享 shell |
| `player/plugins/player/src/ui.rs` | — | 重写：`PageDef` 模板渲染 + `pages()/page_html()`，2 单测（页面同源/未知页 None） |
| `player/plugins/player/src/lib.rs` | — | `pub mod outputs`、`PlayerState.selected_output`（Mutex）+ `selected/selected_id`、注册 `player.outputs.list/set`、play_mpv 继承所选音频设备、`contribute_ui_pages` 改 PageDef 迭代 |
| `player/plugins/player/src/session.rs` | — | `is_viewer()`、`apply_audio_device(device)`（非 viewer 会话 set+回读） |
| `player/plugins/player/src/backend.rs` | — | `set_audio_device`（set_property + String 回读校验，失败 P100） |
| `player/docs/01-总体设计.md` | v0.4 → **v0.5** | §5.12 outputs.list 补 `selected` + 新增 outputs.set 行、P2 实装备注、§9 P2 完成、附录 B **#38–#41**、变更记录 |
| `player/docs/00-总览与术语.md` | v0.4 → **v0.5** | 状态 P0–P2、编码行、运行环境行（DRM/libmpv audio 枚举来源与缺失事实）、变更记录 |
| `player/README.md` | — | 状态 bullet 增 P2、里程碑表 P2 ✅、运行环境行、下接 P3 |
| `workspace README.md` | — | player 行 P0–P2 + 新增「player P2 完成」小节 |
| `archive/README.md` | — | 索引增 P2 行、队列清空（下接 P3） |

## 关键决策与登记（附录 B）

- **#38 输出枚举定源**：显示器 = DRM sysfs（无 X11/Wayland 环境的唯一真实来源），声卡 = libmpv `audio-device-list`（容器无 `/proc/asound`、`/dev/snd`）；均带降级/兜底；蓝牙留后续。
- **#39 `outputs.set` 新方法**：设计表只有 list 但验收要「能切换」→ 增方法登记；切换语义（audio 即时/继承、display 仅记录、`applied` 计数、内存态选择）与 list 的 `selected` 增补一并登记。
- **#40 Sink trait 缓引**：避免无消费者空 trait；写帧面随 P3/P4 网络 Sink 落类型面。
- **#41 UI 统一实现口径**：共享 shell 模板（无 node/vite 环境的「一套代码三端复用」静态实现），e2 断言两页样式块逐字一致；React 栈留壳层就绪后演进（与 #27 一致）。

## 验收结果与复验命令

- **p2_acceptance 3/3 绿**（e1 list 恒有 display+audio 行与字段；e2 set 未知/缺参 P100 + 音频切换 applied=1 且在播会话继续推进 + 显示切换记录且不打断播放 + selected/active 回显；e3 两页 DOCTYPE/共享 rpc/样式块逐字一致/页面差异内容/未知页 P100）。
- **库内单测 33/33 绿**（P1 的 26 + outputs 5 + ui 2）；**p0 4/4、p1 5/5 回归绿**；**rm 83、ling 45 全量回归绿**；`cargo build` 0 警告。

```sh
cd player && cargo build --workspace
cargo test -p player --lib
cargo test -p player --test p0_acceptance -- --test-threads=1
cargo test -p player --test p1_acceptance -- --test-threads=1
cargo test -p player --test p2_acceptance -- --test-threads=1
cd ../resources-manager && cargo test --workspace -- --test-threads=1
cd ../ling && cargo test --workspace -- --test-threads=1
```

## 校验和

归档时全量 sha256 清单：[`P2-files.sha256`](P2-files.sha256)（排除 `*/target/*`、`archive/` 自身、`*/dist/*`、core dumps、`catalog.db`）。与 `P1-files.sha256`（218 项）比对可得 P2 变更集（新增 outputs/shell/control 系与 p2_acceptance、删除 player.html、上述修改文件及两份 README/文档）。
