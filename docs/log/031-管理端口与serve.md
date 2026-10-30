# log 031 管理端口与 serve/shutdown

> 日期：2026-10-30
> 触发：事件广播通过后继续，补齐「管理端口与命令行」组

## 一、这一段做完了什么

| 项（`04/06`） | 结果 |
|---|---|
| 只绑回环 | 固定 `127.0.0.1`，没有配置项 |
| 端口顺延 | 7900 起试到 7999，成功即写进 `registry.json` 的 `port` |
| `session.json` | 每次启动重新生成 32 字节随机口令（64 位十六进制），退出时删除 |
| 首帧认证 | 非 `auth` 帧、口令错、5 秒未认证，三种情况都断开 |
| 帧格式 | jsonl，`v`/`id`/`op`，`call` 只接受 `kernel.*` |
| 版本不匹配 | 回 `E_API_MISMATCH` 后断开 |
| 子命令 | `serve`、`shutdown`、`plugins`、`install`、`uninstall`、`enable`、`disable`、`logs`、`--version`、`--help` 共十个 |
| `logs` 两条路径 | 不带编号直读内核日志文件；带编号经 `kernel.logs` 读插件 `data/` 下最新的 `.log` |
| 退出码 | 0 成功、2 参数错、3 宿主不可达、4 宿主返回错误 |

`serve` 之外的子命令全部经端口，命令行本身不再构造 `Kernel`——这一点是 `LING-REQ-072` 的要求，也顺带解决了上一个切片留下的「`install` 之后不激活」的问题：宿主是常驻进程，装完当场装载激活。

## 二、写的时候改的两处

**① `install` 之后要立刻激活。** 上一版 `install` 是「先 `load_all`，再拷文件」，拷完进程就退出，新插件从未被装载。改为经端口下发后，`kernel.install` 自己负责「装完再 `load_one`」，返回 `{"id":"demo-a","state":"activated"}`。

**② 错误信息把错误码说了两遍。** 内核方法的返回值是 `"E_PLUGIN_MISSING nope"` 这种「码加说明」的字符串，直接当 `message` 用会输出 `E_PLUGIN_MISSING E_PLUGIN_MISSING nope`。加了一个 `fill_error` 拆成 `code` 与 `message` 两段。

## 三、取舍

**`kernel.*` 方法没有进路由表。** `04/06` §4.1 说「走同一条调用路由」，实现上内核方法是 `Kernel::handle_kernel` 里的一段分发，不经 `Router`。理由：`Router` 的 `RouteEntry` 带 `handler_ctx` 与能力位，这两个字段对内核方法都没有意义，硬塞进去要在路由表里加一堆「plugin_id 是 kernel 时跳过某些检查」的分支。对外语义仍然是「同一个入口、同一套错误码、同一个超时」，差的是内部实现。这一条与文档有偏差，实现更简单，记在这里备查；若将来要把 `kernel.*` 暴露成插件可调用的方法，就应该真的并进路由表。

**服务端是单线程 `select` 循环。** 管理面并发量是「一个命令行客户端」，多线程只会引入同步问题。轮询周期 200 毫秒，`kernel.shutdown` 之后最多 200 毫秒退出。

**`session.json` 在 Windows 上还没有收紧 ACL。** `04/06` §2.3 要求 `0600`（Windows 上是「仅当前用户」的 ACL）。当前文件落在 `%APPDATA%\ling` 下，别的账户默认进不去，但同机管理员能读。这一条记为已知不足，M6 打包前补。

## 四、可复现的验证

```
ling serve --data-dir run/tmp-serve         # 输出：已启动，管理端口 127.0.0.1:7900
ling install run/win-debug/plugins/demo-a --data-dir run/tmp-serve
ling install run/win-debug/plugins/demo-caller --data-dir run/tmp-serve
ling plugins --data-dir run/tmp-serve
ling logs demo-caller --data-dir run/tmp-serve
ling uninstall demo-a --data-dir run/tmp-serve
ling shutdown --data-dir run/tmp-serve
```

实测输出：

```
demo-a                   1.0.0      activated    process
demo-caller              1.0.0      activated    process

event demo-a.pinged pong
reply=pong
event kernel.plugin.state {"generation":2,"plugin_id":"demo-caller","state":"activated",...}
event kernel.plugin.state {"generation":1,"plugin_id":"demo-a","state":"uninstalled",...}
```

错误路径：

| 场景 | 输出 | 退出码 |
|---|---|---|
| `disable nope` | `E_PLUGIN_MISSING nope` | 4 |
| 宿主已退出 | `宿主未运行（找不到 registry.json）` | 3 |
| `session.json` 不在但端口在 | `宿主正在启动或已退出（找不到 session.json）` | 3 |
| 口令被改 | `口令不匹配，宿主已重启过` | 4 |

## 五、距 MVP 还差什么

| 能力 | 状态 |
|---|---|
| 装载与卸载、路由与事件、能力门禁、上下文与账本 | 已实现 |
| 管理端口与命令行（069 至 077） | 十个子命令可用 |
| 内置插件（078 至 083） | 未做，`serve` 目前不会自动装三个内置插件 |
| 性能与资源（089 至 099） | 未测 |
| 验收脚本 | `l1_manifest`、`l2_call`、`l1_bench_call` 未写 |

下一步：三个内置插件（core-settings、core-plugin-manager、core-log），首启自动安装。
