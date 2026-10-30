# 第三方依赖登记。三个都是上游单文件发行版，随仓库一起走，不在构建时下载。

## 为什么不用 vcpkg

`docs/ling/05-构造与开发指南.md` §3.3 原定用 vcpkg。本机实测有两个障碍：

1. `vcpkg.exe` 首次运行要写 `%LOCALAPPDATA%\vcpkg\config`，被沙箱拒绝；
2. vcpkg 装库需要调 MSBuild 找 Visual Studio，本机的 VS 18 与 vcpkg 的探测不保证匹配。

三个库都有官方单文件发行版，取下来随仓库走更稳，也顺带解决了「锁版本」——文件名即版本。

## 登记

| 依赖 | 版本 | 文件 | sha256 |
|---|---|---|---|
| toml++ | v3.4.0 | `tomlplusplus/toml.hpp` | `6b5172ad4dd6519aec67b919181fa7a38a2234131e5b2afa232dfe444819783e` |
| nlohmann/json | v3.11.3 | `nlohmann/json.hpp` | `9bea4c8066ef4a1c206b2be5a36302f8926f7fdc6087af5d20b417d0cf103ea6` |
| Catch2 | v3.7.1 | `catch2/catch_amalgamated.hpp`<br>`catch2/catch_amalgamated.cpp` | `91d47e2ddfb2949df22ff582d6c292554184da743e11d3811dcdd06b5b0a159b`<br>`450fc5f213a87668f0f48c91a2afa4f4226186fef453d36e6fa3bc184ccdb9ba` |

## 来源

- <https://raw.githubusercontent.com/marzer/tomlplusplus/v3.4.0/toml.hpp>
- <https://raw.githubusercontent.com/nlohmann/json/v3.11.3/single_include/nlohmann/json.hpp>
- <https://raw.githubusercontent.com/catchorg/Catch2/v3.7.1/extras/catch_amalgamated.hpp>
- <https://raw.githubusercontent.com/catchorg/Catch2/v3.7.1/extras/catch_amalgamated.cpp>

## 升级规则

升级等于同时改三处：文件名下的内容、本表版本与 sha256、`docs/ling/05-构造与开发指南.md` §3.3
的版本列。三者不一致时以本表为准，并跑一遍全量测试。

## 不修改原则

文件内容原样保留，不本地打补丁。需要改行为时在调用侧适配，否则下次升级会丢。
