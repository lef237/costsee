//! Renders the two-line status line that Claude Code prints below the prompt.
//!
//! The first line describes the session, the second the subscription
//! allowances. Both reuse the dashboard's bars and level colors so the status
//! line and `costsee` itself read the same way.

use crate::{
    format::{CRITICAL_PERCENT, WARNING_PERCENT, bar_fill, remaining_reset_time},
    model::LimitWindow,
};
use chrono::{DateTime, Local};
use serde_json::Value;
use std::path::Path;

const BAR_WIDTH: usize = 10;
const RESET: &str = "\x1b[0m";
const BOLD: &str = "1";
const DIM: &str = "90";
const CYAN: &str = "36";
const GREEN: &str = "32";
const YELLOW: &str = "33";
const RED: &str = "31";

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    colored: bool,
}

impl Palette {
    /// Claude Code renders the status line in the user's terminal, so colors
    /// are available unless the environment opts out.
    pub fn detect() -> Self {
        Self {
            colored: std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty()),
        }
    }

    fn paint(self, code: &str, text: &str) -> String {
        if !self.colored || text.is_empty() {
            return text.to_string();
        }
        format!("\x1b[{code}m{text}{RESET}")
    }

    fn level(self, percent: f64) -> &'static str {
        if percent >= CRITICAL_PERCENT {
            RED
        } else if percent >= WARNING_PERCENT {
            YELLOW
        } else {
            GREEN
        }
    }
}

pub fn render(
    input: &Value,
    windows: &[LimitWindow],
    now: DateTime<Local>,
    palette: Palette,
) -> String {
    let separator = palette.paint(DIM, " · ");
    let mut lines = Vec::new();
    let session = session_segments(input, palette);
    if !session.is_empty() {
        lines.push(session.join(&separator));
    }
    lines.push(allowance_line(windows, now, palette, &separator));
    lines.join("\n")
}

fn session_segments(input: &Value, palette: Palette) -> Vec<String> {
    let mut segments = Vec::new();
    if let Some(model) = input["model"]["display_name"].as_str() {
        segments.push(palette.paint(BOLD, model));
    }
    if let Some(directory) = workspace_name(input) {
        segments.push(palette.paint(CYAN, &directory));
    }
    if let Some(percent) = percentage(&input["context_window"]["used_percentage"]) {
        segments.push(format!(
            "{} {}",
            palette.paint(DIM, "ctx"),
            gauge(percent, palette)
        ));
    }
    if let Some(cost) = cost_usd(input) {
        segments.push(palette.paint(DIM, &format!("${cost:.2}")));
    }
    segments
}

fn allowance_line(
    windows: &[LimitWindow],
    now: DateTime<Local>,
    palette: Palette,
    separator: &str,
) -> String {
    if windows.is_empty() {
        return palette.paint(DIM, "allowance pending");
    }
    windows
        .iter()
        .map(|window| {
            format!(
                "{} {} {}",
                palette.paint(BOLD, &short_label(&window.label)),
                gauge(window.used_percent, palette),
                palette.paint(DIM, &format!("left {}", remaining(window, now)))
            )
        })
        .collect::<Vec<_>>()
        .join(separator)
}

/// A bar plus its percentage, both colored by how much of the window is gone.
fn gauge(percent: f64, palette: Palette) -> String {
    let percent = percent.clamp(0.0, 100.0);
    let level = palette.level(percent);
    let filled = bar_fill(percent, BAR_WIDTH);
    format!(
        "{}{} {}",
        palette.paint(level, &"█".repeat(filled)),
        palette.paint(DIM, &"░".repeat(BAR_WIDTH - filled)),
        palette.paint(level, &format!("{percent:>3.0}%"))
    )
}

fn remaining(window: &LimitWindow, now: DateTime<Local>) -> String {
    window
        .resets_at
        .map(|at| remaining_reset_time(at.signed_duration_since(now)))
        .unwrap_or_else(|| "unknown".into())
}

/// "5 hours" reads as "5h" once the label sits next to a bar.
fn short_label(label: &str) -> String {
    let mut parts = label.split_whitespace();
    match (
        parts.next(),
        parts.next().and_then(|unit| unit.chars().next()),
    ) {
        (Some(count), Some(unit)) => format!("{count}{unit}"),
        _ => label.to_string(),
    }
}

fn workspace_name(input: &Value) -> Option<String> {
    let directory = input["workspace"]["current_dir"]
        .as_str()
        .or_else(|| input["cwd"].as_str())?;
    let path = Path::new(directory);
    Some(
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| directory.to_string()),
    )
}

/// The context window is null until the first API response of a session.
fn percentage(value: &Value) -> Option<f64> {
    let percent = value.as_f64()?;
    (percent.is_finite() && (0.0..=100.0).contains(&percent)).then_some(percent)
}

/// Sessions start at zero, where a price tag is only noise.
fn cost_usd(input: &Value) -> Option<f64> {
    let cost = input["cost"]["total_cost_usd"].as_f64()?;
    (cost.is_finite() && cost >= 0.005).then_some(cost)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;

    fn plain() -> Palette {
        Palette { colored: false }
    }

    fn windows(now: DateTime<Local>) -> Vec<LimitWindow> {
        vec![
            LimitWindow {
                label: "5 hours".into(),
                used_percent: 78.0,
                resets_at: Some(now + ChronoDuration::minutes(59)),
                detail: "reported by Claude Code".into(),
            },
            LimitWindow {
                label: "7 days".into(),
                used_percent: 93.0,
                resets_at: Some(now + ChronoDuration::hours(47)),
                detail: "reported by Claude Code".into(),
            },
        ]
    }

    #[test]
    fn renders_session_and_allowance_lines() {
        let now = Local::now();
        let input = serde_json::json!({
            "model": {"display_name": "Opus"},
            "workspace": {"current_dir": "/home/alice/src/costsee"},
            "context_window": {"used_percentage": 28.4},
            "cost": {"total_cost_usd": 1.234}
        });

        let rendered = render(&input, &windows(now), now, plain());

        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines[0], "Opus · costsee · ctx ███░░░░░░░  28% · $1.23");
        assert_eq!(
            lines[1],
            "5h ████████░░  78% left 59m · 7d █████████░  93% left 1d 23h"
        );
    }

    #[test]
    fn drops_session_fields_that_are_missing_or_still_empty() {
        let now = Local::now();
        let input = serde_json::json!({
            "model": {"display_name": "Opus"},
            "context_window": {"used_percentage": null},
            "cost": {"total_cost_usd": 0.0}
        });

        let rendered = render(&input, &windows(now), now, plain());

        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines[0], "Opus");
        assert!(lines[1].starts_with("5h "));
    }

    #[test]
    fn reports_pending_allowances_without_dropping_the_session_line() {
        let now = Local::now();
        let input = serde_json::json!({"model": {"display_name": "Opus"}});

        let rendered = render(&input, &[], now, plain());

        assert_eq!(rendered, "Opus\nallowance pending");
    }

    #[test]
    fn an_empty_session_leaves_only_the_allowance_line() {
        let now = Local::now();

        let rendered = render(&serde_json::json!({}), &windows(now), now, plain());

        assert_eq!(rendered.lines().count(), 1);
        assert!(rendered.starts_with("5h "));
    }

    #[test]
    fn colors_wrap_every_segment_and_reset_at_the_end_of_each_line() {
        let now = Local::now();
        let input = serde_json::json!({"model": {"display_name": "Opus"}});
        let colored = Palette { colored: true };

        let rendered = render(&input, &windows(now), now, colored);

        assert!(rendered.starts_with("\x1b[1mOpus\x1b[0m"));
        // 78% warns in yellow, 93% is critical in red.
        assert!(rendered.contains("\x1b[33m"));
        assert!(rendered.contains("\x1b[31m"));
        for line in rendered.lines() {
            assert!(line.ends_with(RESET), "line was not reset: {line}");
        }
    }
}
