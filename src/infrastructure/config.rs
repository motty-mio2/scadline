use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const DEFAULT_CACHE_SIZE_MB: u64 = 256;

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct AppConfig {
    pub(crate) cache: CacheConfig,
}

#[derive(Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct CacheConfig {
    pub(crate) max_size_mb: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_size_mb: DEFAULT_CACHE_SIZE_MB,
        }
    }
}

fn config_file_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .map(|home| home.join(".config").join("scadline").join("config.toml"))
}

pub(crate) fn load_or_create_config() -> AppConfig {
    let Some(path) = config_file_path() else {
        return AppConfig::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(contents) => toml::from_str(&contents).unwrap_or_else(|error| {
            eprintln!("{} の解析に失敗しました: {error}", path.display());
            AppConfig::default()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let config = AppConfig::default();
            if let Some(parent) = path.parent()
                && std::fs::create_dir_all(parent).is_ok()
                && let Ok(contents) = toml::to_string_pretty(&config)
            {
                let _ = std::fs::write(path, contents);
            }
            config
        }
        Err(error) => {
            eprintln!("{} を読み込めません: {error}", path.display());
            AppConfig::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_uses_default_cache_limit_when_omitted() {
        let config: AppConfig = toml::from_str("").expect("empty config should use defaults");
        assert_eq!(config.cache.max_size_mb, DEFAULT_CACHE_SIZE_MB);
    }
}
