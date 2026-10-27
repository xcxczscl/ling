//! 画面比例（`docs/01` §5.5，P0 硬性规则）：
//! 不拉伸、不裁切、允许单/双侧黑边（letterbox/pillarbox）、**禁止四边黑边**。
//!
//! 等比缩放到最大内接矩形：`scale = min(...)`，**一次取 min，永不二次缩放**。
//! libmpv 的 `keepaspect`（默认开启）走同一规则；本模块是可断言的纯函数，
//! 供自渲染路径与单测使用。
//!
//! P0 渲染走 libmpv（不直接消费本模块）；纯函数由单测守护，P2 输出抽象
//! 接入自渲染时启用——故整模块允许 dead_code。
#![allow(dead_code)]

/// 显示矩形（屏幕坐标系，左上原点）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// 四边黑边余量（左、上、右、下，像素）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Borders {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Borders {
    /// 有黑边的边数（>0.5px 才算）。
    pub fn count(&self) -> usize {
        [self.left, self.top, self.right, self.bottom]
            .iter()
            .filter(|&&v| v > 0.5)
            .count()
    }

    /// §5.5 断言：四边同时黑 = scale 计算错误。
    pub fn has_four_edge_black(&self) -> bool {
        self.count() == 4
    }
}

/// 等比缩放到屏幕最大内接矩形（step1–3：min 一次、居中、余量即黑边）。
pub fn fit_rect(content_w: f64, content_h: f64, screen_w: f64, screen_h: f64) -> Rect {
    assert!(content_w > 0.0 && content_h > 0.0, "内容尺寸必须为正");
    assert!(screen_w > 0.0 && screen_h > 0.0, "屏幕尺寸必须为正");
    let scale = (screen_w / content_w).min(screen_h / content_h);
    let w = content_w * scale;
    let h = content_h * scale;
    Rect { x: (screen_w - w) / 2.0, y: (screen_h - h) / 2.0, w, h }
}

/// 屏幕上该矩形的四边黑边。
pub fn borders_of(rect: &Rect, screen_w: f64, screen_h: f64) -> Borders {
    Borders {
        left: rect.x,
        top: rect.y,
        right: screen_w - rect.x - rect.w,
        bottom: screen_h - rect.y - rect.h,
    }
}

/// 便捷：内容+屏幕 → (矩形, 黑边)。`has_four_edge_black()` 供断言。
pub fn fit(
    content_w: f64,
    content_h: f64,
    screen_w: f64,
    screen_h: f64,
) -> (Rect, Borders) {
    let r = fit_rect(content_w, content_h, screen_w, screen_h);
    let b = borders_of(&r, screen_w, screen_h);
    (r, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-6;

    /// §5.5 文档例：21:9（1920×823）在 4K 16:9（3840×2160）→ 仅上下黑边。
    #[test]
    fn t1_widescreen_letterbox_only_top_bottom() {
        let (r, b) = fit(1920.0, 823.0, 3840.0, 2160.0);
        assert!((r.w - 3840.0).abs() < EPS, "应铺满宽：{}", r.w);
        assert!((r.h - 1646.0).abs() < EPS, "显示高≈1646：{}", r.h);
        assert!(b.left <= EPS && b.right <= EPS, "左右无黑边");
        assert!(b.top > 0.0 && b.bottom > 0.0, "上下有黑边");
        assert!(!b.has_four_edge_black(), "禁止四边黑边");
        assert_eq!(b.count(), 2);
    }

    /// 竖屏内容在横屏 → pillarbox（仅左右黑边）。
    #[test]
    fn t2_portrait_pillarbox_only_left_right() {
        let (r, b) = fit(720.0, 1280.0, 1920.0, 1080.0);
        assert!((r.h - 1080.0).abs() < EPS, "应铺满高");
        assert!(b.top <= EPS && b.bottom <= EPS);
        assert!(b.left > 0.0 && b.right > 0.0);
        assert!(!b.has_four_edge_black());
        assert_eq!(b.count(), 2);
    }

    /// 完全同比例 → 零黑边。
    #[test]
    fn t3_same_aspect_no_borders() {
        let (r, b) = fit(1920.0, 1080.0, 1920.0, 1080.0);
        assert!((r.w - 1920.0).abs() < EPS && (r.h - 1080.0).abs() < EPS);
        assert_eq!(b.count(), 0);
        assert!(!b.has_four_edge_black());
    }

    /// 小内容必须放大铺满一维——不许「适配后四周留黑」（禁止四边黑边的核心）。
    #[test]
    fn t4_small_content_must_upscale_no_four_edges() {
        let (r, b) = fit(320.0, 240.0, 1920.0, 1080.0);
        // scale = min(1920/320, 1080/240) = min(6, 4.5) = 4.5 → 1440×1080
        assert!((r.h - 1080.0).abs() < EPS, "高必须铺满");
        assert!((r.w - 1440.0).abs() < EPS);
        assert!(!b.has_four_edge_black(), "小内容四边黑边 = 违规");
        assert_eq!(b.count(), 2, "仅左右黑边");
    }

    /// 反例守卫：二次缩放会产生四边黑边——fit_rect 永不走那条路。
    #[test]
    fn t5_single_min_never_double_scale() {
        // 错误做法演示：先按宽、再按高各缩一次 → 四边黑。确保我们的路径不这样。
        let (cw, ch, sw, sh): (f64, f64, f64, f64) = (1000.0, 500.0, 1920.0, 1080.0);
        // 错误：scale_by_w 再 scale_by_h
        let wrong_scale = (sw / cw).min(sh / ch) * 0.9; // 故意二次约束
        let wrong_w = cw * wrong_scale;
        let wrong_h = ch * wrong_scale;
        assert!(wrong_w < sw && wrong_h < sh, "二次缩放 → 四边黑（反例成立）");
        // 正确路径
        let (r, b) = fit(cw, ch, sw, sh);
        assert!((r.w - sw).abs() < EPS || (r.h - sh).abs() < EPS, "正确路径必铺满一维");
        assert!(!b.has_four_edge_black());
    }
}
