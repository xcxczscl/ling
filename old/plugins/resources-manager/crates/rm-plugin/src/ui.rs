//! 静态页面：browse / dup / entries / devices（M1+M2）。
//!
//! 环境无 node/vite → 不用 vite 打包，直接 `include_str!` 静态 HTML；
//! 贡献给 core-ui（`core-ui.ui.contribute`）或经 `resources-manager.ui.page` 直出。
//! 变更登记：`docs/01` 附录 B。

pub const PAGE_BROWSE: &str = include_str!("../assets/browse.html");
pub const PAGE_DUP: &str = include_str!("../assets/dup.html");
pub const PAGE_ENTRIES: &str = include_str!("../assets/entries.html");
pub const PAGE_DEVICES: &str = include_str!("../assets/devices.html");

/// 页面清单：(path, title, icon, html)。
pub fn pages() -> [(&'static str, &'static str, &'static str, &'static str); 4] {
    [
        ("/resources/browse", "资源浏览", "folder", PAGE_BROWSE),
        ("/resources/dup", "重复内容", "copy", PAGE_DUP),
        ("/resources/entries", "词条", "book", PAGE_ENTRIES),
        ("/resources/devices", "设备", "share", PAGE_DEVICES),
    ]
}

/// 按 path 取 HTML；未知 path → None。
pub fn page_html(path: &str) -> Option<&'static str> {
    pages()
        .into_iter()
        .find(|(p, ..)| *p == path)
        .map(|(.., html)| html)
}
