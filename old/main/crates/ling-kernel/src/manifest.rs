use std::path::Path;

use serde::Deserialize;

use ling_sdk::codes;
use ling_sdk::{Capability, HOST_API};

use crate::error::KernelError;

const MAX_ID_LEN: usize = 64;
const MAX_VERSION_PART: u32 = 65535;

/// 插件清单（`plugin.toml`，格式见 `docs/04` §2）。
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub plugin: PluginMeta,
    #[serde(default)]
    pub requires: Requires,
    pub sidecar: Option<SidecarMeta>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginMeta {
    pub id: String,
    pub version: String,
    pub api: ApiField,
    pub name: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ApiField {
    Str(String),
    Int(i64),
}

impl ApiField {
    fn parse(&self) -> Option<u32> {
        match self {
            ApiField::Str(s) => s.parse().ok(),
            ApiField::Int(i) if *i >= 0 => u32::try_from(*i).ok(),
            ApiField::Int(_) => None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Requires {
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub plugins: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SidecarMeta {
    pub executable: String,
    pub protocol: String,
    pub restart: String,
}

impl Manifest {
    /// 解析并完成全部字段校验（`docs/04` §2 字段校验表）。
    /// `dir` 是清单所在目录；侧车可执行文件存在性在 `dir` 下核对。
    pub fn parse_and_validate(text: &str, dir: &Path) -> Result<Manifest, KernelError> {
        let manifest: Manifest = toml::from_str(text).map_err(|e| {
            KernelError::manifest(codes::E_MANIFEST_INVALID, format!("清单解析失败：{e}"))
        })?;

        let id = &manifest.plugin.id;
        if !valid_id(id) {
            return Err(KernelError::manifest(
                codes::E_MANIFEST_INVALID,
                format!("非法插件编号（小写字母/数字/短横线，长度≤{MAX_ID_LEN}）：{id}"),
            ));
        }
        if !valid_version(&manifest.plugin.version) {
            return Err(KernelError::manifest(
                codes::E_MANIFEST_INVALID,
                format!(
                    "非法版本号（三段式，各段≤{MAX_VERSION_PART}）：{}",
                    manifest.plugin.version
                ),
            ));
        }
        let api = manifest.plugin.api.parse().ok_or_else(|| {
            KernelError::manifest(
                codes::E_MANIFEST_INVALID,
                format!("api 字段须为非负整数：{:?}", manifest.plugin.api),
            )
        })?;
        if api != HOST_API {
            return Err(KernelError::manifest(
                codes::E_API_MISMATCH,
                format!("插件面向 api={api}，宿主提供 api={HOST_API}"),
            ));
        }
        for cap in &manifest.requires.capabilities {
            if Capability::parse(cap).is_none() {
                return Err(KernelError::manifest(
                    codes::E_CAPABILITY_UNKNOWN,
                    format!("清单声明了宿主不认识的能力 {cap:?}（宿主版本过旧）"),
                ));
            }
        }
        for pref in &manifest.requires.plugins {
            valid_plugin_ref(pref)
                .map_err(|m| KernelError::manifest(codes::E_MANIFEST_INVALID, m))?;
        }
        if let Some(sidecar) = &manifest.sidecar {
            validate_sidecar_path(&sidecar.executable, dir)?;
            if sidecar.protocol != "jsonl" {
                return Err(KernelError::manifest(
                    codes::E_MANIFEST_INVALID,
                    format!("sidecar.protocol 首版固定 jsonl：{}", sidecar.protocol),
                ));
            }
            if sidecar.restart != "on-crash" && sidecar.restart != "never" {
                return Err(KernelError::manifest(
                    codes::E_MANIFEST_INVALID,
                    format!("sidecar.restart 取值非法：{}", sidecar.restart),
                ));
            }
        }
        Ok(manifest)
    }

    pub fn declared_capabilities(&self) -> Result<Vec<Capability>, KernelError> {
        self.requires
            .capabilities
            .iter()
            .map(|c| {
                Capability::parse(c).ok_or_else(|| {
                    KernelError::manifest(codes::E_CAPABILITY_UNKNOWN, format!("未知能力：{c}"))
                })
            })
            .collect()
    }

    pub fn is_sidecar(&self) -> bool {
        self.sidecar.is_some()
    }
}

pub fn read_manifest(dir: &Path) -> Result<Manifest, KernelError> {
    let path = dir.join("plugin.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| {
        KernelError::manifest(
            codes::E_MANIFEST_INVALID,
            format!("读取 {} 失败：{e}", path.display()),
        )
    })?;
    Manifest::parse_and_validate(&text, dir)
}

fn valid_id(id: &str) -> bool {
    if id.is_empty() || id.len() > MAX_ID_LEN {
        return false;
    }
    let mut chars = id.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            p.parse::<u32>()
                .map(|n| n <= MAX_VERSION_PART)
                .unwrap_or(false)
        })
}

/// 版本约束（`docs/03` §1.2）：`@1` 主版本、`@1.2` 主+次、`@1.2.3` 精确，
/// 按段前缀匹配；1–3 段皆合法。
fn valid_version_spec(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    (1..=3).contains(&parts.len())
        && parts.iter().all(|p| {
            p.parse::<u32>()
                .map(|n| n <= MAX_VERSION_PART)
                .unwrap_or(false)
        })
}

fn valid_plugin_ref(s: &str) -> Result<(), String> {
    let (id, ver) = match s.split_once('@') {
        Some((i, v)) => (i, Some(v)),
        None => (s, None),
    };
    if !valid_id(id) {
        return Err(format!("requires.plugins 条目编号非法：{s}"));
    }
    if let Some(v) = ver {
        if !valid_version_spec(v) {
            return Err(format!("requires.plugins 条目版本非法：{s}"));
        }
    }
    Ok(())
}

fn validate_sidecar_path(rel: &str, dir: &Path) -> Result<(), KernelError> {
    let p = Path::new(rel);
    if p.is_absolute() || rel.split('/').any(|seg| seg == "..") {
        return Err(KernelError::manifest(
            codes::E_MANIFEST_INVALID,
            format!("sidecar.executable 含非法路径：{rel}"),
        ));
    }
    let full = dir.join(p);
    if !full.is_file() {
        return Err(KernelError::manifest(
            codes::E_MANIFEST_INVALID,
            format!("sidecar.executable 不存在或不是文件：{rel}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        std::env::temp_dir()
    }

    fn ok_manifest() -> &'static str {
        r#"
[plugin]
id = "demo-a"
version = "0.1.0"
api = "1"

[requires]
capabilities = ["fs"]
plugins = ["demo-b@1.0.0"]
"#
    }

    #[test]
    fn valid_manifest_parses() {
        let m = Manifest::parse_and_validate(ok_manifest(), &dir()).unwrap();
        assert_eq!(m.plugin.id, "demo-a");
        assert_eq!(m.declared_capabilities().unwrap(), vec![Capability::Fs]);
    }

    #[test]
    fn bad_id_rejected() {
        let text = ok_manifest().replace("demo-a", "Demo_A");
        let e = Manifest::parse_and_validate(&text, &dir()).unwrap_err();
        assert_eq!(e.ecode(), Some(codes::E_MANIFEST_INVALID));
    }

    #[test]
    fn api_mismatch_rejected() {
        let text = ok_manifest().replace(r#"api = "1""#, r#"api = "2""#);
        let e = Manifest::parse_and_validate(&text, &dir()).unwrap_err();
        assert_eq!(e.ecode(), Some(codes::E_API_MISMATCH));
    }

    #[test]
    fn unknown_capability_rejected() {
        let text = ok_manifest().replace("fs", "telepathy");
        let e = Manifest::parse_and_validate(&text, &dir()).unwrap_err();
        assert_eq!(e.ecode(), Some(codes::E_CAPABILITY_UNKNOWN));
    }

    #[test]
    fn bad_plugin_ref_rejected() {
        let text = ok_manifest().replace("demo-b@1.0.0", "demo-b@x");
        let e = Manifest::parse_and_validate(&text, &dir()).unwrap_err();
        assert_eq!(e.ecode(), Some(codes::E_MANIFEST_INVALID));
    }

    #[test]
    fn bad_version_rejected() {
        let text = ok_manifest().replace("0.1.0", "0.1");
        let e = Manifest::parse_and_validate(&text, &dir()).unwrap_err();
        assert_eq!(e.ecode(), Some(codes::E_MANIFEST_INVALID));
    }
}
