//! 配置加载与校验（对齐 Python config.py）。
//!
//! config.json 加载 + 默认值回退。models 字段语义为「权威清单」，
//! 启动时与上游 /v1/models 并集去重后写回 config.json。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub api_key: String,

    #[serde(default = "default_anthropic_base")]
    pub anthropic_base: String,
    #[serde(default = "default_openai_base")]
    pub openai_base: String,

    #[serde(default = "default_zcode_app_version")]
    pub zcode_app_version: String,
    #[serde(default = "default_zcode_agent")]
    pub zcode_agent: String,
    #[serde(default = "default_zcode_title")]
    pub zcode_title: String,
    #[serde(default = "default_zcode_referer")]
    pub zcode_referer: String,
    #[serde(default)]
    pub zcode_session_id: String,

    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub management_key: String,

    #[serde(default = "default_models")]
    pub models: Vec<String>,
    #[serde(default = "default_upstream_timeout")]
    pub upstream_timeout: f64,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

// ---- 默认值函数 ---- //
fn default_anthropic_base() -> String {
    "https://open.bigmodel.cn/api/anthropic".into()
}
fn default_openai_base() -> String {
    "https://open.bigmodel.cn/api/coding/paas/v4".into()
}
fn default_zcode_app_version() -> String {
    "3.1.0".into()
}
fn default_zcode_agent() -> String {
    "glm".into()
}
fn default_zcode_title() -> String {
    "Z Code@electron".into()
}
fn default_zcode_referer() -> String {
    "https://zcode.z.ai".into()
}
fn default_host() -> String {
    "127.0.0.1".into()
}
fn default_port() -> u16 {
    8787
}
fn default_models() -> Vec<String> {
    vec!["GLM-5.2".into(), "glm-5-turbo".into()]
}
fn default_upstream_timeout() -> f64 {
    300.0
}
fn default_log_level() -> String {
    "INFO".into()
}

impl Config {
    /// 从 JSON 文件加载配置。`api_key = "auto"` 视为未设置（报错，因为
    /// Rust 版运行在服务器上，无法读本机 ZCode）。
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read config file: {}", path.display()))?;
        let mut cfg: Config = serde_json::from_str(&content)
            .with_context(|| format!("invalid JSON in config: {}", path.display()))?;

        // auto：从本机 ZCode 安装目录自动读取 bigmodel-coding-plan 的 apiKey
        if cfg.api_key == "auto" {
            match read_zcode_plan_key(None) {
                Some(k) => {
                    tracing::info!("auto-resolved api_key from local ZCode config");
                    cfg.api_key = k;
                }
                None => anyhow::bail!(
                    "api_key=auto but could not find ZCode Coding Plan key on this machine. \
                     Run on a machine with ZCode installed, or set api_key explicitly."
                ),
            }
        }

        if cfg.api_key.is_empty() {
            anyhow::bail!("api_key is required (set it in config.json or use 'auto')");
        }

        if (cfg.host == "0.0.0.0" || cfg.host == "::") && cfg.management_key.is_empty() {
            eprintln!(
                "WARNING: listening on {} without management_key — \
                 anyone can use your quota",
                cfg.host
            );
        }
        Ok(cfg)
    }

    /// 把更新后的 models 写回 config.json（保留其它字段）。
    pub fn write_back_models(path: &Path, models: &[String]) -> Result<()> {
        let content = std::fs::read_to_string(path)?;
        let mut raw: serde_json::Value = serde_json::from_str(&content)?;
        if let Some(obj) = raw.as_object_mut() {
            obj.insert(
                "models".into(),
                serde_json::Value::Array(models.iter().map(|m| m.clone().into()).collect()),
            );
        }
        let out = serde_json::to_string_pretty(&raw)?;
        std::fs::write(path, out + "\n")?;
        Ok(())
    }

    /// 查找配置文件路径：--config > RELAY_CONFIG 环境变量 > 脚本同目录 > /data/config.json
    pub fn resolve_path(explicit: Option<&str>) -> Result<PathBuf> {
        if let Some(p) = explicit {
            return Ok(PathBuf::from(p));
        }
        if let Ok(p) = std::env::var("RELAY_CONFIG") {
            return Ok(PathBuf::from(p));
        }
        // 脚本同目录
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let local = dir.join("config.json");
                if local.is_file() {
                    return Ok(local);
                }
            }
        }
        // 容器挂载点兜底
        let data = PathBuf::from("/data/config.json");
        if data.is_file() {
            return Ok(data);
        }
        anyhow::bail!("config.json not found; copy config.example.json to config.json")
    }
}

/// 从 ZCode 的 config.json 读取 bigmodel-coding-plan 的 apiKey。
///
/// 优先级：ZCODE_CONFIG_PATH 环境变量（测试/自定义用）> ~/.zcode/v2/config.json
/// > ~/AppData/Roaming/ZCode/config.json（Windows）。找不到返回 None。
pub fn read_zcode_plan_key(explicit: Option<&Path>) -> Option<String> {
    let candidates: Vec<PathBuf> = if let Some(p) = explicit {
        vec![p.to_path_buf()]
    } else if let Ok(p) = std::env::var("ZCODE_CONFIG_PATH") {
        vec![PathBuf::from(p)]
    } else {
        let mut v = Vec::new();
        if let Some(home) = dirs_home() {
            v.push(home.join(".zcode").join("v2").join("config.json"));
            v.push(
                home.join("AppData")
                    .join("Roaming")
                    .join("ZCode")
                    .join("config.json"),
            );
        }
        v
    };

    for path in candidates {
        if let Some(key) = try_read_plan_key(&path) {
            return Some(key);
        }
    }
    None
}

/// 从单个 ZCode config 文件里提取 bigmodel-coding-plan 的 apiKey。
fn try_read_plan_key(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let root: serde_json::Value = serde_json::from_str(&content).ok()?;
    let providers = root.get("provider")?.as_object()?;
    for pid in &["builtin:bigmodel-coding-plan", "bigmodel-coding-plan"] {
        if let Some(key) = providers
            .get(*pid)
            .and_then(|p| p.get("options"))
            .and_then(|o| o.get("apiKey"))
            .and_then(|k| k.as_str())
        {
            if !key.is_empty() {
                return Some(key.to_string());
            }
        }
    }
    None
}

/// 获取用户 home 目录（跨平台）。
fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_tmp(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    #[test]
    fn load_full_config() {
        let f = write_tmp(
            r#"{"api_key":"k1","management_key":"m1","host":"0.0.0.0","port":9000,"models":["A","B"]}"#,
        );
        let cfg = Config::load(f.path()).unwrap();
        assert_eq!(cfg.api_key, "k1");
        assert_eq!(cfg.management_key, "m1");
        assert_eq!(cfg.host, "0.0.0.0");
        assert_eq!(cfg.port, 9000);
        assert_eq!(cfg.models, vec!["A".to_string(), "B".to_string()]);
    }

    #[test]
    fn load_uses_defaults_for_missing_fields() {
        let f = write_tmp(r#"{"api_key":"k1"}"#);
        let cfg = Config::load(f.path()).unwrap();
        assert_eq!(cfg.anthropic_base, "https://open.bigmodel.cn/api/anthropic");
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 8787);
        assert_eq!(
            cfg.models,
            vec!["GLM-5.2".to_string(), "glm-5-turbo".to_string()]
        );
    }

    #[test]
    fn auto_key_rejected_when_no_zcode_found() {
        // read_zcode_plan_key 指向不存在的文件 → 返回 None
        let bogus = PathBuf::from("/nonexistent/zcode/config.json");
        assert!(read_zcode_plan_key(Some(&bogus)).is_none());
    }

    #[test]
    fn auto_key_reads_from_zcode_config() {
        // 模拟 ZCode 的 config.json，含 bigmodel-coding-plan 的 apiKey
        let zcode_cfg = write_tmp(
            r#"{"provider":{"builtin:bigmodel-coding-plan":{"options":{"apiKey":"auto-resolved-key-123"}}}}"#,
        );
        std::env::set_var("ZCODE_CONFIG_PATH", zcode_cfg.path());
        let f = write_tmp(r#"{"api_key":"auto"}"#);
        let cfg = Config::load(f.path()).unwrap();
        assert_eq!(cfg.api_key, "auto-resolved-key-123");
        std::env::remove_var("ZCODE_CONFIG_PATH");
    }

    #[test]
    fn read_zcode_plan_key_finds_coding_plan() {
        let zcode_cfg = write_tmp(
            r#"{"provider":{"builtin:bigmodel-coding-plan":{"options":{"apiKey":"found-key"}}}}"#,
        );
        let key = read_zcode_plan_key(Some(zcode_cfg.path()));
        assert_eq!(key.as_deref(), Some("found-key"));
    }

    #[test]
    fn read_zcode_plan_key_returns_none_when_missing() {
        let zcode_cfg = write_tmp(r#"{"provider":{"other":{"options":{"apiKey":"x"}}}}"#);
        assert!(read_zcode_plan_key(Some(zcode_cfg.path())).is_none());
    }

    #[test]
    fn read_zcode_plan_key_handles_alternate_provider_id() {
        // 不带 builtin: 前缀的 provider id 也要认
        let zcode_cfg =
            write_tmp(r#"{"provider":{"bigmodel-coding-plan":{"options":{"apiKey":"alt-key"}}}}"#);
        let key = read_zcode_plan_key(Some(zcode_cfg.path()));
        assert_eq!(key.as_deref(), Some("alt-key"));
    }

    #[test]
    fn empty_key_rejected() {
        let f = write_tmp(r#"{"api_key":""}"#);
        assert!(Config::load(f.path()).is_err());
    }

    #[test]
    fn write_back_models_preserves_other_fields() {
        let f = write_tmp(r#"{"api_key":"k1","management_key":"m1","models":["old"],"port":1234}"#);
        let path = f.path().to_path_buf();
        Config::write_back_models(&path, &["new1".into(), "new2".into()]).unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.models, vec!["new1".to_string(), "new2".to_string()]);
        assert_eq!(cfg.api_key, "k1"); // 其它字段保留
        assert_eq!(cfg.management_key, "m1");
        assert_eq!(cfg.port, 1234);
    }
}
