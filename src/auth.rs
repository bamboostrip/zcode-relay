//! 管理鉴权：校验外部调用方带来的 management key。
//!
//! key 可放在 Authorization: Bearer xxx 或 x-api-key 头里（Authorization 优先）。
//! 未配置 expected 时放行所有请求（不鉴权）。

/// 返回是否通过鉴权。
///
/// - `authorization`: 请求的 Authorization 头值（可能 "Bearer xxx"）
/// - `x_api_key`: 请求的 x-api-key 头值
/// - `expected`: 配置的 management_key；为空则放行
pub fn check_auth(authorization: Option<&str>, x_api_key: Option<&str>, expected: &str) -> bool {
    if expected.is_empty() {
        return true; // 未配置 → 不鉴权
    }

    // Authorization 优先
    let mut provided: Option<String> = None;
    if let Some(auth) = authorization {
        let val = auth.trim();
        let lower = val.to_lowercase();
        if lower.starts_with("bearer ") {
            provided = Some(val[7..].trim().to_string());
        } else if !val.is_empty() {
            provided = Some(val.to_string());
        }
    }
    // 回退到 x-api-key
    if provided.is_none() {
        if let Some(k) = x_api_key {
            let k = k.trim();
            if !k.is_empty() {
                provided = Some(k.to_string());
            }
        }
    }

    match provided {
        Some(p) => p == expected,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_expected_allows_all() {
        assert!(check_auth(None, None, ""));
        assert!(check_auth(Some("anything"), None, ""));
    }

    #[test]
    fn bearer_token_accepted() {
        assert!(check_auth(Some("Bearer secret123"), None, "secret123"));
    }

    #[test]
    fn bearer_case_insensitive_scheme() {
        assert!(check_auth(Some("bearer secret123"), None, "secret123"));
    }

    #[test]
    fn x_api_key_accepted() {
        assert!(check_auth(None, Some("secret123"), "secret123"));
    }

    #[test]
    fn wrong_key_rejected() {
        assert!(!check_auth(Some("Bearer wrong"), None, "secret123"));
    }

    #[test]
    fn missing_credentials_rejected() {
        assert!(!check_auth(None, None, "secret123"));
    }

    #[test]
    fn bearer_takes_precedence() {
        // Authorization 正确，x-api-key 错误 → 放行
        assert!(check_auth(
            Some("Bearer secret123"),
            Some("wrong"),
            "secret123"
        ));
    }

    #[test]
    fn whitespace_trimmed() {
        assert!(check_auth(Some("Bearer  secret123  "), None, "secret123"));
    }
}
