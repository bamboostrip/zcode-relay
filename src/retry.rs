//! 重试退避逻辑（对齐 ZCode 的 Anthropic SDK 策略）。
//!
//! ZCode 源码中的重试配置：
//!   retryWithExponentialBackoffRespectingRetryHeaders:
//!     maxRetries=2, initialDelayInMs=2000, backoffFactor=2
//! relay 把 maxRetries 提升到 10（对齐 ZCode UI「重新连接中，10 次机会」），
//! 退避公式保持一致：每次 delay *= factor，封顶 max_delay，尊重 Retry-After 头。

use rand::Rng;

/// 可重试的 HTTP 状态码（瞬时错误）。
pub const RETRYABLE_STATUS: &[u16] = &[429, 500, 502, 503, 504];

/// 判断给定 HTTP 状态码是否应重试。
pub fn should_retry(status: u16) -> bool {
    RETRYABLE_STATUS.contains(&status)
}

/// 计算第 `attempt` 次重试前的退避秒数。
///
/// - `initial`: 初始退避（默认 2s）
/// - `factor`: 指数因子（默认 2）
/// - `max_delay`: 退避上限（默认 20s）
/// - `jitter`: 在 base 上叠加 [0, jitter] 随机量，避免重试风暴（默认 1s）
/// - `retry_after`: 上游 Retry-After 头（秒），取 max(计算值, retry_after)
pub fn compute_backoff(
    attempt: u32,
    initial: f64,
    factor: f64,
    max_delay: f64,
    jitter: f64,
    retry_after: Option<f64>,
) -> f64 {
    let mut base = initial * factor.powi((attempt - 1) as i32);
    base = base.min(max_delay);
    if jitter > 0.0 {
        base += rand::rng().random_range(0.0..jitter);
    }
    if let Some(ra) = retry_after {
        base = base.max(ra);
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_retry_transients() {
        for &s in &[429u16, 500, 502, 503, 504] {
            assert!(should_retry(s), "{} should retry", s);
        }
    }

    #[test]
    fn should_not_retry_non_transients() {
        for &s in &[200u16, 400, 401, 403, 404] {
            assert!(!should_retry(s), "{} should not retry", s);
        }
    }

    #[test]
    fn backoff_first_attempt_is_initial() {
        let d = compute_backoff(1, 2.0, 2.0, 20.0, 0.0, None);
        assert!((d - 2.0).abs() < 0.001);
    }

    #[test]
    fn backoff_grows_exponentially() {
        let delays: Vec<f64> = (1..=4)
            .map(|a| compute_backoff(a, 2.0, 2.0, 20.0, 0.0, None))
            .collect();
        assert!((delays[0] - 2.0).abs() < 0.001);
        assert!((delays[1] - 4.0).abs() < 0.001);
        assert!((delays[2] - 8.0).abs() < 0.001);
        assert!((delays[3] - 16.0).abs() < 0.001);
    }

    #[test]
    fn backoff_capped_at_max() {
        // attempt=5 本应是 32s，被 max=20 截断
        let d = compute_backoff(5, 2.0, 2.0, 20.0, 0.0, None);
        assert!((d - 20.0).abs() < 0.001);
    }

    #[test]
    fn backoff_with_jitter_in_range() {
        for _ in 0..100 {
            let d = compute_backoff(2, 2.0, 2.0, 20.0, 1.0, None);
            assert!(d >= 4.0 && d <= 5.0, "jitter out of range: {}", d);
        }
    }

    #[test]
    fn backoff_respects_retry_after() {
        let d = compute_backoff(1, 2.0, 2.0, 20.0, 0.0, Some(15.0));
        assert!((d - 15.0).abs() < 0.001);
    }
}
