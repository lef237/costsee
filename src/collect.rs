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
    // Claude is read from its cache only, so this never blocks on `claude`.
    // Account-wide probes run in the background: the dashboard polls on its
    // own interval, and `report` asks for one explicitly before calling this.
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

/// The allowance windows Claude Code reports, in display order. The last two
/// are populated only for plans with model-scoped weekly limits (currently
/// Max), so most caches only ever fill the first two keys. The third element
/// is the window's duration, used to expire readings that have no reset time:
/// once older than the window itself, they can only describe a window that has
/// already ended.
const CLAUDE_WINDOWS: [(&str, &str, i64); 4] = [
    ("five_hour", "5 hours", 5 * 60 * 60),
    ("seven_day", "7 days", 7 * 24 * 60 * 60),
    ("seven_day_opus", "7-day Opus", 7 * 24 * 60 * 60),
    ("seven_day_sonnet", "7-day Sonnet", 7 * 24 * 60 * 60),
];

pub fn claude_quota(cache: &Path) -> Result<QuotaSummary> {
    if !cache.exists() {
        anyhow::bail!("run `costsee setup` to connect Claude Code");
    }
    let raw = std::fs::read_to_string(cache)?;
    let value: Value = serde_json::from_str(&raw)?;
    let mut windows = Vec::new();
    let now = Local::now().timestamp();
    for (key, label, max_age) in CLAUDE_WINDOWS {
        let Some(reading) = value["rate_limits"]
            .get(key)
            .and_then(|window| parse_claude_reading(window, now, max_age))
        else {
            continue;
        };
        windows.push(LimitWindow {
            label: label.into(),
            used_percent: reading.percent,
            resets_at: reading
                .resets_at
                .and_then(|at| Local.timestamp_opt(at, 0).single()),
            detail: reading.detail,
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
    let now = Local::now().timestamp();
    let normalized = normalize_claude_source(&incoming, "reported by Claude Code", now);
    // The capture itself succeeded even if the merge or the reread below found
    // nothing new. Claude Code runs this as its status line, so having nothing
    // to report yet must not fail the command.
    merge_and_write_claude_cache(cache, &[&normalized], now)?;
    Ok(claude_quota(cache)
        .map(|summary| summary.windows)
        .unwrap_or_default())
}

/// Refreshes Claude Code's cache from every source that does not require a
/// live status-line session: the account-wide `get_usage` control request,
/// the CLI's own cached snapshot in `~/.claude.json`, and the desktop app's
/// usage history, from most to least trusted. Each is independent and
/// best-effort, so a source that is unavailable (no `claude` on PATH, no
/// desktop app installed, offline) is skipped rather than failing the whole
/// refresh; the merge below then keeps whichever surviving reading is
/// freshest. Silent on total failure — the next periodic call tries again,
/// and the status line keeps updating the same cache independently.
pub fn probe_claude(config: &Config) -> Result<()> {
    let now = Local::now().timestamp();
    let mut readings = Vec::new();
    if let Ok(rate_limits) = claude_usage_response(&config.commands.claude) {
        readings.push(normalize_probe_response(&rate_limits, now));
    }
    if let Ok(cached) = read_claude_cached_utilization(&config.paths.claude_config_json) {
        readings.push(normalize_cached_json(&cached, now));
    }
    if let Ok(sample) = read_claude_history_latest(&config.paths.claude_usage_history) {
        readings.push(normalize_history_sample(&sample, now));
    }
    let candidates: Vec<&Value> = readings.iter().collect();
    merge_and_write_claude_cache(&config.paths.claude_usage_cache, &candidates, now)
}

/// Sends Claude Code's own `get_usage` control request over a throwaway `-p`
/// session and returns the `rate_limits` object from its reply. This is the
/// same account-wide allowance data the desktop app and IDE extensions show;
/// unlike the status line, it does not depend on a message having been sent
/// from this particular client. Hooks are disabled so a background poll
/// cannot trigger the user's own SessionStart/Stop automations, and session
/// persistence is off so a five-minute poll never writes a session to disk.
fn claude_usage_response(executable: &Path) -> Result<Value> {
    let mut child = ChildGuard(
        Command::new(executable)
            .args([
                "-p",
                "--no-session-persistence",
                "--verbose",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--settings",
                r#"{"disableAllHooks":true}"#,
            ])
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
        .context("Claude Code stdout is unavailable")?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                let _ = sender.send(value);
            }
        }
    });
    {
        let mut stdin = child
            .0
            .stdin
            .take()
            .context("Claude Code stdin is unavailable")?;
        let request = serde_json::json!({
            "type": "control_request",
            "request_id": "usage",
            "request": {"subtype": "get_usage"}
        });
        send_json_line(&mut stdin, &request)?;
        // Dropping stdin closes it, so Claude Code exits once it has replied
        // instead of waiting for a turn that will never come.
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let message = receiver
            .recv_timeout(remaining)
            .context("timed out waiting for Claude Code usage")?;
        if message["type"] != "control_response" || message["response"]["request_id"] != "usage" {
            continue;
        }
        if message["response"]["subtype"] != "success" {
            anyhow::bail!("Claude Code get_usage failed: {}", message["response"]);
        }
        return message["response"]["response"]
            .get("rate_limits")
            .filter(|value| value.is_object())
            .cloned()
            .context("Claude Code reported no rate limits");
    }
}

fn normalize_probe_response(rate_limits: &Value, now: i64) -> Value {
    let mut limits = Map::new();
    for (key, _, _) in CLAUDE_WINDOWS {
        let window = &rate_limits[key];
        let Some(percent) = window["utilization"].as_f64() else {
            continue;
        };
        let resets_at = window["resets_at"].as_str().and_then(parse_rfc3339_seconds);
        limits.insert(
            key.into(),
            claude_window_json(percent, resets_at, "Claude Code · all clients", now),
        );
    }
    serde_json::json!({ "rate_limits": limits })
}

fn read_claude_cached_utilization(path: &Path) -> Result<Value> {
    let raw = std::fs::read_to_string(path)?;
    let value: Value = serde_json::from_str(&raw)?;
    value
        .get("cachedUsageUtilization")
        .cloned()
        .context("no cachedUsageUtilization in Claude Code's own config")
}

fn normalize_cached_json(cached: &Value, now: i64) -> Value {
    let fetched_at = cached["fetchedAtMs"].as_i64().map(|ms| ms / 1_000);
    let detail = match fetched_at.and_then(|at| Local.timestamp_opt(at, 0).single()) {
        Some(at) => format!("Claude Code cache · fetched {}", at.format("%m/%d %H:%M")),
        None => "Claude Code cache".to_string(),
    };
    let utilization = &cached["utilization"];
    let mut limits = Map::new();
    for (key, _, _) in CLAUDE_WINDOWS {
        let window = &utilization[key];
        let Some(percent) = window["utilization"].as_f64() else {
            continue;
        };
        let resets_at = window["resets_at"].as_str().and_then(parse_rfc3339_seconds);
        limits.insert(
            key.into(),
            claude_window_json(percent, resets_at, &detail, fetched_at.unwrap_or(now)),
        );
    }
    serde_json::json!({ "rate_limits": limits })
}

fn read_claude_history_latest(path: &Path) -> Result<Value> {
    let raw = std::fs::read_to_string(path)?;
    let value: Value = serde_json::from_str(&raw)?;
    let samples = value["samples"]
        .as_array()
        .context("no samples in Claude desktop app history")?;
    samples
        .iter()
        .max_by_key(|sample| sample["t"].as_i64().unwrap_or(0))
        .cloned()
        .context("Claude desktop app history is empty")
}

/// The desktop app records samples without a reset time, so these windows
/// display as `reset unknown`; the detail string carries when the sample was
/// taken instead, since the app may not have run recently.
fn normalize_history_sample(sample: &Value, now: i64) -> Value {
    let observed_at = sample["t"].as_i64().map(|ms| ms / 1_000).unwrap_or(now);
    let detail = match Local.timestamp_opt(observed_at, 0).single() {
        Some(at) => format!("Claude desktop app · seen {}", at.format("%m/%d %H:%M")),
        None => "Claude desktop app".to_string(),
    };
    let usage = &sample["u"];
    let mut limits = Map::new();
    for (short, key) in [
        ("fh", "five_hour"),
        ("sd", "seven_day"),
        ("so", "seven_day_opus"),
        ("sn", "seven_day_sonnet"),
    ] {
        let Some(percent) = usage[short].as_f64() else {
            continue;
        };
        limits.insert(
            key.into(),
            claude_window_json(percent, None, &detail, observed_at),
        );
    }
    serde_json::json!({ "rate_limits": limits })
}

fn parse_rfc3339_seconds(text: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|at| at.timestamp())
}

/// Normalizes Claude Code's own status-line JSON into the same shape the
/// other sources produce, so all four can be merged by one function.
fn normalize_claude_source(raw: &Value, detail: &str, observed_at: i64) -> Value {
    let mut limits = Map::new();
    if let Some(source) = raw.get("rate_limits").and_then(Value::as_object) {
        for (key, _, _) in CLAUDE_WINDOWS {
            let Some(window) = source.get(key) else {
                continue;
            };
            let Some(percent) = window["used_percentage"].as_f64() else {
                continue;
            };
            let resets_at = window.get("resets_at").and_then(Value::as_i64);
            limits.insert(
                key.into(),
                claude_window_json(percent, resets_at, detail, observed_at),
            );
        }
    }
    serde_json::json!({ "rate_limits": limits })
}

fn claude_window_json(
    percent: f64,
    resets_at: Option<i64>,
    detail: &str,
    observed_at: i64,
) -> Value {
    let mut window = serde_json::json!({
        "used_percentage": percent,
        "resets_at": resets_at,
        "detail": detail,
    });
    // The observation time only breaks a tie between two undated readings; a
    // dated one is always decided by its reset time instead. Persisting it
    // anyway would change the cache on every poll even when usage has not
    // moved, rewriting the file and flashing a false "auto-refreshed". So it
    // is kept only where it can matter.
    if resets_at.is_none() {
        window["observed_at"] = observed_at.into();
    }
    window
}

struct ClaudeReading {
    percent: f64,
    resets_at: Option<i64>,
    observed_at: i64,
    detail: String,
}

fn parse_claude_reading(value: &Value, now: i64, max_age: i64) -> Option<ClaudeReading> {
    let percent = value["used_percentage"].as_f64()?;
    if !(percent.is_finite() && (0.0..=100.0).contains(&percent)) {
        return None;
    }
    let resets_at = match value.get("resets_at") {
        None | Some(Value::Null) => None,
        Some(raw) => {
            let at = raw.as_i64()?;
            if at <= now {
                // An expired dated window is not a usable reading at all,
                // even as a last resort: it is not evidence of anything
                // about the window that replaced it.
                return None;
            }
            Some(at)
        }
    };
    let observed_at = value["observed_at"].as_i64().unwrap_or(now);
    // A reading without a reset time has nothing else to tie it to a window,
    // so once it is older than the window itself it can only describe a window
    // that has already ended; drop it rather than showing a stale number as
    // the current one.
    if resets_at.is_none() && observed_at < now.saturating_sub(max_age) {
        return None;
    }
    let detail = value["detail"]
        .as_str()
        .unwrap_or("reported by Claude Code")
        .to_string();
    Some(ClaudeReading {
        percent,
        resets_at,
        observed_at,
        detail,
    })
}

/// Picks the more trustworthy of two readings for the same window. A reading
/// with a known reset time always beats one without, since usage is only
/// guaranteed to move in one direction within a single, identifiable window.
/// Between two dated readings, the one with the later reset wins outright
/// (it belongs to a newer window); within the same reset, the higher usage
/// wins, since usage cannot decrease before a reset. Between two undated
/// readings, only the observation time can tell them apart.
fn better_claude_reading(
    old: Option<ClaudeReading>,
    new: Option<ClaudeReading>,
) -> Option<ClaudeReading> {
    match (old, new) {
        (Some(old), Some(new)) => Some(match (old.resets_at, new.resets_at) {
            (Some(o), Some(n)) if n > o || (n == o && new.percent >= old.percent) => new,
            (Some(_), Some(_)) => old,
            (None, Some(_)) => new,
            (Some(_), None) => old,
            (None, None) if new.observed_at >= old.observed_at => new,
            (None, None) => old,
        }),
        (Some(old), None) => Some(old),
        (None, Some(new)) => Some(new),
        (None, None) => None,
    }
}

fn merge_claude_windows(
    existing: Option<&Value>,
    candidates: &[&Value],
    now: i64,
) -> Option<Map<String, Value>> {
    let mut merged = Map::new();
    for (key, _, max_age) in CLAUDE_WINDOWS {
        let mut best = existing
            .and_then(|value| value.get("rate_limits"))
            .and_then(|limits| limits.get(key))
            .and_then(|window| parse_claude_reading(window, now, max_age));
        for candidate in candidates {
            let reading = candidate
                .get("rate_limits")
                .and_then(|limits| limits.get(key))
                .and_then(|window| parse_claude_reading(window, now, max_age));
            best = better_claude_reading(best, reading);
        }
        if let Some(reading) = best {
            merged.insert(
                key.into(),
                claude_window_json(
                    reading.percent,
                    reading.resets_at,
                    &reading.detail,
                    reading.observed_at,
                ),
            );
        }
    }
    (!merged.is_empty()).then_some(merged)
}

fn merge_and_write_claude_cache(cache: &Path, candidates: &[&Value], now: i64) -> Result<()> {
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
    let rate_limits = merge_claude_windows(existing.as_ref(), candidates, now).unwrap_or_default();
    let sanitized = serde_json::json!({ "rate_limits": rate_limits });

    // Persist only the allowance fields. This also strips session metadata from
    // caches written by older costsee versions.
    if existing.as_ref() != Some(&sanitized) {
        write_json_atomically(cache, &sanitized)?;
    }

    lock.unlock()?;
    Ok(())
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

    fn claude_reading(percent: f64, resets_at: Option<i64>, observed_at: i64) -> ClaudeReading {
        ClaudeReading {
            percent,
            resets_at,
            observed_at,
            detail: "test".into(),
        }
    }

    #[test]
    fn parse_claude_reading_accepts_dated_and_undated_windows() {
        let now = Local::now().timestamp();
        let five_hour = 5 * 3_600;

        let dated = serde_json::json!({"used_percentage": 40.0, "resets_at": now + 3_600});
        let reading = parse_claude_reading(&dated, now, five_hour).unwrap();
        assert_eq!(reading.resets_at, Some(now + 3_600));
        assert_eq!(reading.detail, "reported by Claude Code");

        let undated = serde_json::json!({"used_percentage": 12.0, "resets_at": null});
        assert_eq!(
            parse_claude_reading(&undated, now, five_hour)
                .unwrap()
                .resets_at,
            None
        );

        let expired = serde_json::json!({"used_percentage": 90.0, "resets_at": now - 1});
        assert!(parse_claude_reading(&expired, now, five_hour).is_none());

        let out_of_range = serde_json::json!({"used_percentage": 140.0, "resets_at": null});
        assert!(parse_claude_reading(&out_of_range, now, five_hour).is_none());

        // Without a reset time, a reading older than the window can only be
        // about a window that has already ended, so it is expired too.
        let stale = serde_json::json!({
            "used_percentage": 12.0,
            "resets_at": null,
            "observed_at": now - 6 * 3_600
        });
        assert!(parse_claude_reading(&stale, now, five_hour).is_none());
        let fresh = serde_json::json!({
            "used_percentage": 12.0,
            "resets_at": null,
            "observed_at": now - 2 * 3_600
        });
        assert!(parse_claude_reading(&fresh, now, five_hour).is_some());
    }

    #[test]
    fn better_claude_reading_prefers_a_dated_reading_over_an_undated_one_either_way_round() {
        let picked = better_claude_reading(
            Some(claude_reading(90.0, None, 500)),
            Some(claude_reading(5.0, Some(1_000), 0)),
        );
        assert_eq!(picked.unwrap().resets_at, Some(1_000));

        let picked = better_claude_reading(
            Some(claude_reading(5.0, Some(1_000), 0)),
            Some(claude_reading(90.0, None, 500)),
        );
        assert_eq!(picked.unwrap().resets_at, Some(1_000));
    }

    #[test]
    fn better_claude_reading_picks_the_later_reset_and_the_higher_usage_within_one() {
        let picked = better_claude_reading(
            Some(claude_reading(80.0, Some(1_000), 0)),
            Some(claude_reading(4.0, Some(2_000), 0)),
        );
        assert_eq!(picked.unwrap().resets_at, Some(2_000));

        let picked = better_claude_reading(
            Some(claude_reading(10.0, Some(1_000), 0)),
            Some(claude_reading(50.0, Some(1_000), 0)),
        );
        assert_eq!(picked.unwrap().percent, 50.0);
    }

    #[test]
    fn better_claude_reading_breaks_undated_ties_by_observation_time() {
        let picked = better_claude_reading(
            Some(claude_reading(90.0, None, 100)),
            Some(claude_reading(3.0, None, 200)),
        );
        assert_eq!(picked.unwrap().percent, 3.0);
    }

    #[test]
    fn normalize_probe_response_reads_utilization_and_rfc3339_resets() {
        let now = Local::now().timestamp();
        let resets_at = Local::now() + chrono::Duration::hours(1);
        let rate_limits = serde_json::json!({
            "five_hour": {"utilization": 46, "resets_at": resets_at.to_rfc3339()},
            "seven_day": {"utilization": 43, "resets_at": resets_at.to_rfc3339()},
            "seven_day_opus": null
        });

        let normalized = normalize_probe_response(&rate_limits, now);

        let reading =
            parse_claude_reading(&normalized["rate_limits"]["five_hour"], now, 5 * 3_600).unwrap();
        assert_eq!(reading.percent, 46.0);
        assert!(reading.resets_at.is_some());
        assert_eq!(reading.detail, "Claude Code · all clients");
        assert!(normalized["rate_limits"].get("seven_day_opus").is_none());
        assert!(normalized["rate_limits"].get("seven_day_sonnet").is_none());
    }

    #[test]
    fn normalize_cached_json_reads_the_clis_own_cache_shape() {
        let now = Local::now().timestamp();
        let resets_at = Local::now() + chrono::Duration::hours(2);
        let cached = serde_json::json!({
            "fetchedAtMs": now * 1_000,
            "utilization": {
                "five_hour": null,
                "seven_day": {"utilization": 43, "resets_at": resets_at.to_rfc3339()}
            }
        });

        let normalized = normalize_cached_json(&cached, now);

        let reading =
            parse_claude_reading(&normalized["rate_limits"]["seven_day"], now, 7 * 24 * 3_600)
                .unwrap();
        assert_eq!(reading.percent, 43.0);
        assert!(reading.detail.starts_with("Claude Code cache · fetched"));
        assert!(normalized["rate_limits"].get("five_hour").is_none());
    }

    #[test]
    fn a_cli_cache_without_a_reset_time_is_aged_by_its_fetch_time() {
        // The cache snapshot is dated by fetchedAtMs, not by the poll time, so
        // a two-day-old snapshot without a reset time is expired rather than
        // being treated as the freshest reading.
        let now = Local::now().timestamp();
        let cached = serde_json::json!({
            "fetchedAtMs": (now - 48 * 3_600) * 1_000,
            "utilization": {
                "five_hour": {"utilization": 80, "resets_at": null}
            }
        });

        let normalized = normalize_cached_json(&cached, now);
        let reading = parse_claude_reading(&normalized["rate_limits"]["five_hour"], now, 5 * 3_600);

        assert!(reading.is_none());
    }

    #[test]
    fn normalize_history_sample_maps_short_keys_and_has_no_reset_time() {
        let observed = Local::now().timestamp();
        let sample = serde_json::json!({"t": observed * 1_000, "u": {"fh": 12, "sd": 55}});

        let normalized = normalize_history_sample(&sample, observed);

        let reading =
            parse_claude_reading(&normalized["rate_limits"]["five_hour"], observed, 5 * 3_600)
                .unwrap();
        assert_eq!(reading.percent, 12.0);
        assert_eq!(reading.resets_at, None);
        assert!(reading.detail.starts_with("Claude desktop app · seen"));
        assert!(normalized["rate_limits"].get("seven_day_opus").is_none());
    }

    #[test]
    fn read_claude_history_latest_picks_the_newest_sample_regardless_of_order() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("plan-usage-history.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 2,
                "samples": [
                    {"t": 1_000, "u": {"fh": 10, "sd": 20}},
                    {"t": 3_000, "u": {"fh": 30, "sd": 40}},
                    {"t": 2_000, "u": {"fh": 15, "sd": 25}}
                ]
            })
            .to_string(),
        )
        .unwrap();

        let latest = read_claude_history_latest(&path).unwrap();

        assert_eq!(latest["t"], 3_000);
    }

    #[test]
    fn probe_claude_falls_back_to_the_cli_cache_and_desktop_history_when_the_probe_is_unavailable()
    {
        let directory = tempdir().unwrap();
        let mut config = Config::default();
        config.commands.claude = "/nonexistent/claude".into();
        config.paths.claude_usage_cache = directory.path().join("claude-usage.json");
        config.paths.claude_config_json = directory.path().join("claude.json");
        config.paths.claude_usage_history = directory.path().join("plan-usage-history.json");

        // Only the CLI's cache knows the seven-day window's reset time, so it
        // must win there even though the desktop app reports higher usage.
        // Only the desktop app reports the five-hour window at all.
        let resets_at = Local::now() + chrono::Duration::hours(3);
        std::fs::write(
            &config.paths.claude_config_json,
            serde_json::json!({
                "cachedUsageUtilization": {
                    "fetchedAtMs": Local::now().timestamp() * 1_000,
                    "utilization": {
                        "seven_day": {"utilization": 46, "resets_at": resets_at.to_rfc3339()}
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            &config.paths.claude_usage_history,
            serde_json::json!({
                "version": 2,
                "samples": [
                    {"t": Local::now().timestamp() * 1_000, "u": {"fh": 22, "sd": 90}}
                ]
            })
            .to_string(),
        )
        .unwrap();

        probe_claude(&config).unwrap();

        let summary = claude_quota(&config.paths.claude_usage_cache).unwrap();
        let five_hour = summary
            .windows
            .iter()
            .find(|window| window.label == "5 hours")
            .unwrap();
        let seven_day = summary
            .windows
            .iter()
            .find(|window| window.label == "7 days")
            .unwrap();
        assert_eq!(five_hour.used_percent, 22.0);
        assert!(five_hour.resets_at.is_none());
        assert_eq!(seven_day.used_percent, 46.0);
        assert!(seven_day.resets_at.is_some());
    }

    #[test]
    fn a_reading_keeps_its_observation_time_only_when_it_has_no_reset_time() {
        let now = Local::now().timestamp();

        let dated = claude_window_json(40.0, Some(now + 3_600), "probe", now);
        assert!(dated.get("observed_at").is_none());

        let undated = claude_window_json(12.0, None, "desktop", now);
        assert_eq!(undated["observed_at"], now);
    }

    #[test]
    fn probing_with_unchanged_usage_never_rewrites_the_cache() {
        let directory = tempdir().unwrap();
        let cache = directory.path().join("claude-usage.json");
        let resets_at = Local::now() + chrono::Duration::hours(1);
        let rate_limits = serde_json::json!({
            "five_hour": {"utilization": 46, "resets_at": resets_at.to_rfc3339()}
        });
        let normalized = normalize_probe_response(&rate_limits, Local::now().timestamp());

        merge_and_write_claude_cache(&cache, &[&normalized], Local::now().timestamp()).unwrap();
        let before = std::fs::metadata(&cache).unwrap().modified().unwrap();
        // A later poll within the same window agrees with the first, so the
        // cache must not be touched and the dashboard must show no refresh.
        std::thread::sleep(std::time::Duration::from_millis(10));
        merge_and_write_claude_cache(&cache, &[&normalized], Local::now().timestamp()).unwrap();
        let after = std::fs::metadata(&cache).unwrap().modified().unwrap();

        assert_eq!(before, after);
    }

    /// Writes a stand-in for `claude -p --verbose ...` that swallows the
    /// control request on stdin and answers with `reply_lines` on stdout, the
    /// way the real binary did when this shape was verified against it.
    #[cfg(unix)]
    fn mock_claude(reply_lines: &[&str]) -> (tempfile::TempDir, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempdir().unwrap();
        let executable = directory.path().join("claude");
        let script = format!(
            "#!/bin/sh\ncat > /dev/null\ncat <<'EOF'\n{}\nEOF\n",
            reply_lines.join("\n")
        );
        std::fs::write(&executable, script).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        (directory, executable)
    }

    #[cfg(unix)]
    #[test]
    fn claude_usage_response_parses_a_real_control_reply() {
        let resets_at = Local::now() + chrono::Duration::hours(1);
        // The foreign request id proves the probe skips messages that are not
        // its reply before matching, as the real binary also interleaves other
        // messages with the control response.
        let foreign = serde_json::json!({
            "type": "assistant",
            "request_id": "other",
            "message": {"role": "assistant", "content": []},
        })
        .to_string();
        let reply = serde_json::json!({
            "type": "control_response",
            "response": {
                "request_id": "usage",
                "subtype": "success",
                "response": {
                    "rate_limits": {
                        "five_hour": {"utilization": 46, "resets_at": resets_at.to_rfc3339()},
                        "seven_day": {"utilization": 43, "resets_at": null},
                        "seven_day_opus": null,
                    }
                }
            }
        })
        .to_string();
        let (directory, executable) = mock_claude(&[&foreign, &reply]);

        let rate_limits = claude_usage_response(&executable).unwrap();
        drop(directory);

        // The raw reply carries the account-wide shape; feed it through the
        // same normalization the probe path uses before reading percentages.
        let now = Local::now().timestamp();
        let normalized = normalize_probe_response(&rate_limits, now);
        let five_hour =
            parse_claude_reading(&normalized["rate_limits"]["five_hour"], now, 5 * 3_600).unwrap();
        assert_eq!(five_hour.percent, 46.0);
        assert_eq!(five_hour.resets_at, Some(resets_at.timestamp()));
        let seven_day =
            parse_claude_reading(&normalized["rate_limits"]["seven_day"], now, 7 * 24 * 3_600)
                .unwrap();
        assert_eq!(seven_day.percent, 43.0);
        assert!(seven_day.resets_at.is_none());
        assert!(normalized["rate_limits"].get("seven_day_opus").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn claude_usage_response_rejects_a_failed_control_reply() {
        let reply = serde_json::json!({
            "type": "control_response",
            "response": {
                "request_id": "usage",
                "subtype": "failure",
                "response": {"error": "not subscribed"},
            }
        })
        .to_string();
        let (directory, executable) = mock_claude(&[&reply]);

        let result = claude_usage_response(&executable);
        drop(directory);

        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn claude_usage_response_requires_rate_limits_in_a_successful_reply() {
        let reply = serde_json::json!({
            "type": "control_response",
            "response": {"request_id": "usage", "subtype": "success", "response": {}}
        })
        .to_string();
        let (directory, executable) = mock_claude(&[&reply]);

        let result = claude_usage_response(&executable);
        drop(directory);

        assert!(result.is_err());
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
