// 页面内容的承载文件。真正的页面在 page.html，构建时被转成字符串常量
// 编进这个文件，运行时不读磁盘（core-ui/02 §3.1）。
#include "ui_server.hpp"

namespace ling {

const char* page_html() {
    return
#include "page_html.inc"
        ;
}

}  // namespace ling
