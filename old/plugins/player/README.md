# player — 内容执行器插件

**本质是一个执行器**：把 resources-manager 登记的内容"用起来"。

串流播放只是其中一个模块，不是全部——每个能力都是本插件内的一个独立模块。

## 当前状态

- **设计已封档（终审通过）**：[docs/player/00-总览与术语.md](../../docs/player/00-总览与术语.md) + [docs/player/01-总体设计.md](../../docs/player/01-总体设计.md)（v0.10）。
- **P0（本地最小播放）完成**（2026-10-17）：libmpv 管道 + 会话状态机 + 控制页 + P 族错误；`p0_acceptance` 4 项 + 单测 8 项全绿。
- **P1（图片查看 + PDF 只读渲染）完成**（2026-10-17）：图片/PDF 查看器会话 + `goto_page`/`frame`/`set_subtitle` + ffprobe 轨枚举 + 查看器页与控制页字幕下拉；`p1_acceptance` 5 项 + 单测 26 项全绿。
- **P2（输出设备枚举/切换 + UI 统一）完成**（2026-10-17）：DRM sysfs 显示器 + libmpv `audio-device-list` 声卡 + 共享 shell 模板 + 输出设备卡；`p2_acceptance` 3 项 + 单测 33 项全绿。
- **P3（串流输出 v1：模式 A + lite 接收器 + 局域网发现）完成**（2026-10-17）：7914 顺延代理、128-bit token、Range 透传 + UDP 心跳发现 + `player.cast` + `player-lite` bin；`p3_acceptance` 3 项 + 单测 49 项全绿。
- **P4（串流输出 v2：自动/手动路由 + 重编码模式 B/C + 电视协议占位）完成**（2026-10-17）：自动选择 A/B/C 路由 + ffmpeg 分块转码 + tv.rs 电视协议占位；`p4_acceptance` 3 项 + 单测 58 项全绿。
- **P5（沙箱运行）完成**（2026-10-18）：Landlock + seccomp + 退出即杀 + `player.run`/`run.list`/`run.stop`；`p5_acceptance` e1–e3 + 单测 65 项全绿。
- **P6（播放会话：本地持久化）完成**（2026-10-18）：player.db WAL + 写穿钩子 + 跨重启续播；`p6_acceptance` e1–e3 + 单测 71 项全绿。
- **P7（远程执行 + 跨设备接力）完成**（2026-10-18）：HTTP 控制平面 7917 + `player.remote.*` 7 方法 + 会话同步 LWW + stdin JSONL 输入；`p7_acceptance` e1–e3 + 单测 82 项全绿。**P0–P7 全部收官**。
- **运行环境**：ffmpeg（P4 起为模式 B/C 重编码运行时依赖，需含 libx264/mjpeg 编码器）、poppler-utils（`pdftoppm`/`pdfinfo`）、libmpv 动态库（已随 run/mpv-sdk 打包）。

## 文档

| 顺序 | 文档 | 回答什么 |
|---|---|---|
| ① | [../../docs/player/00-总览与术语.md](../../docs/player/00-总览与术语.md) | 名词解释、贯穿示例、当前状态 |
| ② | [../../docs/player/01-总体设计.md](../../docs/player/01-总体设计.md) | **唯一权威**：五区架构、串流四模式、错误码 P 族、方法/事件、沙箱、远程执行、里程碑 P0–P7 |

## 与资源管理插件的分工

- [resources-manager](../resources-manager/README.md) 只做**数据面**：交付逻辑字节流（`stream.open`）或（仅运行场景）真实路径（`get_local_path`）。契约见 docs/resources-manager/03 §2.6。
- 本插件是**消费方**，跑在 [ling 宿主](../../main/README.md) 上。运行 exe/apk = 执行任意代码，绝不能与全盘读写的资源管理插件同进程——崩溃、安全问题都隔离在自己这边。

## 架构速览

```
Source（传输区）→ Capability（功能区）→ Sink（输出区）
        ↑ 横切：Session（会话区）· Discovery（发现区）
```

- **Source**：向 rm 要流 URL，HTTP Range 拿字节
- **Capability**：音视频解码 / 图片渲染 / PDF / 沙箱运行
- **Sink**：本机屏幕 / 网络推流（模式 A/B/C/D）/ 电视协议（留接口）
- **Session**：播放进度/音量/字幕本地持久化（player.db，P6），跨设备接力（player 自建同步，不改 rm）
- **Discovery**：本机输出设备 + 局域网接收器（读 rm peer 表，不写 rm）

详见 docs/player/01 §4。

## 里程碑（P0–P7）

| 里程碑 | 内容 |
|---|---|
| **P0** ✅ | 本地最小播放（stream.open → 解码 → 本机屏幕）**（2026-10-17 完成）** |
| **P1** ✅ | 图片查看 + PDF 只读渲染 **（2026-10-17 完成）** |
| **P2** ✅ | 输出抽象 + 本机设备枚举；UI 风格统一 **（2026-10-17 完成）** |
| **P3** ✅ | 串流输出 v1：字节转发模式 A + 精简接收器 + 局域网发现 **（2026-10-17 完成）** |
| **P4** ✅ | 串流输出 v2：重编码模式 B/C + 自动/手动路由 + 电视协议 Sink 占位 **（2026-10-17 完成）** |
| **P5** ✅ | 沙箱运行：本机执行 exe/apk（Landlock+seccomp，退出即杀）**（2026-10-18 完成）** |
| **P6** ✅ | 播放会话：本地持久化（player.db，跨重启续播进度/音量/字幕）**（2026-10-18 完成）** |
| **P7** ✅ | 远程执行 + 跨设备接力（控制平面 7917、`remote.*` 方法面、会话同步 LWW 接力、stdin JSONL 输入）**（2026-10-18 完成）** |

详见 docs/player/01 §9。

## 共用内容

见 [`../../docs/common/`](../../docs/common/)：文档编写规范、公共术语。
