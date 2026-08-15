use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub allowances: Allowances,
    pub commands: Commands,
    pub paths: Paths,
    /// How often the dashboard rereads Codex usage from the App Server.
    pub refresh_interval_secs: u64,
    /// How often the dashboard asks Claude Code for account-wide usage,
    /// covering the desktop app, IDE extensions, and other clients that never
    /// run the status line. Matches the CLI's own internal cache lifetime, so
    /// asking more often would not return fresher numbers.
    pub claude_refresh_interval_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            allowances: Allowances::default(),
            commands: Commands::default(),
            paths: Paths::default(),
            refresh_interval_secs: 60,
            claude_refresh_interval_secs: 300,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Allowances {
    /// Legacy single-window configuration. Prefer `opencode_go`.
    pub opencode_monthly_usd: Option<f64>,
    pub opencode_go: Option<OpenCodeGoAllowances>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct OpenCodeGoAllowances {
    pub five_hour_usd: f64,
    pub weekly_usd: f64,
    pub monthly_usd: f64,
}

impl Default for OpenCodeGoAllowances {
    fn default() -> Self {
        Self {
            five_hour_usd: 12.0,
            weekly_usd: 30.0,
            monthly_usd: 60.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Commands {
    pub codex: PathBuf,
    pub claude: PathBuf,
}

impl Default for Commands {
    fn default() -> Self {
        Self {
            codex: PathBuf::from("codex"),
            claude: PathBuf::from("claude"),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Paths {
    pub opencode_db: PathBuf,
    pub claude_usage_cache: PathBuf,
    /// Claude Code's own `~/.claude.json`, read for the `cachedUsageUtilization`
    /// snapshot it keeps regardless of which client last talked to the API.
    pub claude_config_json: PathBuf,
    /// The Claude desktop app's own usage history, the only source that
    /// reflects that app's usage without asking `claude` to be installed.
    pub claude_usage_history: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self {
            opencode_db: home.join(".local/share/opencode/opencode.db"),
            claude_usage_cache: dirs::data_local_dir()
                .unwrap_or_else(|| home.join(".local/share"))
                .join("costsee/claude-usage.json"),
            claude_config_json: home.join(".claude.json"),
            claude_usage_history: dirs::config_dir()
                .unwrap_or_else(|| home.join(".config"))
                .join("Claude/plan-usage-history.json"),
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self::path_from_home(&home)
    }

    fn path_from_home(home: &std::path::Path) -> PathBuf {
        home.join(".config/costsee/config.toml")
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
        let raw = toml::to_string_pretty(&Self::default())?
            .replacen(
                "[allowances]\n",
                "[allowances]\n\
                 # Uncomment the following section to use OpenCode Go.\n\
                 # [allowances.opencode_go]\n\
                 # five_hour_usd = 12.0\n\
                 # weekly_usd = 30.0\n\
                 # monthly_usd = 60.0\n",
                1,
            )
            .replacen(
                "refresh_interval_secs = 60\n",
                "# How often costsee rereads Codex usage from the App Server.\nrefresh_interval_secs = 60\n",
                1,
            )
            .replacen(
                "claude_refresh_interval_secs = 300\n",
                "# How often costsee asks Claude Code for account-wide usage, covering\n\
                 # clients besides this one (the desktop app, IDE extensions, etc).\n\
                 claude_refresh_interval_secs = 300\n",
                1,
            );
        fs::write(&path, raw)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn default_config_is_under_dot_config_on_every_platform() {
        assert_eq!(
            Config::path_from_home(Path::new("/home/alice")),
            PathBuf::from("/home/alice/.config/costsee/config.toml")
        );
    }

    #[test]
    fn default_refresh_interval_is_one_minute() {
        assert_eq!(Config::default().refresh_interval_secs, 60);
    }

    #[test]
    fn a_custom_refresh_interval_survives_a_round_trip() {
        let config = Config {
            refresh_interval_secs: 30,
            ..Config::default()
        };
        let raw = toml::to_string(&config).unwrap();
        let parsed: Config = toml::from_str(&raw).unwrap();
        assert_eq!(parsed.refresh_interval_secs, 30);
    }
}
