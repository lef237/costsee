use crate::{
    config::Config,
    model::{LimitWindow, QuotaSnapshot, QuotaSummary, Tool},
};
use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Local, TimeZone};
use rusqlite::Connection;
use serde_json::{Map, Value};
use std::{
    fs::OpenOptions,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};
use tempfile::NamedTempFile;

pub fn collect_quotas(config: &Config) -> QuotaSnapshot {
    let mut snapshot = QuotaSnapshot::default();
    snapshot.tools.insert(
        Tool::Codex,
        codex_quota(&config.commands.codex).unwrap_or_else(error_summary),
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

/// Reloads Claude Code alone, for the refresh its cache triggers by itself.
/// Collecting every tool would start the Codex app server, which answers in
/// seconds, and the dashboard writes that cache on every Claude Code message.
pub fn refresh_claude(config: &Config, snapshot: &mut QuotaSnapshot) {
    snapshot.tools.insert(
        Tool::Claude,
        claude_quota(&config.paths.claude_usage_cache).unwrap_or_else(error_summary),
    );
}

/// Collects Codex alone, folding any failure into its summary. The periodic
/// auto-refresh runs this in the background so the dashboard keeps answering
/// keys while the App Server starts.
pub fn collect_codex(config: &Config) -> QuotaSummary {
    codex_quota(&config.commands.codex).unwrap_or_else(error_summary)
}

fn error_summary(err: anyhow::Error) -> QuotaSummary {
    QuotaSummary {
        note: Some(format!("{err:#}")),
        ..Default::default()
    }
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

pub fn codex_quota(executable: &Path) -> Result<QuotaSummary> {
    let response = codex_rate_limits(executable)?;
    codex_summary(&response, Local::now())
}

fn codex_rate_limits(executable: &Path) -> Result<Value> {
    let mut child = ChildGuard(
        Command::new(executable)
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to start {}", executable.display()))?,
    );
    let stdout = child
        .0
        .stdout
        .take()
        .context("Codex stdout is unavailable")?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                let _ = sender.send(value);
            }
        }
    });
    let stdin = child
        .0
        .stdin
        .as_mut()
        .context("Codex stdin is unavailable")?;
    let initialize = serde_json::json!({
        "method": "initialize",
        "id": 0,
        "params": {
            "clientInfo": {
                "name": "costsee",
                "title": "costsee",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    });
    send_json_line(stdin, &initialize)?;
    receive_rpc_result(&receiver, 0)?;
    for request in [
        serde_json::json!({"method": "initialized", "params": {}}),
        serde_json::json!({"method": "account/rateLimits/read", "id": 1}),
    ] {
        send_json_line(stdin, &request)?;
    }
    receive_rpc_result(&receiver, 1)
}

fn send_json_line(writer: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn receive_rpc_result(receiver: &mpsc::Receiver<Value>, id: i64) -> Result<Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let response = receiver
            .recv_timeout(remaining)
            .context("timed out waiting for Codex rate limits")?;
        if response["id"] != id {
            continue;
        }
        if !response["error"].is_null() {
            anyhow::bail!("Codex App Server error: {}", response["error"]);
        }
        return response
            .get("result")
            .cloned()
            .context("Codex returned no rate-limit result");
    }
}

fn codex_summary(response: &Value, updated_at: DateTime<Local>) -> Result<QuotaSummary> {
    let limits = response["rateLimitsByLimitId"]["codex"]
        .as_object()
        .map(|_| &response["rateLimitsByLimitId"]["codex"])
        .unwrap_or(&response["rateLimits"]);
    let mut windows = Vec::new();
    for key in ["primary", "secondary"] {
        let value = &limits[key];
        let Some(percent) = value["usedPercent"].as_f64() else {
            continue;
        };
        let minutes = value["windowDurationMins"].as_u64().unwrap_or(0);
        windows.push(LimitWindow {
            label: window_label(minutes),
            used_percent: percent,
            resets_at: value["resetsAt"]
                .as_i64()
                .and_then(|seconds| Local.timestamp_opt(seconds, 0).single()),
            detail: "Codex App Server".into(),
        });
    }
    if let Some(remaining) = limits["individualLimit"]["remainingPercent"].as_f64() {
        windows.push(LimitWindow {
            label: "spend control".into(),
            used_percent: 100.0 - remaining,
            resets_at: limits["individualLimit"]["resetsAt"]
                .as_i64()
                .and_then(|seconds| Local.timestamp_opt(seconds, 0).single()),
            detail: "Codex App Server".into(),
        });
    }
    if windows.is_empty() {
        anyhow::bail!("Codex reported no active allowance windows");
    }
    Ok(QuotaSummary {
        windows,
        plan: limits["planType"].as_str().map(str::to_owned),
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
    let now = Local::now().timestamp();
    for (key, label) in [("five_hour", "5 hours"), ("seven_day", "7 days")] {
        let rate_limit = &value["rate_limits"][key];
        let Some((percent, resets_at)) = valid_claude_window(rate_limit, now) else {
            continue;
        };
        windows.push(LimitWindow {
            label: label.into(),
            used_percent: percent,
            resets_at: Local.timestamp_opt(resets_at, 0).single(),
            detail: "reported by Claude Code".into(),
        });
    }
    if windows.is_empty() {
        if value["rate_limits"]
            .as_object()
            .is_some_and(|limits| !limits.is_empty())
        {
            anyhow::bail!(
                "Claude Code allowance data is expired; waiting for a current API response"
            );
        }
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
    validate_opencode_database(&connection)
        .context("unsupported OpenCode database format; update costsee")?;
    let now = Local::now();
    // A daylight-saving change can make local midnight ambiguous or skip it, so
    // take the first matching instant rather than insisting on a unique one.
    let month_start = month_boundary(now.year(), now.month()).context("invalid month start")?;
    let (year, month) = if now.month() == 12 {
        (now.year() + 1, 1)
    } else {
        (now.year(), now.month() + 1)
    };
    let month_reset = month_boundary(year, month);
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

fn month_boundary(year: i32, month: u32) -> Option<DateTime<Local>> {
    Local
        .with_ymd_and_hms(year, month, 1, 0, 0, 0)
        .earliest()
        .or_else(|| {
            // Midnight itself does not exist in this zone; the window starts as
            // soon as the clock reaches the new month.
            Local.with_ymd_and_hms(year, month, 1, 1, 0, 0).earliest()
        })
}

fn validate_opencode_database(connection: &Connection) -> Result<()> {
    let (has_messages, has_recognized): (bool, bool) = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM message),
                EXISTS(
                    SELECT 1 FROM message
                    WHERE json_type(data, '$.providerID') IS NOT NULL
                      AND json_type(data, '$.cost') IS NOT NULL
                )",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if has_messages && !has_recognized {
        anyhow::bail!("message records have no recognized provider/cost fields");
    }
    Ok(())
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

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn capture_claude(input: &str, cache: &Path) -> Result<Vec<LimitWindow>> {
    let incoming: Value = serde_json::from_str(input).context("invalid Claude status-line JSON")?;
    if let Some(parent) = cache.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let lock_path = cache.with_extension("lock");
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    lock.lock()?;

    let existing = std::fs::read_to_string(cache)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
    let now = Local::now().timestamp();
    let rate_limits = merge_claude_limits(existing.as_ref(), &incoming, now).unwrap_or_default();
    let sanitized = serde_json::json!({ "rate_limits": rate_limits });

    // Persist only the allowance fields. This also strips session metadata from
    // caches written by older costsee versions.
    if existing.as_ref() != Some(&sanitized) {
        write_json_atomically(cache, &sanitized)?;
    }

    lock.unlock()?;
    // The capture itself succeeded. Claude Code runs this as its status line, so
    // having nothing to report yet must not fail the command.
    Ok(claude_quota(cache)
        .map(|summary| summary.windows)
        .unwrap_or_default())
}

fn valid_claude_window(value: &Value, now: i64) -> Option<(f64, i64)> {
    let percent = value["used_percentage"].as_f64()?;
    let resets_at = value["resets_at"].as_i64()?;
    (percent.is_finite() && (0.0..=100.0).contains(&percent) && resets_at > now)
        .then_some((percent, resets_at))
}

fn merge_claude_limits(
    existing: Option<&Value>,
    incoming: &Value,
    now: i64,
) -> Option<Map<String, Value>> {
    let mut merged = Map::new();
    for key in ["five_hour", "seven_day"] {
        let old = existing
            .and_then(|value| value.get("rate_limits"))
            .and_then(|limits| limits.get(key));
        let new = incoming
            .get("rate_limits")
            .and_then(|limits| limits.get(key));
        let selected = match (
            old.and_then(|value| valid_claude_window(value, now)),
            new.and_then(|value| valid_claude_window(value, now)),
        ) {
            (Some((old_percent, old_reset)), Some((new_percent, new_reset)))
                if new_reset > old_reset
                    || (new_reset == old_reset && new_percent >= old_percent) =>
            {
                new
            }
            (Some(_), _) => old,
            (None, Some(_)) => new,
            (None, None) => None,
        };
        if let Some(value) = selected {
            merged.insert(key.into(), value.clone());
        }
    }
    (!merged.is_empty()).then_some(merged)
}

fn write_json_atomically(cache: &Path, value: &Value) -> Result<()> {
    let parent = cache
        .parent()
        .context("Claude usage cache has no parent directory")?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut temporary, value)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(cache)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace {}", cache.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OpenCodeGoAllowances;
    use tempfile::tempdir;

    #[test]
    fn parses_codex_app_server_allowance() {
        let now = Local::now();
        let response = serde_json::json!({
            "rateLimits": {
                "planType": "plus",
                "primary": {
                    "usedPercent": 42.0,
                    "windowDurationMins": 300,
                    "resetsAt": now.timestamp() + 3_600
                }
            },
            "rateLimitsByLimitId": null
        });
        let summary = codex_summary(&response, now).unwrap();
        assert_eq!(summary.windows[0].label, "5 hours");
        assert_eq!(summary.windows[0].used_percent, 42.0);
        assert_eq!(summary.plan.as_deref(), Some("plus"));
    }

    #[test]
    fn captures_claude_status_line_data() {
        let directory = tempdir().unwrap();
        let cache = directory.path().join("claude.json");
        let now = Local::now().timestamp();
        let input = serde_json::json!({
            "rate_limits": {
                "five_hour": {"used_percentage": 23.5, "resets_at": now + 3_600},
                "seven_day": {"used_percentage": 41.2, "resets_at": now + 86_400}
            }
        });
        let windows = capture_claude(&input.to_string(), &cache).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[1].used_percent, 41.2);
        let cached: Value =
            serde_json::from_str(&std::fs::read_to_string(&cache).unwrap()).unwrap();
        assert_eq!(
            cached.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["rate_limits"]
        );
    }

    #[test]
    fn claude_cache_ignores_missing_and_older_session_limits() {
        let directory = tempdir().unwrap();
        let cache = directory.path().join("claude.json");
        let now = Local::now().timestamp();
        let current = serde_json::json!({
            "session_id": "current",
            "rate_limits": {
                "five_hour": {"used_percentage": 8.0, "resets_at": now + 3_600},
                "seven_day": {"used_percentage": 6.0, "resets_at": now + 86_400}
            }
        });
        capture_claude(&current.to_string(), &cache).unwrap();

        let without_limits = serde_json::json!({"session_id": "new-session"});
        let windows = capture_claude(&without_limits.to_string(), &cache).unwrap();
        assert_eq!(windows[0].used_percent, 8.0);

        let stale = serde_json::json!({
            "session_id": "old-session",
            "rate_limits": {
                "five_hour": {"used_percentage": 33.0, "resets_at": now + 1_800},
                "seven_day": {"used_percentage": 13.0, "resets_at": now + 43_200}
            }
        });
        let windows = capture_claude(&stale.to_string(), &cache).unwrap();
        assert_eq!(windows[0].used_percent, 8.0);
        assert_eq!(windows[1].used_percent, 6.0);
    }

    #[test]
    fn capturing_before_any_limits_arrive_succeeds_with_no_windows() {
        let directory = tempdir().unwrap();
        let cache = directory.path().join("claude.json");

        let windows = capture_claude(r#"{"session_id":"first-message"}"#, &cache).unwrap();

        assert!(windows.is_empty());
    }

    #[test]
    fn claude_capture_strips_legacy_session_metadata() {
        let directory = tempdir().unwrap();
        let cache = directory.path().join("claude.json");
        let now = Local::now().timestamp();
        let legacy = serde_json::json!({
            "cwd": "/private/project",
            "session_id": "secret-session-id",
            "transcript_path": "/private/transcript.jsonl",
            "cost": {"total_cost_usd": 12.34},
            "rate_limits": {
                "five_hour": {"used_percentage": 8.0, "resets_at": now + 3_600}
            }
        });
        std::fs::write(&cache, legacy.to_string()).unwrap();

        capture_claude(r#"{"session_id":"another-session"}"#, &cache).unwrap();

        let cached: Value =
            serde_json::from_str(&std::fs::read_to_string(&cache).unwrap()).unwrap();
        assert!(cached.get("cwd").is_none());
        assert!(cached.get("session_id").is_none());
        assert!(cached.get("transcript_path").is_none());
        assert!(cached.get("cost").is_none());
        assert_eq!(cached["rate_limits"]["five_hour"]["used_percentage"], 8.0);
    }

    #[test]
    fn claude_cache_accepts_a_new_reset_with_lower_usage() {
        let directory = tempdir().unwrap();
        let cache = directory.path().join("claude.json");
        let now = Local::now().timestamp();
        let first = serde_json::json!({
            "rate_limits": {
                "five_hour": {"used_percentage": 90.0, "resets_at": now + 60}
            }
        });
        capture_claude(&first.to_string(), &cache).unwrap();
        let reset = serde_json::json!({
            "rate_limits": {
                "five_hour": {"used_percentage": 2.0, "resets_at": now + 18_000}
            }
        });
        let windows = capture_claude(&reset.to_string(), &cache).unwrap();
        assert_eq!(windows[0].used_percent, 2.0);
    }

    #[test]
    fn concurrent_claude_captures_keep_the_newest_window() {
        let directory = tempdir().unwrap();
        let cache = directory.path().join("claude.json");
        let now = Local::now().timestamp();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut threads = Vec::new();

        for (percent, reset_offset) in [(80.0, 3_600), (4.0, 18_000)] {
            let cache = cache.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                let input = serde_json::json!({
                    "rate_limits": {
                        "five_hour": {
                            "used_percentage": percent,
                            "resets_at": now + reset_offset
                        }
                    }
                });
                barrier.wait();
                capture_claude(&input.to_string(), &cache).unwrap();
            }));
        }
        barrier.wait();
        for thread in threads {
            thread.join().unwrap();
        }

        let summary = claude_quota(&cache).unwrap();
        assert_eq!(summary.windows[0].used_percent, 4.0);
        assert_eq!(
            summary.windows[0].resets_at.unwrap().timestamp(),
            now + 18_000
        );
    }

    #[test]
    fn refreshing_claude_leaves_every_other_tool_alone() {
        let directory = tempdir().unwrap();
        let cache = directory.path().join("claude.json");
        let now = Local::now().timestamp();
        std::fs::write(
            &cache,
            serde_json::json!({
                "rate_limits": {
                    "five_hour": {"used_percentage": 12.0, "resets_at": now + 3_600}
                }
            })
            .to_string(),
        )
        .unwrap();
        let mut config = Config::default();
        config.paths.claude_usage_cache = cache;
        // Collecting Codex too would replace its summary with this failure.
        config.commands.codex = "/nonexistent/codex".into();
        let mut snapshot = QuotaSnapshot::default();
        snapshot.tools.insert(
            Tool::Codex,
            QuotaSummary {
                windows: vec![LimitWindow {
                    label: "7 days".into(),
                    used_percent: 41.0,
                    resets_at: None,
                    detail: "Codex App Server".into(),
                }],
                ..QuotaSummary::default()
            },
        );

        refresh_claude(&config, &mut snapshot);

        assert_eq!(snapshot.tools[&Tool::Claude].windows[0].used_percent, 12.0);
        assert_eq!(snapshot.tools[&Tool::Codex].windows[0].used_percent, 41.0);
    }

    #[test]
    fn claude_quota_rejects_expired_limits() {
        let directory = tempdir().unwrap();
        let cache = directory.path().join("claude.json");
        let expired = serde_json::json!({
            "rate_limits": {
                "five_hour": {
                    "used_percentage": 99.0,
                    "resets_at": Local::now().timestamp() - 1
                }
            }
        });
        std::fs::write(&cache, expired.to_string()).unwrap();
        assert!(claude_quota(&cache).is_err());
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

    #[test]
    fn month_boundaries_resolve_in_every_month() {
        let now = Local::now();

        for month in 1..=12 {
            assert!(month_boundary(now.year(), month).is_some(), "month {month}");
        }
    }

    #[test]
    fn rejects_unrecognized_opencode_message_format() {
        let connection = Connection::open_in_memory().unwrap();
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
        connection
            .execute(
                "INSERT INTO message VALUES
                 ('unknown', 0, '{\"provider\":\"opencode-go\",\"amount\":6.0}')",
                [],
            )
            .unwrap();

        assert!(validate_opencode_database(&connection).is_err());
    }
}
