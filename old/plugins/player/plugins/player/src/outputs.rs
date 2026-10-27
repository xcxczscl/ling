//! 本机输出设备发现与选择（`docs/01` §4.5 发现区 + §5.12 `player.outputs.*`）。
//!
//! 数据来源（§4.5「操作系统 API」）：
//! - **显示器**：`/sys/class/drm/card*-*` 连接器（status/modes）；无 DRM 连接器
//!   → 合成兜底行「本机显示」（保证 list 恒有 display 行）。
//! - **声卡**：libmpv `audio-device-list`（mpv 自己的 ao 枚举，字符串 JSON）；
//!   探测失败 → 空表（诚实降级，无 ALSA/无音频栈时本就为空）。
//! - 蓝牙设备（§4.5 提及）本机无 bluetoothctl/适配器 → 留后续，登记附录 B。
//!
//! 切换（`player.outputs.set`）：audio 类设备即时应用到全部 mpv 会话（mpv
//! `audio-device` 可运行时 set 并回读）；display 类在 headless 无窗口可迁，
//! 仅记录选择（活动标记 + 新会话继承口径见附录 B）。

use std::path::Path;

use serde_json::json;

use crate::error::PlayerError;

/// DRM sysfs 根（独立参数便于测试注入假目录）。
pub const DRM_ROOT: &str = "/sys/class/drm";

/// 输出设备行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputRow {
    pub id: String,
    /// `display` | `audio`
    pub kind: String,
    pub name: String,
    /// 显示器连接状态（`connected`/`disconnected`/`unknown`；audio 为 None）。
    pub status: Option<String>,
}

/// 当前选中的输出设备（PlayerState 持有；内存态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selected {
    pub id: String,
    pub kind: String,
}

impl OutputRow {
    pub fn to_json(&self, active: bool) -> serde_json::Value {
        json!({
            "id": self.id,
            "kind": self.kind,
            "name": self.name,
            "status": self.status,
            "active": active,
        })
    }
}

/// 解析 DRM 连接器目录（`root` = `/sys/class/drm` 形态的目录）。
pub fn drm_outputs(root: &Path) -> Vec<OutputRow> {
    let mut rows = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return fallback_display();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("card") && n.contains('-'))
        .collect();
    names.sort();
    for name in names {
        let dir = root.join(&name);
        let status = std::fs::read_to_string(dir.join("status"))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "unknown".into());
        // 连接器短名：card0-DP-1 → DP-1
        let short = name.split_once('-').map(|(_, s)| s.to_string()).unwrap_or_else(|| name.clone());
        rows.push(OutputRow {
            id: name,
            kind: "display".into(),
            name: format!("{short}（{status}）"),
            status: Some(status),
        });
    }
    if rows.is_empty() {
        fallback_display()
    } else {
        rows
    }
}

/// 无连接器兜底：恒有一条本机显示，保证 UI 与 outputs.set 有对象可选。
fn fallback_display() -> Vec<OutputRow> {
    vec![OutputRow {
        id: "local-0".into(),
        kind: "display".into(),
        name: "本机显示（未探测到 DRM 连接器）".into(),
        status: Some("unknown".into()),
    }]
}

/// 解析 mpv `audio-device-list` 字符串（JSON 数组 `[{name, description}]`）。
pub fn parse_audio_device_list(s: &str) -> Vec<OutputRow> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(s) else {
        return Vec::new();
    };
    let Some(arr) = v.as_array() else { return Vec::new() };
    arr.iter()
        .filter_map(|it| {
            let id = it["name"].as_str()?.to_string();
            let desc = it["description"].as_str().unwrap_or(&id).to_string();
            Some(OutputRow {
                id,
                kind: "audio".into(),
                name: desc,
                status: None,
            })
        })
        .collect()
}

/// 声卡枚举：临时 libmpv 实例读 `audio-device-list`（读完即弃）。
/// mpv 初始化失败 → 空表（诚实降级）。
pub fn audio_outputs() -> Vec<OutputRow> {
    let mpv = match libmpv2::Mpv::with_initializer(|init| {
        init.set_property("vo", "null")?;
        init.set_property("ao", "null")?;
        Ok(())
    }) {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    match mpv.get_property::<String>("audio-device-list") {
        Ok(s) => parse_audio_device_list(&s),
        Err(_) => Vec::new(),
    }
}

/// 全量输出设备（display + audio）。
pub fn all_outputs() -> Vec<OutputRow> {
    let mut rows = drm_outputs(Path::new(DRM_ROOT));
    rows.extend(audio_outputs());
    rows
}

/// `player.outputs.list` 响应。
pub fn list_value(selected: Option<&str>) -> serde_json::Value {
    let rows = all_outputs();
    json!({
        "rows": rows.iter().map(|r| r.to_json(r.id == selected.unwrap_or(""))).collect::<Vec<_>>(),
        "selected": selected.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null),
    })
}

/// 校验 output_id 存在（枚举是权威），否则 **P100**。
pub fn validate(output_id: &str) -> Result<OutputRow, PlayerError> {
    all_outputs()
        .into_iter()
        .find(|r| r.id == output_id)
        .ok_or_else(|| PlayerError::bad_param(format!("未知输出设备：{output_id}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t1_drm_parse_fixture() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (name, status, modes) in [
            ("card0-DP-1", "disconnected", ""),
            ("card0-HDMI-A-1", "connected", "1920x1080\n1280x720\n"),
            ("card0", "ignored", ""),   // 无连字符 → 非连接器
            ("renderD128", "ignored", ""), // 非 card 前缀
        ] {
            let d = root.join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("status"), status).unwrap();
            std::fs::write(d.join("modes"), modes).unwrap();
        }
        let rows = drm_outputs(root);
        assert_eq!(rows.len(), 2, "card0/renderD128 应被过滤：{rows:?}");
        assert_eq!(rows[0].id, "card0-DP-1");
        assert_eq!(rows[0].kind, "display");
        assert_eq!(rows[0].status.as_deref(), Some("disconnected"));
        assert_eq!(rows[1].id, "card0-HDMI-A-1");
        assert!(rows[1].name.contains("connected"));
        assert!(rows[1].name.contains("DP-1") || rows[1].name.contains("HDMI-A-1"));
    }

    #[test]
    fn t2_drm_empty_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let rows = drm_outputs(dir.path());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "local-0");
        assert_eq!(rows[0].kind, "display");
        // 目录不存在也兜底
        let rows = drm_outputs(Path::new("/nonexistent/drm-root"));
        assert_eq!(rows[0].id, "local-0");
    }

    #[test]
    fn t3_audio_parse() {
        let s = r#"[{"name":"auto","description":"Autoselect device"},{"name":"alsa","description":"Default (alsa)"}]"#;
        let rows = parse_audio_device_list(s);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "auto");
        assert_eq!(rows[0].kind, "audio");
        assert_eq!(rows[1].name, "Default (alsa)");
        // 坏输入 → 空
        assert!(parse_audio_device_list("not json").is_empty());
        assert!(parse_audio_device_list(r#"{"x":1}"#).is_empty());
        // 缺 description → 回退 id
        let rows = parse_audio_device_list(r#"[{"name":"dev1"}]"#);
        assert_eq!(rows[0].name, "dev1");
    }

    #[test]
    fn t4_audio_outputs_from_libmpv() {
        // 本环境有 libmpv：至少枚举出 auto（探测失败则空表——两种都允许，只防 panic）
        let rows = audio_outputs();
        for r in &rows {
            assert_eq!(r.kind, "audio");
            assert!(!r.id.is_empty());
        }
    }

    #[test]
    fn t5_list_and_validate_json() {
        let v = list_value(None);
        let rows = v["rows"].as_array().unwrap();
        assert!(rows.iter().any(|r| r["kind"] == "display"), "恒有 display 行");
        assert!(v["selected"].is_null());
        let first = rows[0]["id"].as_str().unwrap().to_string();
        let v2 = list_value(Some(&first));
        assert_eq!(v2["selected"], first.as_str());
        assert_eq!(v2["rows"][0]["active"], true);
        // 校验：存在 ok / 不存在 P100
        assert!(validate(&first).is_ok());
        let err = validate("no-such-output").unwrap_err();
        assert_eq!(err.code, "P100");
    }
}
