pub fn resolve_secret(env_value: Option<String>, config_value: Option<String>) -> Option<String> {
    normalize_secret(env_value).or_else(|| normalize_secret(config_value))
}

pub fn resolve_secret_from_env(env_key: &str, config_value: Option<String>) -> Option<String> {
    let env_value = std::env::var(env_key).ok();
    resolve_secret(env_value, config_value)
}

fn normalize_secret(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        if value.trim().is_empty() {
            None
        } else {
            Some(value)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvVarGuard {
        key: String,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }

            Self {
                key: key.to_string(),
                previous,
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(previous) => std::env::set_var(&self.key, previous),
                None => std::env::remove_var(&self.key),
            }
        }
    }

    #[test]
    fn env_value_wins_over_config_value() {
        let resolved = resolve_secret(
            Some("env-secret-token".to_string()),
            Some("config-secret-token".to_string()),
        );

        assert_eq!(resolved.as_deref(), Some("env-secret-token"));
    }

    #[test]
    fn env_value_is_used_when_config_is_missing() {
        let resolved = resolve_secret(Some("env-secret-token".to_string()), None);

        assert_eq!(resolved.as_deref(), Some("env-secret-token"));
    }

    #[test]
    fn empty_env_value_falls_back_to_config_value() {
        let resolved = resolve_secret(
            Some("   ".to_string()),
            Some("config-secret-token".to_string()),
        );

        assert_eq!(resolved.as_deref(), Some("config-secret-token"));
    }

    #[test]
    fn resolve_secret_from_env_prefers_environment() {
        let key = "MUNINN_TYPES_TEST_SECRET_FROM_ENV";
        let _guard = EnvVarGuard::set(key, Some("env-priority"));

        let resolved = resolve_secret_from_env(key, Some("config-fallback".to_string()));

        assert_eq!(resolved.as_deref(), Some("env-priority"));
    }

    #[test]
    fn resolve_secret_from_env_uses_config_when_env_missing() {
        let key = "MUNINN_TYPES_TEST_SECRET_MISSING";
        let _guard = EnvVarGuard::set(key, None);

        let resolved = resolve_secret_from_env(key, Some("config-fallback".to_string()));

        assert_eq!(resolved.as_deref(), Some("config-fallback"));
    }
}
