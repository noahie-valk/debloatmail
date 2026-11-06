//! Path discovery and configuration management for `debloatmailctl`.
//!
//! Per `docs/debloatmailctl.md` §4:
//! - Config: `$XDG_CONFIG_HOME/debloatmail/config.toml`
//! - Database: `$XDG_DATA_HOME/debloatmail/mail.db` (+ `-wal`, `-shm`)
//! - Blobs: `$XDG_CACHE_HOME/debloatmail/blobs/<ab>/<hash>`
//! - Standalone lock: `$XDG_STATE_HOME/debloatmail/debloatmail-standalone.lock`

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value as TomlValue;

use crate::error::CliError;

/// Resolved system and application paths.
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_paths: Vec<PathBuf>,
    pub db_path: PathBuf,
    pub blobs_dir: PathBuf,
    pub state_dir: PathBuf,
    pub lock_path: PathBuf,
}

impl AppPaths {
    /// Resolves paths honoring CLI overrides and standard XDG directory conventions.
    pub fn resolve(cli_config: Option<&[String]>, cli_db: Option<&Path>) -> Result<Self, CliError> {
        let home = env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"));

        let config_home = env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".config"));

        let data_home = env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".local/share"));

        let cache_home = env::var("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".cache"));

        let state_home = env::var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".local/state"));

        let mut config_paths = Vec::new();
        if let Some(configs) = cli_config {
            for c_arg in configs {
                for part in c_arg.split(':') {
                    if !part.trim().is_empty() {
                        config_paths.push(PathBuf::from(part.trim()));
                    }
                }
            }
        }
        if config_paths.is_empty() {
            config_paths.push(config_home.join("debloatmail/config.toml"));
        }

        let db_path = if let Some(override_path) = cli_db {
            override_path.to_path_buf()
        } else {
            data_home.join("debloatmail/mail.db")
        };

        let blobs_dir = cache_home.join("debloatmail/blobs");
        let state_dir = state_home.join("debloatmail");
        let lock_path = state_dir.join("debloatmail-standalone.lock");

        Ok(Self {
            config_paths,
            db_path,
            blobs_dir,
            state_dir,
            lock_path,
        })
    }
}

/// Application preferences loaded from `config.toml`.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub raw: TomlValue,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            raw: TomlValue::Table(toml::map::Map::new()),
        }
    }
}

impl AppConfig {
    /// Deep-merges multiple configuration files left-to-right.
    pub fn load_merged(paths: &[PathBuf]) -> Result<Self, CliError> {
        let mut merged = TomlValue::Table(toml::map::Map::new());

        for path in paths {
            if path.exists() {
                let content = fs::read_to_string(path).map_err(|e| {
                    CliError::Internal(format!("failed to read config at {}: {e}", path.display()))
                })?;
                let val: TomlValue = toml::from_str(&content).map_err(|e| {
                    CliError::Usage(format!("syntax error in config {}: {e}", path.display()))
                })?;
                deep_merge_toml(&mut merged, val);
            }
        }

        Ok(Self { raw: merged })
    }

    /// Gets a value by dotted key path (e.g. `sync.poll_interval`).
    pub fn get(&self, key: &str) -> Option<&TomlValue> {
        let mut curr = &self.raw;
        for part in key.split('.') {
            match curr {
                TomlValue::Table(table) => {
                    curr = table.get(part)?;
                }
                _ => return None,
            }
        }
        Some(curr)
    }

    /// Sets a dotted key value and persists it to the primary configuration file.
    pub fn set_and_save(primary_path: &Path, key: &str, raw_val: &str) -> Result<(), CliError> {
        let mut existing_val = if primary_path.exists() {
            let content = fs::read_to_string(primary_path)?;
            toml::from_str(&content).unwrap_or_else(|_| TomlValue::Table(toml::map::Map::new()))
        } else {
            TomlValue::Table(toml::map::Map::new())
        };

        let parsed_val: TomlValue = toml::from_str(&format!("v = {raw_val}"))
            .map(|t| match t {
                TomlValue::Table(mut m) => m
                    .remove("v")
                    .unwrap_or(TomlValue::String(raw_val.to_string())),
                _ => TomlValue::String(raw_val.to_string()),
            })
            .unwrap_or_else(|_| TomlValue::String(raw_val.to_string()));

        let parts: Vec<&str> = key.split('.').collect();
        set_nested_key(&mut existing_val, &parts, parsed_val);

        if let Some(parent) = primary_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let serialized = toml::to_string_pretty(&existing_val)?;
        fs::write(primary_path, serialized)?;
        Ok(())
    }
}

fn deep_merge_toml(target: &mut TomlValue, source: TomlValue) {
    match (target, source) {
        (TomlValue::Table(t_map), TomlValue::Table(s_map)) => {
            for (k, v) in s_map {
                match t_map.get_mut(&k) {
                    Some(existing) => deep_merge_toml(existing, v),
                    None => {
                        t_map.insert(k, v);
                    }
                }
            }
        }
        (target, source) => {
            *target = source;
        }
    }
}

fn set_nested_key(curr: &mut TomlValue, parts: &[&str], value: TomlValue) {
    if parts.is_empty() {
        return;
    }
    if !curr.is_table() {
        *curr = TomlValue::Table(toml::map::Map::new());
    }

    if let TomlValue::Table(map) = curr {
        if parts.len() == 1 {
            map.insert(parts[0].to_string(), value);
        } else {
            let entry = map
                .entry(parts[0].to_string())
                .or_insert_with(|| TomlValue::Table(toml::map::Map::new()));
            set_nested_key(entry, &parts[1..], value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_deep_merge_toml() {
        let mut base: TomlValue = toml::from_str(
            r#"
            [ui]
            color = "auto"
            page_size = 25
            [sync]
            interval = 60
        "#,
        )
        .unwrap();

        let override_cfg: TomlValue = toml::from_str(
            r#"
            [ui]
            page_size = 50
            theme = "dark"
        "#,
        )
        .unwrap();

        deep_merge_toml(&mut base, override_cfg);

        let cfg = AppConfig { raw: base };
        assert_eq!(cfg.get("ui.page_size").unwrap().as_integer(), Some(50));
        assert_eq!(cfg.get("ui.color").unwrap().as_str(), Some("auto"));
        assert_eq!(cfg.get("ui.theme").unwrap().as_str(), Some("dark"));
        assert_eq!(cfg.get("sync.interval").unwrap().as_integer(), Some(60));
    }

    #[test]
    fn test_set_and_save() {
        let tmp = tempdir().unwrap();
        let cfg_path = tmp.path().join("config.toml");

        AppConfig::set_and_save(&cfg_path, "cache.cap_mb", "500").unwrap();
        let loaded = AppConfig::load_merged(std::slice::from_ref(&cfg_path)).unwrap();
        assert_eq!(loaded.get("cache.cap_mb").unwrap().as_integer(), Some(500));

        AppConfig::set_and_save(&cfg_path, "ui.theme", "\"solarized\"").unwrap();
        let loaded2 = AppConfig::load_merged(&[cfg_path]).unwrap();
        assert_eq!(loaded2.get("cache.cap_mb").unwrap().as_integer(), Some(500));
        assert_eq!(loaded2.get("ui.theme").unwrap().as_str(), Some("solarized"));
    }
}
