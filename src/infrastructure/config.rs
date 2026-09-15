use std::{fmt, path::PathBuf, str::FromStr};

use serde::{Deserialize, Serialize};

const DEFAULT_CACHE_SIZE_MB: u64 = 256;

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct AppConfig {
    pub(crate) cache: CacheConfig,
    pub(crate) openscad: OpenScadConfig,
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

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct OpenScadConfig {
    /// Leave this unset to use the installed OpenSCAD with its default geometry backend.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) backend: Option<ModelBackend>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ModelBackend {
    Cgal,
    Manifold,
    Openrscad,
}

impl ModelBackend {
    pub(crate) fn openscad_backend(self) -> Option<&'static str> {
        match self {
            Self::Cgal => Some("CGAL"),
            Self::Manifold => Some("Manifold"),
            Self::Openrscad => None,
        }
    }

    pub(crate) fn cache_key(self) -> &'static str {
        match self {
            Self::Cgal => "cgal",
            Self::Manifold => "manifold",
            Self::Openrscad => "openrscad",
        }
    }
}

impl fmt::Display for ModelBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.cache_key())
    }
}

impl FromStr for ModelBackend {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "cgal" => Ok(Self::Cgal),
            "manifold" => Ok(Self::Manifold),
            "openrscad" => Ok(Self::Openrscad),
            _ => Err(format!(
                "不明なモデルバックエンドです: {value} (cgal、manifold、openrscad のいずれかを指定してください)"
            )),
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
        assert_eq!(config.openscad.backend, None);
    }

    #[test]
    fn config_accepts_manifold_backend() {
        let config: AppConfig =
            toml::from_str("[openscad]\nbackend = \"manifold\"\n").expect("backend should parse");
        assert_eq!(config.openscad.backend, Some(ModelBackend::Manifold));
    }

    #[test]
    fn default_config_can_be_written_without_an_optional_backend() {
        let contents = toml::to_string_pretty(&AppConfig::default()).expect("serialize config");
        assert!(!contents.contains("backend"));
    }
}
