//! 模型清单管理：config.models（权威清单）∪ 上游列表，并集去重后写回 config.json。
//!
//! 语义（与 Python 版一致）：
//! - config.json 的 models 是「权威清单」：用户手填确定可用的模型。
//! - 启动时拉取上游 /v1/models，与 config.models 并集去重，结果写回 config.json。
//! - 官方列表以后补了新模型 → 自动并入，代码零改动；出新模型在 config 加一行即可。

use crate::config::Config;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// config.models 与上游列表并集去重（config 在前保留优先级）。
pub fn merge_models(config_models: &[String], upstream_models: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut merged: Vec<String> = Vec::new();
    for m in config_models.iter().chain(upstream_models.iter()) {
        if !m.is_empty() && seen.insert(m.clone()) {
            merged.push(m.clone());
        }
    }
    merged
}

/// 从上游 /v1/models 拉取模型 id 列表。失败返回空 Vec（不阻断，靠 config 兜底）。
pub async fn fetch_upstream(
    client: &reqwest::Client,
    anthropic_base: &str,
    api_key: &str,
) -> Vec<String> {
    let url = format!("{}/v1/models", anthropic_base.trim_end_matches('/'));
    let result = client
        .get(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .timeout(Duration::from_secs(15))
        .send()
        .await;

    match result {
        Ok(resp) if resp.status().is_success() => match resp.json::<UpstreamModels>().await {
            Ok(body) => {
                let ids: Vec<String> = body.data.into_iter().filter_map(|m| m.id).collect();
                tracing::info!(
                    "fetched {} models from upstream: {}",
                    ids.len(),
                    ids.join(", ")
                );
                ids
            }
            Err(e) => {
                tracing::warn!(
                    "failed to parse upstream models ({}), use config.models only",
                    e
                );
                Vec::new()
            }
        },
        Ok(resp) => {
            tracing::warn!(
                "upstream /v1/models returned {}, use config.models only",
                resp.status()
            );
            Vec::new()
        }
        Err(e) => {
            tracing::warn!(
                "failed to fetch upstream models ({}), use config.models only",
                e
            );
            Vec::new()
        }
    }
}

/// 模型清单注册器：启动时 bootstrap（拉取+合并+写回），运行时缓存查询。
pub struct ModelRegistry {
    config_models: Mutex<Vec<String>>,
    cache: Mutex<Option<CachedModels>>,
    config_path: std::path::PathBuf,
}

struct CachedModels {
    models: Vec<String>,
    expires_at: Instant,
}

const CACHE_TTL: Duration = Duration::from_secs(600);

#[derive(serde::Deserialize)]
struct UpstreamModels {
    data: Vec<UpstreamModel>,
}

#[derive(serde::Deserialize)]
struct UpstreamModel {
    id: Option<String>,
}

impl ModelRegistry {
    pub fn new(cfg: &Config, config_path: &Path) -> Self {
        Self {
            config_models: Mutex::new(cfg.models.clone()),
            cache: Mutex::new(None),
            config_path: config_path.to_path_buf(),
        }
    }

    /// 启动时调用：拉取上游 → 合并 config.models → 写回 config.json。
    pub async fn bootstrap(&self, client: &reqwest::Client, cfg: &Config) -> Vec<String> {
        let upstream = fetch_upstream(client, &cfg.anthropic_base, &cfg.api_key).await;
        let config_models = self.config_models.lock().unwrap().clone();
        let merged = merge_models(&config_models, &upstream);

        // 与 config 现值不同才写回（幂等，避免无谓 IO）
        let set_eq = config_models.len() == merged.len()
            && config_models.iter().collect::<HashSet<_>>()
                == merged.iter().collect::<HashSet<_>>();
        if !set_eq {
            if let Err(e) = Config::write_back_models(&self.config_path, &merged) {
                tracing::warn!(
                    "cannot write config.json ({}); if Docker, ensure volume is not :ro",
                    e
                );
            } else {
                tracing::info!("merged models written back to config.json");
            }
            *self.config_models.lock().unwrap() = merged.clone();
        }

        *self.cache.lock().unwrap() = Some(CachedModels {
            models: merged.clone(),
            expires_at: Instant::now() + CACHE_TTL,
        });
        tracing::info!("models ready ({}): {}", merged.len(), merged.join(", "));
        merged
    }

    /// 返回当前清单（带缓存）。
    pub fn list_models(&self) -> Vec<String> {
        let guard = self.cache.lock().unwrap();
        if let Some(c) = guard.as_ref() {
            if c.expires_at > Instant::now() {
                return c.models.clone();
            }
        }
        drop(guard);
        // 过期或未加载：返回 config.models 兜底（避免阻塞）
        self.config_models.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_dedupes_with_config_first() {
        let config = vec!["GLM-5.2".to_string()];
        let upstream = vec![
            "glm-4.5".into(),
            "glm-5-turbo".into(),
            "GLM-5.2".into(), // 重复
        ];
        let merged = merge_models(&config, &upstream);
        assert_eq!(merged, vec!["GLM-5.2", "glm-4.5", "glm-5-turbo"]);
    }

    #[test]
    fn merge_empty_config_returns_upstream() {
        let merged = merge_models(&[], &["a".into(), "b".into()]);
        assert_eq!(merged, vec!["a", "b"]);
    }

    #[test]
    fn merge_empty_upstream_returns_config() {
        let merged = merge_models(&["a".into(), "b".into()], &[]);
        assert_eq!(merged, vec!["a", "b"]);
    }

    #[test]
    fn merge_skips_empty_strings() {
        let merged = merge_models(&["a".into(), "".into()], &["b".into(), "".into()]);
        assert_eq!(merged, vec!["a", "b"]);
    }

    #[test]
    fn merge_both_empty() {
        assert!(merge_models(&[], &[]).is_empty());
    }

    #[test]
    fn merge_preserves_order_config_then_upstream() {
        let config = vec!["c1".to_string(), "c2".to_string()];
        let upstream = vec!["u1".to_string(), "u2".to_string()];
        let merged = merge_models(&config, &upstream);
        assert_eq!(merged, vec!["c1", "c2", "u1", "u2"]);
    }
}
