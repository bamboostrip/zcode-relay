//! 上游转发核心：reqwest 连接池 + 重试 + SSE 流式。
//!
//! 设计要点（与 Python proxy.py 对齐）：
//! - 全局 reqwest::Client（连接池 + keep-alive + HTTP/2）。
//! - 重试：对非流式和「流式首字节前」的瞬时错误（429/5xx/连接错误）重试，
//!   指数退避（对齐 ZCode）。流式一旦开始传输（首字节已发）不再重试。
//! - body 原样透传（thinking/effort 等字段完整到达智谱）。

use crate::config::Config;
use crate::headers::{build_zcode_headers, HeaderConfig};
use crate::retry::{compute_backoff, should_retry};
use anyhow::Result;
use futures_util::{Stream, StreamExt};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::sleep;

/// 重试配置。
#[derive(Clone)]
pub struct RetryConfig {
    pub max_retries: u32,
    pub initial_delay: f64,
    pub factor: f64,
    pub max_delay: f64,
    pub jitter: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 10, // 对齐 ZCode UI「10 次机会」
            initial_delay: 2.0,
            factor: 2.0,
            max_delay: 20.0,
            jitter: 1.0,
        }
    }
}

/// 转发结果：非流式含 body；流式含字节流。
pub enum ProxyResult {
    NonStream {
        status: u16,
        headers: HashMap<String, String>,
        body: bytes::Bytes,
    },
    Stream {
        status: u16,
        content_type: String,
        stream: Box<dyn Stream<Item = Result<bytes::Bytes, anyhow::Error>> + Send + Unpin>,
    },
    /// 重试耗尽后的错误响应
    Error { status: u16, body: bytes::Bytes },
}

/// 上游转发器。
pub struct Proxy {
    client: reqwest::Client,
    retry: RetryConfig,
}

impl Proxy {
    pub fn new(cfg: &Config, retry: RetryConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            // reqwest 默认通过 ALPN 协商 HTTP/2（TLS 连接自动升级），无需显式开启
            .pool_max_idle_per_host(20)
            .pool_idle_timeout(Duration::from_secs(90))
            .timeout(Duration::from_secs(cfg.upstream_timeout as u64))
            .build()?;
        Ok(Self { client, retry })
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    fn build_headers(
        &self,
        cfg: &Config,
        extra: &HashMap<String, String>,
    ) -> reqwest::header::HeaderMap {
        let mut h = build_zcode_headers(cfg);
        h.insert("x-api-key".into(), cfg.api_key.clone());
        h.insert("Content-Type".into(), "application/json".into());
        // 合并透传的协议头
        for (k, v) in extra {
            h.insert(k.clone(), v.clone());
        }
        let mut map = reqwest::header::HeaderMap::new();
        for (k, v) in h {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(&v),
            ) {
                map.insert(name, val);
            }
        }
        map
    }

    /// 转发请求，带重试。
    pub async fn forward(
        &self,
        cfg: &Config,
        method: &str,
        url: &str,
        body: bytes::Bytes,
        stream: bool,
        extra_headers: HashMap<String, String>,
    ) -> ProxyResult {
        let headers = self.build_headers(cfg, &extra_headers);
        let attempts = self.retry.max_retries + 1;

        for attempt in 1..=attempts {
            match self
                .try_once(method, url, body.clone(), headers.clone(), stream)
                .await
            {
                Ok(result) => return result,
                Err(RetryableErr {
                    status,
                    body: err_body,
                    retry_after,
                }) => {
                    if attempt >= attempts {
                        tracing::warn!(
                            "retry exhausted after {} attempts: HTTP {}",
                            attempt,
                            status
                        );
                        return ProxyResult::Error {
                            status,
                            body: err_body,
                        };
                    }
                    let delay = compute_backoff(
                        attempt,
                        self.retry.initial_delay,
                        self.retry.factor,
                        self.retry.max_delay,
                        self.retry.jitter,
                        retry_after,
                    );
                    tracing::info!(
                        "attempt {} got {}, retrying in {:.1}s",
                        attempt,
                        status,
                        delay
                    );
                    sleep(Duration::from_secs_f64(delay)).await;
                }
            }
        }
        unreachable!("retry loop must return")
    }

    async fn try_once(
        &self,
        method: &str,
        url: &str,
        body: bytes::Bytes,
        headers: reqwest::header::HeaderMap,
        stream: bool,
    ) -> Result<ProxyResult, RetryableErr> {
        let req_method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| RetryableErr::new(500, bytes::Bytes::new()))?;
        let resp = self
            .client
            .request(req_method, url)
            .headers(headers)
            .body(body)
            .send()
            .await
            .map_err(|e| {
                tracing::warn!("upstream connect error: {}", e);
                RetryableErr::new(502, bytes::Bytes::new())
            })?;

        let status = resp.status().as_u16();

        if stream {
            // 流式：首字节前失败可重试
            if should_retry(status) {
                // 先取 retry_after（bytes() 会消费 resp）
                let retry_after = parse_retry_after(&resp);
                let err_body = resp.bytes().await.unwrap_or_default();
                return Err(RetryableErr {
                    status,
                    body: err_body,
                    retry_after,
                });
            }
            let content_type = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("text/event-stream")
                .to_string();
            let stream = resp.bytes_stream().map(|r| r.map_err(anyhow::Error::from));
            return Ok(ProxyResult::Stream {
                status,
                content_type,
                stream: Box::new(Box::pin(stream)),
            });
        }

        // 非流式
        if should_retry(status) {
            let retry_after = parse_retry_after(&resp);
            let err_body = resp.bytes().await.unwrap_or_default();
            return Err(RetryableErr {
                status,
                body: err_body,
                retry_after,
            });
        }
        let mut hdrs = HashMap::new();
        for (k, v) in resp.headers() {
            if let Ok(vs) = v.to_str() {
                hdrs.insert(k.as_str().to_string(), vs.to_string());
            }
        }
        let body = resp.bytes().await.unwrap_or_default();
        Ok(ProxyResult::NonStream {
            status,
            headers: hdrs,
            body,
        })
    }
}

impl HeaderConfig for Config {
    fn zcode_app_version(&self) -> &str {
        &self.zcode_app_version
    }
    fn zcode_agent(&self) -> &str {
        &self.zcode_agent
    }
    fn zcode_title(&self) -> &str {
        &self.zcode_title
    }
    fn zcode_referer(&self) -> &str {
        &self.zcode_referer
    }
    fn zcode_session_id(&self) -> &str {
        &self.zcode_session_id
    }
}

struct RetryableErr {
    status: u16,
    body: bytes::Bytes,
    retry_after: Option<f64>,
}

impl RetryableErr {
    fn new(status: u16, body: bytes::Bytes) -> Self {
        Self {
            status,
            body,
            retry_after: None,
        }
    }
}

/// 从响应头解析 Retry-After（秒）。在 bytes() 消费 resp 之前调用。
fn parse_retry_after(resp: &reqwest::Response) -> Option<f64> {
    resp.headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<f64>().ok())
}
