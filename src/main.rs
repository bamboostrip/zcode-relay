//! zcode-relay — 把 ZCode Coding Plan 额度转发成标准 API。
//!
//! 基于 axum + reqwest + tokio。启动：`./zcode-relay`（读 config.json）。
//! 详见 README.md。

mod app;
mod auth;
mod config;
mod headers;
mod models;
mod proxy;
mod retry;

use anyhow::Result;
use config::Config;
use proxy::{Proxy, RetryConfig};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // 健康检查子命令（Docker HEALTHCHECK 用，distroless 无 curl）
    if args.iter().any(|a| a == "--healthcheck") {
        let port: u16 = std::env::var("HEALTHCHECK_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8787);
        let url = format!("http://127.0.0.1:{}/healthz", port);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(4))
            .build()?;
        match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => std::process::exit(0),
            _ => std::process::exit(1),
        }
    }

    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "zcode_relays=info".parse().unwrap()),
        )
        .with_target(false)
        .init();

    // 解析配置路径（--config / RELAY_CONFIG / 脚本同目录 / /data/config.json）
    let explicit = args
        .iter()
        .position(|a| a == "--config" || a == "-c")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str());
    let cfg_path = Config::resolve_path(explicit)?;
    let cfg = Config::load(&cfg_path)?;

    tracing::info!("zcode-relay starting on {}:{}", cfg.host, cfg.port);
    tracing::info!(
        "upstream: anthropic={} | openai={}",
        cfg.anthropic_base,
        cfg.openai_base
    );
    tracing::info!(
        "management auth: {}",
        if cfg.management_key.is_empty() {
            "DISABLED (open access)"
        } else {
            "ENABLED"
        }
    );
    tracing::info!(
        "models in config: {}",
        if cfg.models.is_empty() {
            "(none)".into()
        } else {
            cfg.models.join(", ")
        }
    );

    // 构建上游客户端
    let proxy = Proxy::new(&cfg, RetryConfig::default())?;

    // 模型清单 bootstrap（拉取+合并+写回）
    let models = models::ModelRegistry::new(&cfg, &cfg_path);
    models.bootstrap(proxy.client(), &cfg).await;

    // 组装应用状态
    let state = Arc::new(app::AppState {
        cfg: cfg.clone(),
        proxy,
        models,
    });

    let app = app::create_app(state);
    let listener = tokio::net::TcpListener::bind((cfg.host.as_str(), cfg.port)).await?;
    tracing::info!("listening on http://{}:{}", cfg.host, cfg.port);
    axum::serve(listener, app).await?;
    Ok(())
}
