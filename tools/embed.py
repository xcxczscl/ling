"""把页面文件嵌进可执行文件。

页面写在一个普通 .html 里，构建时转成一段 C++ 字符串常量。
这样发布只有一个 exe，不依赖磁盘上的前端文件（core-ui/02 §3.1）。
"""
import pathlib
import sys


def main():
    if len(sys.argv) != 3:
        print("用法：embed.py <输入.html> <输出.inc>")
        return 2
    source = pathlib.Path(sys.argv[1])
    target = pathlib.Path(sys.argv[2])
    text = source.read_text(encoding="utf-8")
    # 用不常见的分隔符，避免页面里的 ")xxx" 提前结束字符串。
    assert ')ling-page"' not in text, "页面里出现了保留分隔符"
    target.write_text('R"ling-page(\n' + text + '\n)ling-page"', encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main())
