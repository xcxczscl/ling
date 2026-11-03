"""把外壳的前端文件嵌进可执行文件。

页面写成一堆普通文件（shell.html / shell.css / shell.js …），构建时转成一段
C++ 常量表。这样发布只有一个 exe，不依赖磁盘上的前端文件
（docs/ling/builtin-plugins/core-ui/03-架构设计.md §3.1）。

用法：
    embed.py <输出.inc> <键>=<文件>[,<文件>...] ...

键就是服务端取它时用的名字（`shell.html`、`/views/plugins.js` 之类）；
一个键给多个文件时按顺序拼起来，中间插一行分隔注释——三个脚本因此可以分开写、
审代码时各看各的，发出去仍是同一个 `/shell.js`。
"""
import pathlib
import sys

# 页面里不该出现的分隔符：出现了说明前端代码写错了，早失败比生成坏文件好。
DELIMITER = "ling-asset"


def read(path: pathlib.Path) -> str:
    text = path.read_text(encoding="utf-8")
    assert DELIMITER not in text, f"{path} 里出现了保留分隔符 {DELIMITER}"
    return text


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    target = pathlib.Path(sys.argv[1])
    entries = []
    for spec in sys.argv[2:]:
        key, _, raw = spec.partition("=")
        if not key or not raw:
            print(f"参数写法不对：{spec}")
            return 2
        pieces = []
        files = raw.split(",")
        for item in files:
            path = pathlib.Path(item)
            # 只有拼接多个文件时才插分隔注释：单文件的键插了会跑到文件最前面，
            # html 里那就是一段正文，`<meta charset>` 会被挤进 <body>。
            if len(files) > 1:
                pieces.append(f"/* ---- {path.name} ---- */\n" + read(path))
            else:
                pieces.append(read(path))
        entries.append((key, "\n".join(pieces)))

    lines = ["// 由 tools/embed.py 生成，不要手改。", "",
             "struct EmbeddedAsset {", "    const char* name;", "    const char* text;",
             "};", "", "static const EmbeddedAsset kEmbeddedAssets[] = {"]
    for key, text in entries:
        lines.append(f'    {{"{key}", R"{DELIMITER}(')
        lines.append(text)
        lines.append(f'){DELIMITER}"}},')
    lines.append("};")
    lines.append("")
    lines.append("static const unsigned kEmbeddedAssetCount = " + str(len(entries)) + ";")
    target.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main())
