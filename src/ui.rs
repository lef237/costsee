use crate::{
    collect::collect_quotas,
    config::Config,
    model::{LimitWindow, QuotaSnapshot, QuotaSummary, Tool},
};
use anyhow::Result;
use chrono::{DateTime, Duration as ChronoDuration, Local};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use std::{
    io,
    time::{Duration, Instant},
};

pub fn run(config: Config, config_path: std::path::PathBuf) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = app_loop(&mut terminal, config, config_path);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn app_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: Config,
    config_path: std::path::PathBuf,
) -> Result<()> {
    let mut snapshot = collect_quotas(&config);
    let mut claude_cache_modified = cache_modified(&config.paths.claude_usage_cache);
    let mut status = Status::Idle;
    loop {
        terminal.draw(|frame| draw(frame, &snapshot, &config_path, status))?;
        status = status.expired();
        if !event::poll(Duration::from_millis(500))? {
            let modified = cache_modified(&config.paths.claude_usage_cache);
            if modified != claude_cache_modified {
                snapshot = collect_quotas(&config);
                claude_cache_modified = modified;
                status = Status::Refreshed {
                    at: Instant::now(),
                    by_key: false,
                };
            }
            continue;
        }
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('r') => {
                    // Collecting can block on the Codex app server, so say so first.
                    status = Status::Refreshing;
                    terminal.draw(|frame| draw(frame, &snapshot, &config_path, status))?;
                    snapshot = collect_quotas(&config);
                    claude_cache_modified = cache_modified(&config.paths.claude_usage_cache);
                    status = Status::Refreshed {
                        at: Instant::now(),
                        by_key: true,
                    };
                }
                _ => {}
            }
        }
    }
    Ok(())
}

const FLASH: Duration = Duration::from_secs(3);
/// Room the footer always keeps for a status label, so the config path below
/// does not appear and disappear as the status changes.
const STATUS_WIDTH: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Idle,
    Refreshing,
    Refreshed { at: Instant, by_key: bool },
}

impl Status {
    /// Drops a refresh confirmation once it has been on screen long enough.
    fn expired(self) -> Self {
        match self {
            Status::Refreshed { at, .. } if at.elapsed() >= FLASH => Status::Idle,
            other => other,
        }
    }

    fn label(self) -> Option<(&'static str, Color)> {
        match self {
            Status::Idle => None,
            Status::Refreshing => Some(("refreshing…", Color::Yellow)),
            Status::Refreshed { by_key: true, .. } => Some(("✓ refreshed", Color::Green)),
            Status::Refreshed { by_key: false, .. } => Some(("✓ auto-refreshed", Color::Green)),
        }
    }
}

fn cache_modified(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
}

const INDENT: usize = 2;
const GAP: usize = 2;
const PERCENT_WIDTH: usize = 4;
const MIN_BAR: usize = 10;
const MAX_BAR: usize = 44;
const LABEL_TIERS: [usize; 3] = [16, 9, 0];
/// Most a wrapped explanation may grow to before it is left truncated.
const MAX_NOTE_ROWS: usize = 3;
const RESET_TIERS: [u16; 3] = [40, 20, 12];

fn draw(
    frame: &mut ratatui::Frame,
    snapshot: &QuotaSnapshot,
    config_path: &std::path::Path,
    status: Status,
) {
    let mut constraints = vec![Constraint::Length(1), Constraint::Length(1)];
    for tool in Tool::ALL {
        constraints.push(Constraint::Length(block_height(
            &snapshot.tools[&tool],
            frame.area().width,
        )));
    }
    constraints.push(Constraint::Min(0));
    constraints.push(Constraint::Length(1));
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(frame.area());

    let now = Local::now();
    let columns = columns(snapshot, frame.area().width.saturating_sub(2) as usize, now);
    draw_header(frame, root[0], snapshot);
    for (index, tool) in Tool::ALL.into_iter().enumerate() {
        draw_tool(frame, root[index + 2], snapshot, tool, &columns, now);
    }
    draw_footer(frame, root[root.len() - 1], config_path, status);
}

fn block_height(summary: &QuotaSummary, width: u16) -> u16 {
    let rows = if summary.windows.is_empty() {
        1 + note_rows(note_of(summary), width)
    } else {
        summary.windows.len()
    };
    2 + rows as u16
}

fn note_of(summary: &QuotaSummary) -> &str {
    summary.note.as_deref().unwrap_or("not configured")
}

/// Counts the lines a wrapped note needs, following the same greedy word wrap
/// the paragraph uses so the block is neither too short nor padded with blanks.
fn note_rows(note: &str, width: u16) -> usize {
    let available = (width as usize).saturating_sub(2 + INDENT).max(1);
    let mut rows = 1;
    let mut used = 0;
    for word in note.split_whitespace() {
        let length = word.chars().count();
        if used == 0 {
            used = length;
        } else if used + 1 + length <= available {
            used += 1 + length;
        } else {
            rows += 1;
            used = length;
        }
        while used > available {
            rows += 1;
            used -= available;
        }
    }
    rows.min(MAX_NOTE_ROWS)
}

fn draw_header(frame: &mut ratatui::Frame, area: Rect, snapshot: &QuotaSnapshot) {
    let mut spans = vec![
        Span::styled(
            " COSTSEE ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ];
    match most_constrained(snapshot) {
        Some((tool, label, percent)) => {
            let percent_span = Span::styled(
                format!("{percent:.0}%"),
                Style::default()
                    .fg(level_color(percent))
                    .add_modifier(Modifier::BOLD),
            );
            if area.width >= 56 {
                spans.push(Span::styled(
                    "most constrained ",
                    Style::default().fg(Color::DarkGray),
                ));
                spans.push(Span::raw(format!("{} · {}", tool.label(), label)));
                spans.push(Span::raw("  "));
                spans.push(percent_span);
            } else {
                spans.push(percent_span);
                spans.push(Span::raw(format!(" {} · {label}", tool.label())));
            }
        }
        None => spans.push(Span::styled(
            "waiting for usage data",
            Style::default().fg(Color::Yellow),
        )),
    }

    let updated = snapshot
        .tools
        .values()
        .filter_map(|summary| summary.updated_at)
        .max()
        .filter(|_| area.width >= 64)
        .map(|at| format!("updated {} ", at.format("%H:%M")))
        .unwrap_or_default();
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(updated.len() as u16)])
        .split(area);
    frame.render_widget(Paragraph::new(Line::from(spans)), columns[0]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            updated,
            Style::default().fg(Color::DarkGray),
        ))),
        columns[1],
    );
}

fn draw_footer(
    frame: &mut ratatui::Frame,
    area: Rect,
    config_path: &std::path::Path,
    status: Status,
) {
    let key = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);
    let mut keys = vec![
        Span::raw(" "),
        Span::styled("r", key),
        Span::styled(" refresh   ", dim),
        Span::styled("q", key),
        Span::styled(" quit", dim),
    ];
    let reserved = Line::from(keys.clone()).width() + GAP + STATUS_WIDTH;
    if let Some((text, color)) = status.label() {
        keys.push(Span::raw(" ".repeat(GAP)));
        keys.push(Span::styled(text, Style::default().fg(color)));
    }

    let path = format!("{} ", config_path.display());
    if area.width as usize > reserved + path.chars().count() + 4 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(path, dim)).right_aligned()),
            area,
        );
    }
    frame.render_widget(Paragraph::new(Line::from(keys)), area);
}

fn most_constrained(snapshot: &QuotaSnapshot) -> Option<(Tool, &str, f64)> {
    snapshot
        .tools
        .iter()
        .flat_map(|(tool, summary)| {
            summary
                .windows
                .iter()
                .map(move |window| (*tool, window.label.as_str(), window.used_percent))
        })
        .max_by(|left, right| left.2.total_cmp(&right.2))
}

fn level_color(percent: f64) -> Color {
    if percent >= 90.0 {
        Color::Red
    } else if percent >= 70.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn border_color(worst: Option<f64>) -> Color {
    match worst {
        Some(percent) if percent >= 90.0 => Color::Red,
        Some(percent) if percent >= 70.0 => Color::Yellow,
        _ => Color::DarkGray,
    }
}

fn draw_tool(
    frame: &mut ratatui::Frame,
    area: Rect,
    snapshot: &QuotaSnapshot,
    tool: Tool,
    columns: &Columns,
    now: DateTime<Local>,
) {
    let summary = &snapshot.tools[&tool];
    let worst = summary
        .windows
        .iter()
        .map(|window| window.used_percent)
        .reduce(f64::max);
    let mut title = vec![
        Span::raw(" "),
        Span::styled(tool.label(), Style::default().add_modifier(Modifier::BOLD)),
    ];
    if let Some(plan) = summary.plan.as_deref() {
        title.push(Span::styled(
            format!(" · {plan}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    title.push(Span::raw(" "));
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color(worst)))
        .title_top(Line::from(title));
    if let Some(meta) = block_meta(summary, area.width) {
        block = block.title_top(
            Line::from(Span::styled(
                format!(" {meta} "),
                Style::default().fg(Color::DarkGray),
            ))
            .right_aligned(),
        );
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if summary.windows.is_empty() {
        draw_missing(frame, inner, summary);
        return;
    }

    let plan = meta_plan(&summary.windows, columns);
    // A short terminal drops trailing windows instead of leaving gaps between them.
    let visible = summary.windows.len().min(inner.height as usize);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(1); visible])
        .split(inner);
    for (index, window) in summary.windows.iter().take(visible).enumerate() {
        frame.render_widget(
            Paragraph::new(window_line(window, columns, &plan, now)),
            rows[index],
        );
    }
}

fn draw_missing(frame: &mut ratatui::Frame, inner: Rect, summary: &QuotaSummary) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" ".repeat(INDENT)),
            Span::styled("no usage data", Style::default().fg(Color::Yellow)),
        ])),
        rows[0],
    );
    // The note is the only place that says what to do next, so wrap it instead
    // of letting a long message run off the edge of the block.
    frame.render_widget(
        Paragraph::new(note_of(summary))
            .style(Style::default().fg(Color::DarkGray))
            .wrap(Wrap { trim: true }),
        indented(rows[1]),
    );
}

fn indented(area: Rect) -> Rect {
    let indent = INDENT as u16;
    Rect {
        x: area.x + indent.min(area.width),
        width: area.width.saturating_sub(indent),
        ..area
    }
}

fn block_meta(summary: &QuotaSummary, width: u16) -> Option<String> {
    if width < 68 {
        return None;
    }
    match (uniform_detail(&summary.windows), summary.updated_at) {
        (Some(source), Some(at)) => {
            Some(format!("{source} · updated {}", at.format("%m/%d %H:%M")))
        }
        (Some(source), None) => Some(source.to_string()),
        (None, Some(at)) => Some(format!("updated {}", at.format("%m/%d %H:%M"))),
        (None, None) => None,
    }
}

/// A detail shared by every window belongs in the block title, not in each row.
fn uniform_detail(windows: &[LimitWindow]) -> Option<&str> {
    let first = windows.first()?.detail.as_str();
    windows
        .iter()
        .all(|window| window.detail == first)
        .then_some(first)
}

/// Widths shared by every tool, so bars and percentages line up across blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Columns {
    label: usize,
    bar: usize,
    remaining: usize,
    meta: usize,
}

fn columns(snapshot: &QuotaSnapshot, width: usize, now: DateTime<Local>) -> Columns {
    let windows = || snapshot.tools.values().flat_map(|summary| &summary.windows);
    let widest_label = windows()
        .map(|window| window.label.chars().count())
        .max()
        .unwrap_or(0);
    let label = LABEL_TIERS
        .into_iter()
        .map(|tier| widest_label.min(tier))
        .find(|label| width >= prefix_width(*label) + MIN_BAR)
        .unwrap_or(0);

    let available = width.saturating_sub(prefix_width(label));
    let remaining = windows()
        .map(|window| remaining_text(window, now).chars().count())
        .max()
        .unwrap_or(0);
    let wanted_meta = windows()
        .map(|window| {
            GAP + remaining + GAP + reset_date(window.resets_at, RESET_TIERS[1]).chars().count()
        })
        .max()
        .unwrap_or(0);
    let bar = available
        .saturating_sub(wanted_meta)
        .clamp(MIN_BAR, MAX_BAR)
        .min(available);
    Columns {
        label,
        bar,
        remaining,
        meta: available - bar,
    }
}

fn prefix_width(label: usize) -> usize {
    let optional = |value: usize| if value > 0 { GAP + value } else { 0 };
    INDENT + optional(label) + GAP + PERCENT_WIDTH + 1
}

/// Which trailing columns one tool can afford inside the shared meta budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MetaPlan {
    detail: usize,
    remaining: usize,
    reset_hint: u16,
}

fn meta_plan(windows: &[LimitWindow], columns: &Columns) -> MetaPlan {
    let budget = columns.meta;
    let detail = match uniform_detail(windows) {
        Some(_) => 0,
        None => windows
            .iter()
            .map(|window| window.detail.chars().count())
            .max()
            .unwrap_or(0),
    };
    let remaining = columns.remaining;
    let mut plan = MetaPlan {
        detail,
        remaining,
        reset_hint: RESET_TIERS[0],
    };

    let mut tier = 1;
    while meta_width(&plan, windows) > budget {
        if tier < RESET_TIERS.len() {
            plan.reset_hint = RESET_TIERS[tier];
            tier += 1;
        } else if plan.reset_hint > 0 {
            plan.reset_hint = 0;
        } else if plan.remaining > 0 {
            plan.remaining = 0;
        } else if plan.detail > 0 {
            plan.detail = budget.saturating_sub(GAP);
            break;
        } else {
            break;
        }
    }
    if plan.detail > 0 && meta_width(&plan, windows) > budget {
        plan.detail = 0;
    }
    plan
}

fn meta_width(plan: &MetaPlan, windows: &[LimitWindow]) -> usize {
    let optional = |value: usize| if value > 0 { GAP + value } else { 0 };
    optional(plan.detail) + optional(plan.remaining) + optional(reset_width(plan, windows))
}

fn reset_width(plan: &MetaPlan, windows: &[LimitWindow]) -> usize {
    if plan.reset_hint == 0 {
        return 0;
    }
    windows
        .iter()
        .map(|window| {
            reset_date(window.resets_at, plan.reset_hint)
                .chars()
                .count()
        })
        .max()
        .unwrap_or(0)
}

fn remaining_text(window: &LimitWindow, now: DateTime<Local>) -> String {
    format!("left {}", reset_remaining(window.resets_at, now))
}

fn window_line(
    window: &LimitWindow,
    columns: &Columns,
    plan: &MetaPlan,
    now: DateTime<Local>,
) -> Line<'static> {
    let percent = window.used_percent.clamp(0.0, 100.0);
    let color = level_color(percent);
    let dim = Style::default().fg(Color::DarkGray);
    let mut spans = vec![Span::raw(" ".repeat(INDENT))];
    if columns.label > 0 {
        spans.push(Span::styled(
            format!(
                "{:>width$}",
                fit(&window.label, columns.label),
                width = columns.label
            ),
            Style::default().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" ".repeat(GAP)));
    }

    let filled = bar_fill(percent, columns.bar);
    spans.push(Span::styled("█".repeat(filled), Style::default().fg(color)));
    spans.push(Span::styled(
        "░".repeat(columns.bar - filled),
        Style::default().fg(Color::DarkGray),
    ));
    spans.push(Span::raw(" ".repeat(GAP)));
    spans.push(Span::styled(
        format!("{percent:>3.0}%"),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ));

    if plan.detail > 0 {
        spans.push(Span::raw(" ".repeat(GAP)));
        spans.push(Span::styled(
            format!(
                "{:<width$}",
                fit(&window.detail, plan.detail),
                width = plan.detail
            ),
            dim,
        ));
    }
    if plan.remaining > 0 {
        spans.push(Span::raw(" ".repeat(GAP)));
        spans.push(Span::styled(
            format!(
                "{:<width$}",
                remaining_text(window, now),
                width = plan.remaining
            ),
            dim,
        ));
    }
    if plan.reset_hint > 0 {
        spans.push(Span::raw(" ".repeat(GAP)));
        spans.push(Span::styled(
            reset_date(window.resets_at, plan.reset_hint),
            dim,
        ));
    }
    Line::from(spans)
}

/// Rounds to the nearest cell, but keeps any non-zero usage visible.
fn bar_fill(percent: f64, width: usize) -> usize {
    if width == 0 {
        return 0;
    }
    let filled = (percent / 100.0 * width as f64).round() as usize;
    if percent > 0.0 {
        filled.clamp(1, width)
    } else {
        filled.min(width)
    }
}

fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    text.chars()
        .take(width.saturating_sub(1))
        .collect::<String>()
        + "…"
}

fn reset_remaining(resets_at: Option<DateTime<Local>>, now: DateTime<Local>) -> String {
    resets_at
        .map(|at| remaining_reset_time(at.signed_duration_since(now)))
        .unwrap_or_else(|| "unknown".into())
}

fn reset_date(resets_at: Option<DateTime<Local>>, width: u16) -> String {
    resets_at
        .map(|at| {
            if width >= 22 {
                format!("reset {}", at.format("%Y-%m-%d %H:%M"))
            } else if width >= 17 {
                format!("reset {}", at.format("%m-%d %H:%M"))
            } else {
                format!("reset {}", at.format("%m-%d"))
            }
        })
        .unwrap_or_else(|| "reset unknown".into())
}

fn remaining_reset_time(remaining: ChronoDuration) -> String {
    let seconds = remaining.num_seconds();
    if seconds <= 0 {
        return "expired".into();
    }
    if seconds < 60 {
        return "<1m".into();
    }

    let total_minutes = seconds / 60;
    let days = total_minutes / (24 * 60);
    let hours = (total_minutes / 60) % 24;
    let minutes = total_minutes % 60;

    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn reset_remaining_reports_time_left() {
        let now = Local::now();
        let resets_at = now + ChronoDuration::hours(2) + ChronoDuration::minutes(5);

        assert_eq!(reset_remaining(Some(resets_at), now), "2h 5m");
    }

    #[test]
    fn reset_date_drops_year_then_time_in_narrow_columns() {
        let resets_at = Local::now();

        assert_eq!(
            reset_date(Some(resets_at), 22),
            format!("reset {}", resets_at.format("%Y-%m-%d %H:%M"))
        );
        assert_eq!(
            reset_date(Some(resets_at), 21),
            format!("reset {}", resets_at.format("%m-%d %H:%M"))
        );
        assert_eq!(
            reset_date(Some(resets_at), 16),
            format!("reset {}", resets_at.format("%m-%d"))
        );
    }

    #[test]
    fn reset_date_fits_the_width_it_was_given() {
        let resets_at = Local::now();

        for tier in RESET_TIERS {
            assert!(reset_date(Some(resets_at), tier).chars().count() <= tier as usize);
        }
    }

    #[test]
    fn remaining_reset_time_uses_compact_units() {
        assert_eq!(
            remaining_reset_time(
                ChronoDuration::days(2) + ChronoDuration::hours(3) + ChronoDuration::minutes(45)
            ),
            "2d 3h"
        );
        assert_eq!(remaining_reset_time(ChronoDuration::minutes(45)), "45m");
        assert_eq!(remaining_reset_time(ChronoDuration::seconds(30)), "<1m");
        assert_eq!(remaining_reset_time(ChronoDuration::zero()), "expired");
    }

    #[test]
    fn reset_fields_handle_unknown_reset_time() {
        assert_eq!(reset_remaining(None, Local::now()), "unknown");
        assert_eq!(reset_date(None, 80), "reset unknown");
    }

    fn window(label: &str, percent: f64, detail: &str) -> LimitWindow {
        LimitWindow {
            label: label.into(),
            used_percent: percent,
            resets_at: Some(Local::now() + ChronoDuration::hours(3)),
            detail: detail.into(),
        }
    }

    fn snapshot() -> QuotaSnapshot {
        let mut tools = BTreeMap::new();
        tools.insert(
            Tool::Codex,
            QuotaSummary {
                windows: vec![
                    window("5 hours", 12.0, "Codex App Server"),
                    window("spend control", 4.0, "Codex App Server"),
                ],
                ..QuotaSummary::default()
            },
        );
        tools.insert(
            Tool::Claude,
            QuotaSummary {
                windows: vec![window("7 days", 93.0, "reported by Claude Code")],
                ..QuotaSummary::default()
            },
        );
        tools.insert(
            Tool::OpenCode,
            QuotaSummary {
                windows: vec![
                    window("5 hours", 8.0, "$0.98 of $12.00 · local"),
                    window("This week", 55.0, "$16.50 of $30.00 · local"),
                ],
                ..QuotaSummary::default()
            },
        );
        QuotaSnapshot { tools }
    }

    #[test]
    fn every_tool_shares_one_bar_width() {
        let snapshot = snapshot();

        for width in [40usize, 56, 80, 120, 200] {
            let columns = columns(&snapshot, width, Local::now());

            assert!(columns.bar >= MIN_BAR.min(width));
            assert!(columns.bar <= MAX_BAR);
            assert!(prefix_width(columns.label) + columns.bar + columns.meta <= width);
        }
    }

    #[test]
    fn rows_never_exceed_the_terminal_width() {
        let snapshot = snapshot();
        let now = Local::now();

        for width in [24usize, 40, 56, 80, 120] {
            let columns = columns(&snapshot, width, now);
            for summary in snapshot.tools.values() {
                let plan = meta_plan(&summary.windows, &columns);
                for window in &summary.windows {
                    let rendered = window_line(window, &columns, &plan, now).width();
                    assert!(rendered <= width, "{rendered} > {width}");
                }
            }
        }
    }

    #[test]
    fn narrow_terminals_drop_trailing_columns_before_the_bar() {
        let snapshot = snapshot();
        let wide = columns(&snapshot, 120, Local::now());
        let narrow = columns(&snapshot, 44, Local::now());

        assert!(wide.meta > narrow.meta);
        assert_eq!(narrow.bar, MIN_BAR);
    }

    #[test]
    fn a_shared_detail_moves_into_the_block_title() {
        let snapshot = snapshot();
        let columns = columns(&snapshot, 100, Local::now());

        let codex = &snapshot.tools[&Tool::Codex];
        assert_eq!(uniform_detail(&codex.windows), Some("Codex App Server"));
        assert_eq!(meta_plan(&codex.windows, &columns).detail, 0);

        let opencode = &snapshot.tools[&Tool::OpenCode];
        assert_eq!(uniform_detail(&opencode.windows), None);
        assert!(meta_plan(&opencode.windows, &columns).detail > 0);
    }

    #[test]
    fn bar_keeps_any_non_zero_usage_visible() {
        assert_eq!(bar_fill(0.0, 20), 0);
        assert_eq!(bar_fill(0.4, 20), 1);
        assert_eq!(bar_fill(50.0, 20), 10);
        assert_eq!(bar_fill(100.0, 20), 20);
        assert_eq!(bar_fill(12.0, 0), 0);
    }

    #[test]
    fn a_note_gets_the_lines_its_length_needs() {
        let note = "unsupported OpenCode database format; update costsee: no such table: message";

        assert_eq!(note_rows(note, 80), 1);
        assert_eq!(note_rows(note, 60), 2);
        assert_eq!(note_rows(note, 40), 3);
        assert_eq!(note_rows(note, 20), MAX_NOTE_ROWS);
        assert_eq!(note_rows("short", 80), 1);
        assert_eq!(note_rows(note, 0), MAX_NOTE_ROWS);
        assert_eq!(note_rows("", 80), 1);
    }

    #[test]
    fn a_block_is_tall_enough_for_what_it_shows() {
        let mut summary = QuotaSummary {
            note: Some("set `allowances.opencode_go` in config".into()),
            ..QuotaSummary::default()
        };
        assert_eq!(block_height(&summary, 80), 4);
        assert_eq!(block_height(&summary, 30), 6);

        summary.windows = vec![window("5 hours", 1.0, "detail")];
        assert_eq!(block_height(&summary, 80), 3);
    }

    #[test]
    fn a_refresh_confirmation_fades_but_refreshing_stays() {
        let fresh = Status::Refreshed {
            at: Instant::now(),
            by_key: true,
        };
        let stale = Status::Refreshed {
            at: Instant::now() - FLASH,
            by_key: true,
        };

        assert_eq!(fresh.expired(), fresh);
        assert_eq!(stale.expired(), Status::Idle);
        assert_eq!(Status::Refreshing.expired(), Status::Refreshing);
        assert_eq!(Status::Idle.expired(), Status::Idle);
    }

    #[test]
    fn every_status_label_fits_the_reserved_footer_space() {
        for status in [
            Status::Refreshing,
            Status::Refreshed {
                at: Instant::now(),
                by_key: true,
            },
            Status::Refreshed {
                at: Instant::now(),
                by_key: false,
            },
        ] {
            let (text, _) = status.label().expect("a visible status");
            assert!(text.chars().count() <= STATUS_WIDTH, "{text}");
        }
    }

    #[test]
    fn only_a_busy_or_recent_refresh_shows_a_label() {
        assert_eq!(Status::Idle.label(), None);
        assert_eq!(Status::Refreshing.label().unwrap().0, "refreshing…");
        assert_eq!(
            Status::Refreshed {
                at: Instant::now(),
                by_key: true,
            }
            .label()
            .unwrap()
            .0,
            "✓ refreshed"
        );
        assert_eq!(
            Status::Refreshed {
                at: Instant::now(),
                by_key: false,
            }
            .label()
            .unwrap()
            .0,
            "✓ auto-refreshed"
        );
    }

    #[test]
    fn fit_truncates_with_an_ellipsis() {
        assert_eq!(fit("5 hours", 9), "5 hours");
        assert_eq!(fit("spend control", 9), "spend co…");
        assert_eq!(fit("日本語のラベル", 4), "日本語…");
    }
}
