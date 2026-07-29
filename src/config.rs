use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub allowances: Allowances,
    pub paths: Paths,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Allowances {
    /// OpenCode has no provider-independent quota API. Set this to your
    /// OpenCode Zen/workspace monthly USD limit to enable its percentage.
    pub opencode_monthly_usd: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Paths {
    pub codex: PathBuf,
    pub opencode_db: PathBuf,
    pub claude_usage_cache: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self {
            codex: home.join(".codex/sessions"),
            opencode_db: home.join(".local/share/opencode/opencode.db"),
            claude_usage_cache: dirs::data_local_dir()
                .unwrap_or_else(|| home.join(".local/share"))
                .join("costsee/claude-usage.json"),
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("costsee/config.toml")
    }

    pub fn load(path: Option<PathBuf>) -> Result<(Self, PathBuf)> {
        let path = path.unwrap_or_else(Self::path);
        if !path.exists() {
            return Ok((Self::default(), path));
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let cfg =
            toml::from_str(&raw).with_context(|| format!("invalid config {}", path.display()))?;
        Ok((cfg, path))
    }

    pub fn init(path: Option<PathBuf>) -> Result<PathBuf> {
        let path = path.unwrap_or_else(Self::path);
        if path.exists() {
            anyhow::bail!("config already exists: {}", path.display());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let raw = toml::to_string_pretty(&Self::default())?.replacen(
            "[allowances]\n",
            "[allowances]\n\
             # OpenCode Zen/workspace の月間上限を設定すると利用率を表示します。\n\
             # opencode_monthly_usd = 20.0\n",
            1,
        );
        fs::write(&path, raw)?;
        Ok(path)
    }
}
