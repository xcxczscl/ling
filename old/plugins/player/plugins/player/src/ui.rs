//! 控制面/查看器网页（`docs/01` §5.7）：共享 shell（模板+样式+rpc 层）×
//! 页面主体/脚本，保证 control/viewer 及后续页面**同一套风格与网关代码**
//! （「一套代码三端复用」的静态实现，无 node/vite；附录 B #37/#41）。
//!
//! 走 core-ui `/rpc` 网关（`{plugin, method:'player.'+method, params}`）。

const SHELL_HTML: &str = include_str!("../assets/shell.html");
const SHELL_CSS: &str = include_str!("../assets/shell.css");
const SHELL_JS: &str = include_str!("../assets/shell.js");

/// 一个贡献页面：path/title/icon + 主体/脚本片段（渲染时套共享 shell）。
pub struct PageDef {
    pub path: &'static str,
    pub title: &'static str,
    pub icon: &'static str,
    pub order: u32,
    body: &'static str,
    script: &'static str,
}

impl PageDef {
    /// 渲染完整 HTML（占位符替换；CSS/JS 作为值注入，含花括号也安全）。
    pub fn html(&self) -> String {
        SHELL_HTML
            .replace("{title}", self.title)
            .replace("{style}", SHELL_CSS)
            .replace("{body}", self.body)
            .replace("{shared_js}", SHELL_JS)
            .replace("{script}", self.script)
    }
}

/// 页面清单。control=300，viewer=301（§5.7）。
pub fn pages() -> [PageDef; 2] {
    [
        PageDef {
            path: "/player/control",
            title: "播放器 · player",
            icon: "play",
            order: 300,
            body: include_str!("../assets/control.html"),
            script: include_str!("../assets/control.js"),
        },
        PageDef {
            path: "/player/viewer",
            title: "查看器 · player",
            icon: "eye",
            order: 301,
            body: include_str!("../assets/viewer.html"),
            script: include_str!("../assets/viewer.js"),
        },
    ]
}

/// 按 path 取渲染后的 HTML；未知 → None。
pub fn page_html(path: &str) -> Option<String> {
    pages().into_iter().find(|p| p.path == path).map(|p| p.html())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t1_pages_share_shell() {
        let pages = pages();
        assert_eq!(pages.len(), 2);
        let control = pages[0].html();
        let viewer = pages[1].html();
        // 同源 shell：标记、样式块、rpc 网关完全一致
        assert!(control.contains(r#"data-shell="p2""#));
        assert!(viewer.contains(r#"data-shell="p2""#));
        for h in [&control, &viewer] {
            assert!(h.starts_with("<!DOCTYPE html>"));
            assert!(h.contains("player.' + method"), "共享 rpc 网关");
            assert!(h.contains("--accent"), "共享样式注入");
            assert!(!h.contains("{style}") && !h.contains("{body}"), "占位符须全部替换");
        }
        // 页面差异化内容
        assert!(control.contains("输出设备（P2）"));
        assert!(control.contains("outputs.list"));
        assert!(viewer.contains("上一页"));
        assert!(viewer.contains("goto_page"));
        assert_eq!(pages[0].order, 300);
        assert_eq!(pages[1].order, 301);
    }

    #[test]
    fn t2_unknown_page_none() {
        assert!(page_html("/nope").is_none());
        assert!(page_html("/player/control").unwrap().contains("播放器"));
        assert!(page_html("/player/viewer").unwrap().contains("查看器"));
    }
}
