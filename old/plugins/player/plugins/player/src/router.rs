//! P4 路由策略（`docs/01` §5.3.2 自动选择 + §6.4 Router 选择面；附录 B #50/#51）：
//! mode 缺省时按「接收器宣称编解码 ↔ 内容编码 ↔ 带宽」自动选择，用户 override 直接生效。
//!
//! - A 字节转发：对端宣称支持当前内容编码（效率最高）；
//! - B 重编码：对端不支持内容编码但宣称 h264 且带宽 ≥ 20 Mbps（§5.3.2 伪代码）；
//! - C 帧流兜底：其余情况（牺牲画质保兼容）；
//! - D 屏幕镜像不参与自动路由（仅手动/电视协议 Sink，§5.3.2 注）。
//!
//! 带宽：设计允许 P4 起「可选 ping 估算 RTT」——首版仍用固定局域网估值 100 Mbps
//!（§5.3.2：不做主动带宽探测防惊群；ping 估算留后续，#52 登记）。

/// 带宽估值（§5.3.2 BandwidthEstimate）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandwidthEstimate {
    pub estimated_mbps: f64,
    /// RTT 估算（可选；首版不主动 ping，恒 None）。
    pub rtt_ms: Option<u32>,
}

impl BandwidthEstimate {
    /// 固定局域网估值（设计首版口径：默认 100 Mbps）。
    pub fn fixed() -> Self {
        Self { estimated_mbps: 100.0, rtt_ms: None }
    }
}

/// 自动选择（§5.3.2 伪代码直译）：A → B → C；D 永不自动。
/// `receiver_codecs` = 接收器心跳宣称的编解码能力（空表 = 什么都没宣称 → 不能 A/B）。
pub fn auto_select_mode(
    receiver_codecs: &[String],
    content_codec: &str,
    bandwidth: &BandwidthEstimate,
) -> &'static str {
    fn supports(codecs: &[String], want: &str) -> bool {
        codecs.iter().any(|c| c.eq_ignore_ascii_case(want))
    }
    if !content_codec.is_empty() && supports(receiver_codecs, content_codec) {
        "A"
    } else if supports(receiver_codecs, "h264") && bandwidth.estimated_mbps >= 20.0 {
        "B"
    } else {
        "C"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codecs(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn t1_auto_a_when_receiver_supports_codec() {
        let bw = BandwidthEstimate::fixed();
        // 对端宣称 hevc → HEVC 内容走 A（§5.3.2 举例：HEVC 电视）
        assert_eq!(auto_select_mode(&codecs(&["hevc", "h264", "aac"]), "hevc", &bw), "A");
        // 大小写不敏感
        assert_eq!(auto_select_mode(&codecs(&["H264"]), "h264", &bw), "A");
        // 内容编码未知/空 → 不算支持
        assert_eq!(auto_select_mode(&codecs(&["h264"]), "", &bw), "B");
    }

    #[test]
    fn t2_auto_b_when_unsupported_but_h264_and_bandwidth_ok() {
        let bw = BandwidthEstimate::fixed();
        // mpeg4 内容、对端只宣称 h264 → B（§5.3.2：重编码到 H.264）
        assert_eq!(auto_select_mode(&codecs(&["h264", "aac"]), "mpeg4", &bw), "B");
        // 带宽 < 20 Mbps → 不能 B，退 C
        let slow = BandwidthEstimate { estimated_mbps: 10.0, rtt_ms: None };
        assert_eq!(auto_select_mode(&codecs(&["h264"]), "mpeg4", &slow), "C");
    }

    #[test]
    fn t3_auto_c_fallback_never_d() {
        let bw = BandwidthEstimate::fixed();
        // 对端什么都没宣称 → C（兜底帧流）
        assert_eq!(auto_select_mode(&[], "h264", &bw), "C");
        // 对端无 h264 宣称（如只宣能力弱的设备）→ C
        assert_eq!(auto_select_mode(&codecs(&["aac"]), "hevc", &bw), "C");
        // 自动路由永不选 D（§5.3.2 注：仅手动/电视协议）
        for cs in [vec![], codecs(&["h264"])] {
            for bw in [BandwidthEstimate::fixed(), BandwidthEstimate { estimated_mbps: 1.0, rtt_ms: None }] {
                for codec in ["", "h264", "hevc"] {
                    assert_ne!(auto_select_mode(&cs, codec, &bw), "D");
                }
            }
        }
    }

    #[test]
    fn t4_bandwidth_fixed_default() {
        let bw = BandwidthEstimate::fixed();
        assert_eq!(bw.estimated_mbps, 100.0, "设计默认 100 Mbps 局域网（§5.3.2）");
        assert!(bw.rtt_ms.is_none(), "首版不主动 ping（#52）");
    }
}
