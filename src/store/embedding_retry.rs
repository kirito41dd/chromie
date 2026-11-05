//! 临时故障持续退避；配置错误等待有效配置保存，避免无效调用消耗额度。
pub(super) const RETRYABLE_ERRORS: &[&str] = &[
    "model_busy",
    "model_unreachable",
    "model_rate_limited",
    "model_server_error",
    "unavailable",
    "storage_error",
];

pub(super) enum Recovery {
    Automatic(time::Duration),
    ConfigurationRequired,
}

impl Recovery {
    pub(super) fn after_failure(code: &str, attempts: i32) -> Self {
        if RETRYABLE_ERRORS.contains(&code) {
            let exponent = attempts.clamp(1, 8) as u32;
            Self::Automatic(time::Duration::seconds(((1i64 << exponent) * 2).min(300)))
        } else {
            Self::ConfigurationRequired
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn temporary_failures_keep_retrying_with_a_bounded_interval() {
        for code in RETRYABLE_ERRORS {
            for attempts in [1, 5, 6, 200, i32::MAX] {
                let Recovery::Automatic(delay) = Recovery::after_failure(code, attempts) else {
                    panic!("临时故障不能因次数耗尽而停止");
                };
                assert!((4..=300).contains(&delay.whole_seconds()));
            }
        }
        assert!(matches!(
            Recovery::after_failure("model_auth", 1),
            Recovery::ConfigurationRequired
        ));
        assert!(matches!(
            Recovery::after_failure("model_dimensions", 1),
            Recovery::ConfigurationRequired
        ));
        assert!(matches!(
            Recovery::after_failure("model_response", 1),
            Recovery::ConfigurationRequired
        ));
    }
}
