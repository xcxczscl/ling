//! `rm-plugin`：resources-manager 插件壳（`docs/04` §2）。
//!
//! 职责：持有 SQLite 连接与作业表，把 `rm-*` 领域库的方法包装成 ling
//! `Context::register_method` 的处理器。入口薄，业务在领域包。

pub mod events;
pub mod methods;
pub mod methods_m2;
pub mod methods_m3;
pub mod methods_m4;
pub mod methods_m5;
pub mod state;
pub mod stream;
pub mod ui;

pub use state::RmState;

use std::sync::{Arc, Mutex};

use ling_sdk::{ActivateError, Context, Plugin};

use rm_proto::codes;

fn activate_err(msg: impl Into<String>) -> ActivateError {
    ActivateError::new(msg.into())
}

/// resources-manager 插件。
pub struct ResourcesManager {
    state: Option<Arc<RmState>>,
}

impl Default for ResourcesManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourcesManager {
    pub fn new() -> Self {
        Self { state: None }
    }
}

impl Plugin for ResourcesManager {
    fn id(&self) -> &str {
        "resources-manager"
    }

    fn activate(&mut self, ctx: Context) -> Result<(), ActivateError> {
        let data_dir = ctx.data_dir();
        let db_path = data_dir.join("catalog.db");
        let conn = rm_catalog::open(&db_path)
            .map_err(|e| activate_err(format!("打开目录库失败：{db_path:?} {e}")))?;
        // 预置标签域。
        rm_catalog::repo::tag::ensure_builtin_domains(&conn)
            .map_err(|e| activate_err(e.to_string()))?;
        // M3：Ed25519 身份（identity.key，0600）。
        let identity = Arc::new(
            rm_transfer::Identity::load_or_generate(&data_dir)
                .map_err(|e| activate_err(format!("加载设备身份失败：{}", e.message)))?,
        );
        // 本机设备：指纹来自 Ed25519 公钥（M1 简化指纹仅作遗留数据迁移依据）。
        let device_id = "dv_local";
        let fingerprint = identity.fingerprint().to_string();
        rm_catalog::repo::placement::device_upsert(
            &conn,
            device_id,
            &methods::hostname(),
            std::env::consts::OS,
            &fingerprint,
            rm_catalog::now(),
        )
        .map_err(|e| activate_err(e.to_string()))?;

        let state = Arc::new(RmState {
            conn: Mutex::new(conn),
            device_id: device_id.to_string(),
            data_dir: data_dir.clone(),
            cancel_flags: Mutex::new(Default::default()),
            identity,
            pairing: Arc::new(Mutex::new(rm_transfer::PairingState::default())),
            server: Mutex::new(None),
            stream: Mutex::new(None),
        });

        // 启动 LAN 传输面（默认 7912，占用顺延；端口写 transfer.json）。
        // 失败不阻断激活（仅失去配对/传输能力），日志级降级。
        match methods_m3::start_lan(&state) {
            Ok(port) => {
                // lan_port 是不可变字段——写回 registry 用 transfer.json；内存里经 server 读。
                let _ = port;
            }
            Err(e) => {
                eprintln!("[resources-manager] LAN 传输面启动失败：{}", e.message);
            }
        }

        // 启动流交付服务（03 §2.6 契约；失败不阻断激活——stream.open 返回 F504）。
        match stream::StreamHandle::start(data_dir.join("catalog.db")) {
            Ok(h) => *state.stream.lock().unwrap() = Some(h),
            Err(e) => eprintln!("[resources-manager] 流交付服务启动失败：{e}"),
        }

        methods::register_all(&ctx, &state).map_err(|e| activate_err(e.to_string()))?;
        events::watch_catalog(&ctx)?;
        contribute_ui_pages(&ctx, &data_dir);

        // 停用时：取消在途作业（置 cancel 位，扫描在批边界停）+ 停止 LAN/流服务。
        let deactivate_state = Arc::clone(&state);
        ctx.on_deactivate(Box::new(move || {
            let flags = deactivate_state.cancel_flags.lock().unwrap();
            for f in flags.values() {
                f.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            drop(flags);
            // 停止 LAN 传输面：打破 ServerHandle→Catalog→RmState 引用环。
            if let Some(handle) = deactivate_state.server.lock().unwrap().take() {
                handle.stop();
            }
            // 停止流交付服务（token 表随句柄销毁，全部失效——契约 §2.6.3）。
            if let Some(handle) = deactivate_state.stream.lock().unwrap().take() {
                handle.stop();
            }
        }));

        self.state = Some(state);
        let _ = codes::F101; // 触达 codes 使用
        Ok(())
    }

    fn deactivate(&mut self) {
        if let Some(state) = self.state.take() {
            let flags = state.cancel_flags.lock().unwrap();
            for f in flags.values() {
                f.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            drop(flags);
            if let Some(handle) = state.server.lock().unwrap().take() {
                handle.stop();
            }
            if let Some(handle) = state.stream.lock().unwrap().take() {
                handle.stop();
            }
        }
    }
}

/// 把 M1 静态页写入 data_dir/ui 并尝试贡献给 core-ui（无 core-ui 时静默跳过）。
fn contribute_ui_pages(ctx: &Context, data_dir: &std::path::Path) {
    let asset_dir = data_dir.join("ui");
    let _ = std::fs::create_dir_all(&asset_dir);
    for (path, title, icon, html) in ui::pages() {
        // core-ui 按 `asset_dir + rel(index.html)` 解析；每页独立 asset_dir 避免互踩。
        let page_assets = asset_dir.join(path.trim_start_matches('/'));
        let _ = std::fs::create_dir_all(&page_assets);
        let _ = std::fs::write(page_assets.join("index.html"), html);
        let args = serde_json::json!({
            "plugin": "resources-manager",
            "type": "page",
            "path": path,
            "title": title,
            "icon": icon,
            "order": 200,
            "asset_dir": page_assets.display().to_string(),
        });
        // core-ui 未装载时 call 失败 → 忽略（页面仍可经 ui.page 方法取到）。
        let _ = ctx.call("core-ui", "core-ui.ui.contribute", args);
    }
}
