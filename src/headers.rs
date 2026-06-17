//! ZCode 身份 header 构建（对齐真实 ZCode 客户端抓包）。
//!
//! 字段来源：从本机 ZCode 的 model-io rollout 日志抓取的真实请求 header。
//! 这 9 个 header 是智谱侧识别「请求来自 ZCode」、走 Coding Plan 额度的依据。

use std::collections::HashMap;
use uuid::Uuid;

/// build_zcode_headers 需要的配置（结构化，避免硬依赖完整 Config）。
pub trait HeaderConfig {
    fn zcode_app_version(&self) -> &str;
    fn zcode_agent(&self) -> &str;
    fn zcode_title(&self) -> &str;
    fn zcode_referer(&self) -> &str;
    fn zcode_session_id(&self) -> &str;
}

/// 构建发往上游的 ZCode 身份 header。
///
/// 9 个字段，分两组（与 ZCode 源码 zYn / grt 函数对应）：
///   静态身份：HTTP-Referer / User-Agent / X-ZCode-App-Version / X-Title / X-ZCode-Agent
///   单次归因：x-request-id / x-zcode-trace-id / x-query-id / x-session-id（纯 UUID）
///
/// 不注入 anthropic-version / anthropic-beta（由 SDK/透传管）。
pub fn build_zcode_headers<C: HeaderConfig + ?Sized>(cfg: &C) -> HashMap<String, String> {
    let session_id = {
        let s = cfg.zcode_session_id();
        if s.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            s.to_string()
        }
    };

    let mut h = HashMap::new();
    // ---- 静态身份 ----
    h.insert("HTTP-Referer".into(), cfg.zcode_referer().into());
    h.insert(
        "User-Agent".into(),
        format!("ZCode/{}", cfg.zcode_app_version()),
    );
    h.insert("X-ZCode-App-Version".into(), cfg.zcode_app_version().into());
    h.insert("X-Title".into(), cfg.zcode_title().into());
    h.insert("X-ZCode-Agent".into(), cfg.zcode_agent().into());
    // ---- 单次请求归因（每次调用随机） ----
    h.insert("x-request-id".into(), Uuid::new_v4().to_string());
    h.insert("x-zcode-trace-id".into(), Uuid::new_v4().to_string());
    h.insert("x-query-id".into(), Uuid::new_v4().to_string());
    h.insert("x-session-id".into(), session_id);
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeCfg {
        session_id: String,
    }
    impl HeaderConfig for FakeCfg {
        fn zcode_app_version(&self) -> &str {
            "3.1.0"
        }
        fn zcode_agent(&self) -> &str {
            "glm"
        }
        fn zcode_title(&self) -> &str {
            "Z Code@electron"
        }
        fn zcode_referer(&self) -> &str {
            "https://zcode.z.ai"
        }
        fn zcode_session_id(&self) -> &str {
            &self.session_id
        }
    }

    fn default_cfg() -> FakeCfg {
        FakeCfg {
            session_id: String::new(),
        }
    }

    #[test]
    fn contains_all_identity_fields() {
        let h = build_zcode_headers(&default_cfg());
        let expected = [
            "HTTP-Referer",
            "User-Agent",
            "X-ZCode-App-Version",
            "X-Title",
            "X-ZCode-Agent",
            "x-request-id",
            "x-zcode-trace-id",
            "x-query-id",
            "x-session-id",
        ];
        for k in expected {
            assert!(h.contains_key(k), "missing header: {}", k);
        }
    }

    #[test]
    fn static_values_match_zcode() {
        let h = build_zcode_headers(&default_cfg());
        assert_eq!(h["User-Agent"], "ZCode/3.1.0");
        assert_eq!(h["X-ZCode-App-Version"], "3.1.0");
        assert_eq!(h["X-ZCode-Agent"], "glm");
        assert_eq!(h["X-Title"], "Z Code@electron");
        assert_eq!(h["HTTP-Referer"], "https://zcode.z.ai");
    }

    #[test]
    fn request_ids_are_uuid() {
        let h = build_zcode_headers(&default_cfg());
        for k in &["x-request-id", "x-zcode-trace-id", "x-query-id"] {
            let v = &h[*k];
            assert!(Uuid::parse_str(v).is_ok(), "{} not uuid: {}", k, v);
        }
    }

    #[test]
    fn request_ids_unique_per_call() {
        let h1 = build_zcode_headers(&default_cfg());
        let h2 = build_zcode_headers(&default_cfg());
        assert_ne!(h1["x-request-id"], h2["x-request-id"]);
        assert_ne!(h1["x-zcode-trace-id"], h2["x-zcode-trace-id"]);
        assert_ne!(h1["x-query-id"], h2["x-query-id"]);
    }

    #[test]
    fn session_id_stable_when_configured() {
        let cfg = FakeCfg {
            session_id: "11111111-2222-3333-4444-555555555555".into(),
        };
        let h = build_zcode_headers(&cfg);
        assert_eq!(h["x-session-id"], "11111111-2222-3333-4444-555555555555");
    }

    #[test]
    fn session_id_generated_when_empty() {
        let h = build_zcode_headers(&default_cfg());
        assert!(Uuid::parse_str(&h["x-session-id"]).is_ok());
    }

    #[test]
    fn no_anthropic_headers_injected() {
        let h = build_zcode_headers(&default_cfg());
        assert!(!h.contains_key("anthropic-version"));
        assert!(!h.contains_key("anthropic-beta"));
    }
}
