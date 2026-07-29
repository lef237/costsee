use crate::{
    config::Config,
    model::{LimitWindow, QuotaSnapshot, QuotaSummary, Tool},
};
use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Local, TimeZone};
use rusqlite::Connection;
use serde_json::Value;
use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};
use walkdir::WalkDir;

pub fn collect_quotas(config: &Config) -> QuotaSnapshot {
    let mut snapshot = QuotaSnapshot::default();
    snapshot.tools.insert(
        Tool::Codex,
        codex_quota(&config.paths.codex).unwrap_or_else(error_summary),
    );
    snapshot.tools.insert(
        Tool::Claude,
        claude_quota(&config.paths.claude_usage_cache).unwrap_or_else(error_summary),
    );
    snapshot.tools.insert(
        Tool::OpenCode,
        opencode_quota(config).unwrap_or_else(error_summary),
    );
    snapshot
}

fn error_summary(err: anyhow::Error) -> QuotaSummary {
    QuotaSummary {
        note: Some(format!("{err:#}")),
        ..Default::default()
    }
}

fn jsonl_files(root: &Path) -> Result<Vec<std::path::PathBuf>> {
    if !root.exists() {
        anyhow::bail!("not found: {}", root.display());
    }
    Ok(WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file()
                && entry.path().extension().is_some_and(|ext| ext == "jsonl")
        })
        .map(|entry| entry.into_path())
        .collect())
}

fn parse_time(value: &Value) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|at| at.with_timezone(&Local))
}

fn window_label(minutes: u64) -> String {
    match minutes {
        300 => "5 hours".into(),
        10_080 => "7 days".into(),
        value if value % 1_440 == 0 => format!("{} days", value / 1_440),
        value if value % 60 == 0 => format!("{} hours", value / 60),
        value => format!("{value} min"),
    }
}

pub fn codex_quota(root: &Path) -> Result<QuotaSummary> {
    let mut latest: Option<(DateTime<Local>, Value)> = None;
    for path in jsonl_files(root)? {
        for line in BufReader::new(File::open(path)?)
            .lines()
            .map_while(Result::ok)
        {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if value["type"] != "event_msg"
                || value["payload"]["type"] != "token_count"
                || value["payload"]["rate_limits"].is_null()
            {
                continue;
            }
            let Some(at) = parse_time(&value["timestamp"]) else {
                continue;
            };
            if latest.as_ref().is_none_or(|(current, _)| at > *current) {
                latest = Some((at, value["payload"]["rate_limits"].clone()));
            }
        }
    }
    let Some((updated_at, limits)) = latest else {
        anyhow::bail!("rate-limit data has not been recorded yet");
    };
    let mut windows = Vec::new();
    for key in ["primary", "secondary", "individual_limit"] {
        let value = &limits[key];
        let Some(percent) = value["used_percent"].as_f64() else {
            continue;
        };
        let minutes = value["window_minutes"].as_u64().unwrap_or(0);
        windows.push(LimitWindow {
            label: window_label(minutes),
            used_percent: percent,
            resets_at: value["resets_at"]
                .as_i64()
                .and_then(|seconds| Local.timestamp_opt(seconds, 0).single()),
            detail: "reported by Codex".into(),
        });
    }
    if windows.is_empty() {
        anyhow::bail!("Codex reported no active allowance windows");
    }
    Ok(QuotaSummary {
        windows,
        plan: limits["plan_type"].as_str().map(str::to_owned),
        note: None,
        updated_at: Some(updated_at),
    })
}

pub fn claude_quota(cache: &Path) -> Result<QuotaSummary> {
    if !cache.exists() {
        anyhow::bail!("run `costsee setup` to connect Claude Code");
    }
    let raw = std::fs::read_to_string(cache)?;
    let value: Value = serde_json::from_str(&raw)?;
    let mut windows = Vec::new();
    for (key, label) in [("five_hour", "5 hours"), ("seven_day", "7 days")] {
        let rate_limit = &value["rate_limits"][key];
        let Some(percent) = rate_limit["used_percentage"].as_f64() else {
            continue;
        };
        windows.push(LimitWindow {
            label: label.into(),
            used_percent: percent,
            resets_at: rate_limit["resets_at"]
                .as_i64()
                .and_then(|seconds| Local.timestamp_opt(seconds, 0).single()),
            detail: "reported by Claude Code".into(),
        });
    }
    if windows.is_empty() {
        anyhow::bail!("Claude Code has not reported subscription limits yet");
    }
    Ok(QuotaSummary {
        windows,
        plan: None,
        note: None,
        updated_at: std::fs::metadata(cache)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .map(DateTime::<Local>::from),
    })
}

pub fn opencode_quota(config: &Config) -> Result<QuotaSummary> {
    let go_limits = config.allowances.opencode_go.as_ref();
    let legacy_monthly = config.allowances.opencode_monthly_usd;
    if go_limits.is_none() && legacy_monthly.is_none() {
        anyhow::bail!("set `allowances.opencode_go` in config");
    }
    let database = &config.paths.opencode_db;
    if !database.exists() {
        anyhow::bail!("not found: {}", database.display());
    }
    let connection =
        Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("failed to open {}", database.display()))?;
    let now = Local::now();
    let month_start = Local
        .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .context("invalid month start")?;
    let (year, month) = if now.month() == 12 {
        (now.year() + 1, 1)
    } else {
        (now.year(), now.month() + 1)
    };
    let month_reset = Local.with_ymd_and_hms(year, month, 1, 0, 0, 0).single();
    let mut windows = Vec::new();

    if let Some(limits) = go_limits {
        validate_go_limits(limits)?;
        windows.push(opencode_window(
            &connection,
            "5 hours",
            now - chrono::Duration::hours(5),
            limits.five_hour_usd,
            None,
        )?);
        windows.push(opencode_window(
            &connection,
            "7 days",
            now - chrono::Duration::days(7),
            limits.weekly_usd,
            None,
        )?);
        windows.push(opencode_window(
            &connection,
            "This month",
            month_start,
            limits.monthly_usd,
            month_reset,
        )?);
    } else if let Some(limit) = legacy_monthly {
        if limit <= 0.0 {
            anyhow::bail!("OpenCode monthly allowance must be greater than zero");
        }
        windows.push(opencode_window(
            &connection,
            "This month",
            month_start,
            limit,
            month_reset,
        )?);
    }

    Ok(QuotaSummary {
        windows,
        plan: Some(if go_limits.is_some() {
            "Go · local estimate".into()
        } else {
            "configured limit".into()
        }),
        note: None,
        updated_at: Some(now),
    })
}

fn validate_go_limits(limits: &crate::config::OpenCodeGoAllowances) -> Result<()> {
    if limits.five_hour_usd <= 0.0 || limits.weekly_usd <= 0.0 || limits.monthly_usd <= 0.0 {
        anyhow::bail!("all OpenCode Go allowances must be greater than zero");
    }
    Ok(())
}

fn opencode_window(
    connection: &Connection,
    label: &str,
    start: DateTime<Local>,
    limit: f64,
    resets_at: Option<DateTime<Local>>,
) -> Result<LimitWindow> {
    let spent: f64 = connection.query_row(
        "SELECT COALESCE(SUM(CAST(json_extract(data, '$.cost') AS REAL)), 0)
         FROM message
         WHERE time_created >= ?1
           AND json_extract(data, '$.providerID') = 'opencode-go'",
        [start.timestamp_millis()],
        |row| row.get(0),
    )?;
    Ok(LimitWindow {
        label: label.into(),
        used_percent: spent / limit * 100.0,
        resets_at,
        detail: format!("${spent:.2} of ${limit:.2} · local"),
    })
}

pub fn capture_claude(input: &str, cache: &Path) -> Result<Vec<LimitWindow>> {
    let value: Value = serde_json::from_str(input).context("invalid Claude status-line JSON")?;
    if let Some(parent) = cache.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = cache.with_extension("tmp");
    std::fs::write(&temporary, serde_json::to_vec(&value)?)?;
    std::fs::rename(temporary, cache)?;
    Ok(claude_quota(cache)?.windows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OpenCodeGoAllowances;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn reads_latest_codex_allowance() {
        let directory = tempdir().unwrap();
        let mut file = File::create(directory.path().join("session.jsonl")).unwrap();
        writeln!(file, r#"{{"timestamp":"2026-07-01T00:00:00Z","type":"event_msg","payload":{{"type":"token_count","rate_limits":{{"plan_type":"plus","primary":{{"used_percent":42.0,"window_minutes":300,"resets_at":1782867600}}}}}}}}"#).unwrap();
        let summary = codex_quota(directory.path()).unwrap();
        assert_eq!(summary.windows[0].label, "5 hours");
        assert_eq!(summary.windows[0].used_percent, 42.0);
        assert_eq!(summary.plan.as_deref(), Some("plus"));
    }

    #[test]
    fn captures_claude_status_line_data() {
        let directory = tempdir().unwrap();
        let cache = directory.path().join("claude.json");
        let input = r#"{"rate_limits":{"five_hour":{"used_percentage":23.5,"resets_at":1782867600},"seven_day":{"used_percentage":41.2,"resets_at":1783126800}}}"#;
        let windows = capture_claude(input, &cache).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[1].used_percent, 41.2);
    }

    #[test]
    fn opencode_go_has_three_windows_and_ignores_other_providers() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("opencode.db");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute(
                "CREATE TABLE message (
                    id TEXT PRIMARY KEY,
                    time_created INTEGER NOT NULL,
                    data TEXT NOT NULL
                )",
                [],
            )
            .unwrap();
        let now = Local::now().timestamp_millis();
        connection
            .execute(
                "INSERT INTO message VALUES
                 ('go', ?1, '{\"providerID\":\"opencode-go\",\"cost\":6.0}'),
                 ('other', ?1, '{\"providerID\":\"openai\",\"cost\":100.0}')",
                [now],
            )
            .unwrap();
        drop(connection);

        let mut config = Config::default();
        config.paths.opencode_db = database;
        config.allowances.opencode_go = Some(OpenCodeGoAllowances::default());
        let summary = opencode_quota(&config).unwrap();

        assert_eq!(summary.windows.len(), 3);
        assert_eq!(summary.windows[0].used_percent, 50.0);
        assert_eq!(summary.windows[1].used_percent, 20.0);
        assert_eq!(summary.windows[2].used_percent, 10.0);
    }
}
