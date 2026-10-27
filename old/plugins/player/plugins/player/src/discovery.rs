//! P3 局域网接收器发现（`docs/01` §4.5/§5.13；附录 B #43/#44）：lite 主动 UDP 心跳、
//! 发送端被动监听——**不扫描局域网**（§4.5 硬规则），也不依赖 rm peer（风险 #6）。
//!
//! - 心跳目的地：`127.0.0.1:7916`（同机/可靠）+ `255.255.255.255:7916`（局域网宣告）；
//!   lite 可用 `--hb` 显式指定（e2e/同机多实例：7916 被占时监听方退临时端口，
//!   实际端口经 `player.receiver.list` 的 `discovery_port` 外发）；
//! - 负载：`{"id","name","port","state","codecs"?}`；接收端以报文源 IP 为地址；
//!   `codecs` = 接收器宣称的编解码能力（P4 自动路由输入，附录 B #50；缺省空表）；
//! - TTL 3 秒、心跳 1 秒；**每次装载一个监听**（ling `lib_seq` 每次 boot 复制 .so
//!   独立 dlopen → 静态表不跨实例共享，7916 被先到实例占用即退临时端口）。

use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// 心跳监听首选端口（UDP；与 TCP 代理池互不冲突）。
pub const DISCOVERY_PORT: u16 = 7916;
/// 心跳有效期：3 次心跳未到即过期。
pub const TTL_MS: u64 = 3000;

/// 已知接收器（进程级表；内存态）。
#[derive(Clone, Debug, PartialEq)]
pub struct ReceiverInfo {
    pub id: String,
    pub name: String,
    pub ip: String,
    pub port: u16,
    pub state: String,
    /// 宣称支持的编解码（小写 codec 名；空 = 未宣称）。
    pub codecs: Vec<String>,
    pub last_seen_ms: u64,
}

static STORE: OnceLock<Mutex<HashMap<String, ReceiverInfo>>> = OnceLock::new();
static START: OnceLock<()> = OnceLock::new();
/// 实际监听端口：7916（首选成功）/ 临时端口（被占退让）/ 0（绑失败，空表）。
static LISTEN_PORT: AtomicU16 = AtomicU16::new(0);

fn store() -> &'static Mutex<HashMap<String, ReceiverInfo>> {
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 实际发现端口（0 = 未起/绑失败）。
pub fn listen_port() -> u16 {
    LISTEN_PORT.load(Ordering::SeqCst)
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 解析后的心跳（坏包 → None）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heartbeat {
    pub id: String,
    pub name: String,
    pub port: u16,
    pub state: String,
    pub codecs: Vec<String>,
}

/// 解析心跳负载（坏包 → None；缺 codecs → 空表）。
pub fn parse_heartbeat(buf: &[u8]) -> Option<Heartbeat> {
    let v: serde_json::Value = serde_json::from_slice(buf).ok()?;
    let id = v["id"].as_str()?.to_string();
    if id.is_empty() {
        return None;
    }
    let name = v["name"].as_str().unwrap_or(&id).to_string();
    let port = v["port"].as_u64()? as u16;
    let state = v["state"].as_str().unwrap_or("idle").to_string();
    let codecs = v["codecs"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .map(|s| s.to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    Some(Heartbeat { id, name, port, state, codecs })
}

/// 收包入表（ip 来自报文源地址；now 注入供单测）。
fn upsert(map: &mut HashMap<String, ReceiverInfo>, hb: Heartbeat, ip: &str, now: u64) {
    map.insert(
        hb.id.clone(),
        ReceiverInfo {
            id: hb.id,
            name: hb.name,
            ip: ip.to_string(),
            port: hb.port,
            state: hb.state,
            codecs: hb.codecs,
            last_seen_ms: now,
        },
    );
}

/// 扫过期（now 注入供单测）。
fn sweep(map: &mut HashMap<String, ReceiverInfo>, now: u64) {
    map.retain(|_, r| now.saturating_sub(r.last_seen_ms) <= TTL_MS);
}

/// 确保监听已起（每次装载一次；7916 被占 → 临时端口，均失败 → 0=空表，不 panic）。
pub fn ensure_listener() {
    if START.get().is_some() {
        return;
    }
    // 首选 7916；同进程可能有早前装载实例的泄漏监听占着 → 退 OS 分配端口。
    let bound = UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT))
        .or_else(|_| UdpSocket::bind(("0.0.0.0", 0)));
    let sock = match bound {
        Ok(s) => s,
        Err(_) => {
            let _ = START.set(());
            return;
        }
    };
    LISTEN_PORT.store(
        sock.local_addr().map(|a| a.port()).unwrap_or(0),
        Ordering::SeqCst,
    );
    let _ = sock.set_read_timeout(Some(std::time::Duration::from_millis(500)));
    std::thread::Builder::new()
        .name("player-discovery".into())
        .spawn(move || {
            let mut buf = [0u8; 2048];
            loop {
                match sock.recv_from(&mut buf) {
                    Ok((n, from)) => {
                        let Some(hb) = parse_heartbeat(&buf[..n]) else { continue };
                        let now = now_ms();
                        let mut map = store().lock().unwrap();
                        upsert(&mut map, hb, &from.ip().to_string(), now);
                    }
                    // 读超时（500ms 轮转）与套接字软错误：继续收；硬失效也留待下轮。
                    Err(_) => continue,
                }
            }
        })
        .ok();
    let _ = START.set(());
}

/// 接收器列表（扫过期；行按 id 排序，含 age_sec 供观测）。
pub fn list_rows() -> Vec<serde_json::Value> {
    ensure_listener();
    let now = now_ms();
    let mut map = store().lock().unwrap();
    sweep(&mut map, now);
    let mut rows: Vec<&ReceiverInfo> = map.values().collect();
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows.iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "name": r.name,
                "addr": r.ip,
                "port": r.port,
                "state": r.state,
                "codecs": r.codecs,
                "age_sec": (now.saturating_sub(r.last_seen_ms)) as f64 / 1000.0,
            })
        })
        .collect()
}

/// 按 id 取接收器（未过期；先确保监听已起）。
pub fn get(id: &str) -> Option<ReceiverInfo> {
    ensure_listener();
    let now = now_ms();
    let mut map = store().lock().unwrap();
    sweep(&mut map, now);
    map.get(id).cloned()
}

/// lite 侧：构造心跳负载（`codecs` 宣称供 P4 自动路由，#50）。
pub fn heartbeat_payload(id: &str, name: &str, port: u16, state: &str, codecs: &[&str]) -> String {
    serde_json::json!({"id": id, "name": name, "port": port, "state": state, "codecs": codecs})
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t1_parse_heartbeat() {
        let json = r#"{"id":"lite-1","name":"客厅","port":7915,"state":"playing"}"#;
        let hb = parse_heartbeat(json.as_bytes()).unwrap();
        assert_eq!(
            (hb.id.as_str(), hb.name.as_str(), hb.port, hb.state.as_str()),
            ("lite-1", "客厅", 7915, "playing")
        );
        assert!(hb.codecs.is_empty(), "缺 codecs → 空表（兼容 P3 lite）");
        // 带 codecs：小写化 + 非字符串过滤
        let hb = parse_heartbeat(
            br#"{"id":"a","port":1,"codecs":["H264", "AAC", 7]}"#,
        )
        .unwrap();
        assert_eq!(hb.codecs, vec!["h264", "aac"]);
        // 缺 id / 坏 JSON / 缺 port → 拒收
        assert!(parse_heartbeat(br#"{"name":"x","port":7915}"#).is_none());
        assert!(parse_heartbeat(b"not json").is_none());
        assert!(parse_heartbeat(br#"{"id":"","port":7915}"#).is_none());
        assert!(parse_heartbeat(br#"{"id":"a"}"#).is_none());
    }

    fn hb(id: &str, name: &str, port: u16, state: &str) -> Heartbeat {
        Heartbeat {
            id: id.into(),
            name: name.into(),
            port,
            state: state.into(),
            codecs: Vec::new(),
        }
    }

    #[test]
    fn t2_upsert_sweep_ttl() {
        let mut map = HashMap::new();
        upsert(&mut map, hb("k1", "n", 7915, "idle"), "10.0.0.2", 1000);
        upsert(&mut map, hb("k2", "n", 7915, "idle"), "10.0.0.3", 5000);
        // 6000ms 时：k1 过期（>3000），k2 存活
        sweep(&mut map, 6000);
        assert!(!map.contains_key("k1"));
        assert!(map.contains_key("k2"));
        // 同 id 刷新 → 覆盖（state/last_seen 更新）
        upsert(&mut map, hb("k2", "n2", 1234, "playing"), "10.0.0.3", 7000);
        sweep(&mut map, 7100);
        let k2 = &map["k2"];
        assert_eq!(k2.state, "playing");
        assert_eq!(k2.port, 1234);
        assert_eq!(k2.name, "n2");
    }

    #[test]
    fn t3_heartbeat_payload_roundtrip() {
        let s = heartbeat_payload("lite-9", "TV", 7915, "idle", &["h264", "aac"]);
        let hb = parse_heartbeat(s.as_bytes()).unwrap();
        assert_eq!(
            (hb.id.as_str(), hb.port, hb.state.as_str()),
            ("lite-9", 7915, "idle")
        );
        assert_eq!(hb.name, "TV");
        assert_eq!(hb.codecs, vec!["h264", "aac"], "codecs 宣称随心跳外发（#50）");
    }

    #[test]
    fn t4_list_rows_sorted_and_shape() {
        // 全程操作内部 store（装载级 OnceLock）：填两条并断言行形状/排序/过期。
        {
            let mut map = store().lock().unwrap();
            map.clear();
            let now = now_ms();
            let mut z = hb("z-id", "Z", 7915, "idle");
            z.codecs = vec!["h264".into()];
            upsert(&mut map, z, "127.0.0.1", now);
            upsert(&mut map, hb("a-id", "A", 7915, "playing"), "127.0.0.1", now);
        }
        let rows = list_rows();
        assert!(listen_port() != 0, "list_rows 确保监听已起（7916 或临时端口）");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["id"], "a-id", "按 id 排序");
        assert_eq!(rows[1]["id"], "z-id");
        assert_eq!(rows[0]["addr"], "127.0.0.1");
        assert_eq!(rows[0]["port"], 7915);
        assert_eq!(rows[0]["codecs"], serde_json::json!([]), "未宣称 → 空数组");
        assert_eq!(rows[1]["codecs"], serde_json::json!(["h264"]), "#50 codecs 行");
        assert!(rows[0]["age_sec"].as_f64().unwrap() >= 0.0);
        // 全体过期 → 空表
        {
            let mut map = store().lock().unwrap();
            map.clear();
            upsert(&mut map, hb("old", "O", 7915, "idle"), "127.0.0.1", 1);
        }
        assert!(list_rows().is_empty());
    }
}
