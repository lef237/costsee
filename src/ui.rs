use crate::{
    collect::collect_quotas,
    config::Config,
    model::{LimitWindow, QuotaSnapshot, Tool},
};
use anyhow::Result;
use chrono::{DateTime, Local};
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
    widgets::{Block, Borders, Gauge, Paragraph},
};
use std::{io, time::Duration};

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
    loop {
        terminal.draw(|frame| draw(frame, &snapshot, &config_path))?;
        if !event::poll(Duration::from_millis(500))? {
            let modified = cache_modified(&config.paths.claude_usage_cache);
            if modified != claude_cache_modified {
                snapshot = collect_quotas(&config);
                claude_cache_modified = modified;
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
                    snapshot = collect_quotas(&config);
                    claude_cache_modified = cache_modified(&config.paths.claude_usage_cache);
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn cache_modified(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
}

fn draw(frame: &mut ratatui::Frame, snapshot: &QuotaSnapshot, config_path: &std::path::Path) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Min(2),
        ])
        .split(frame.area());

    let highest = snapshot
        .tools
        .values()
        .flat_map(|summary| &summary.windows)
        .map(|window| window.used_percent)
        .reduce(f64::max);
    let headline = highest
        .map(|value| format!("most constrained  {value:.0}% used"))
        .unwrap_or_else(|| "waiting for usage data".into());
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " COSTSEE ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  allowance dashboard   "),
            Span::styled(headline, Style::default().fg(Color::Yellow)),
        ]))
        .block(Block::default().borders(Borders::BOTTOM)),
        root[0],
    );

    for (index, tool) in Tool::ALL.into_iter().enumerate() {
        draw_tool(frame, root[index + 1], snapshot, tool);
    }

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" r", Style::default().fg(Color::Cyan)),
            Span::raw(" refresh  "),
            Span::styled("q", Style::default().fg(Color::Cyan)),
            Span::raw(" quit   "),
            Span::styled(
                config_path.display().to_string(),
                Style::default().fg(Color::DarkGray),
            ),
        ])),
        root[4],
    );
}

fn draw_tool(frame: &mut ratatui::Frame, area: Rect, snapshot: &QuotaSnapshot, tool: Tool) {
    let summary = &snapshot.tools[&tool];
    let title = summary
        .plan
        .as_deref()
        .map(|plan| format!(" {} · {} ", tool.label(), plan))
        .unwrap_or_else(|| format!(" {} ", tool.label()));
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if summary.windows.is_empty() {
            Color::DarkGray
        } else {
            Color::Cyan
        }));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if summary.windows.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "usage percentage unavailable",
                    Style::default().fg(Color::Yellow),
                )),
                Line::from(summary.note.as_deref().unwrap_or("no data")),
            ]),
            inner,
        );
        return;
    }

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            summary
                .windows
                .iter()
                .map(|_| Constraint::Ratio(1, summary.windows.len() as u32))
                .collect::<Vec<_>>(),
        )
        .split(inner);
    for (index, window) in summary.windows.iter().enumerate() {
        draw_window(frame, columns[index], window, summary.updated_at);
    }
}

fn draw_window(
    frame: &mut ratatui::Frame,
    area: Rect,
    window: &LimitWindow,
    updated_at: Option<DateTime<Local>>,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);
    let reset = window
        .resets_at
        .map(|at| at.format("%a %H:%M").to_string())
        .unwrap_or_else(|| "unknown".into());
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{}  ", window.label),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("resets {reset}")),
        ])),
        rows[0],
    );
    let percent = window.used_percent.clamp(0.0, 100.0);
    let color = if percent >= 90.0 {
        Color::Red
    } else if percent >= 70.0 {
        Color::Yellow
    } else {
        Color::Green
    };
    frame.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(color))
            .ratio(percent / 100.0)
            .label(format!(
                "{:.0}% used · {:.0}% left",
                percent,
                100.0 - percent
            )),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            &window.detail,
            Style::default().fg(Color::DarkGray),
        )),
        rows[2],
    );
    if let Some(updated) = updated_at {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!("updated {}", updated.format("%m/%d %H:%M")),
                Style::default().fg(Color::DarkGray),
            )),
            rows[3],
        );
    }
}
