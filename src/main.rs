use anyhow::Result;
use chrono::Local;
use clap::{Parser, Subcommand};
use costsee::{
    collect::{capture_claude, collect_quotas, probe_claude},
    config::Config,
    statusline,
};
use std::{
    fs,
    io::{self, Read},
    path::PathBuf,
};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Use a custom config file.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Create an editable default config.
    Init,
    /// Connect Claude Code's official status-line usage feed.
    Setup,
    /// Receive Claude Code status-line JSON (normally called by Claude Code).
    #[command(hide = true)]
    CaptureClaude,
    /// Print the current allowance percentages without opening the TUI.
    Report,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // Neither command reads the config, so a broken one must not block them.
    match cli.command {
        Some(Command::Init) => {
            let path = Config::init(cli.config)?;
            println!("created {}", path.display());
            return Ok(());
        }
        Some(Command::Setup) => return setup_claude(),
        _ => {}
    }
    let (config, path) = Config::load(cli.config)?;
    match cli.command {
        Some(Command::CaptureClaude) => capture(&config),
        Some(Command::Report) => report(&config),
        Some(Command::Init | Command::Setup) => unreachable!(),
        None => costsee::ui::run(config, path),
    }
}

fn report(config: &Config) -> Result<()> {
    // A one-shot report has no dashboard loop to poll in the background, so
    // ask Claude Code for account-wide usage here. Best-effort: a failure
    // leaves whatever the cache already holds. The report also waits on the
    // Codex App Server inside collect_quotas below, so this keeps the same
    // "wait for the current numbers" contract rather than reading stale data.
    let _ = probe_claude(config);
    let snapshot = collect_quotas(config);
    println!("tool\twindow\tused\treset\tstatus");
    for tool in costsee::model::Tool::ALL {
        let summary = &snapshot.tools[&tool];
        if summary.windows.is_empty() {
            println!(
                "{}\t-\t-\t-\t{}",
                tool.label(),
                summary.note.as_deref().unwrap_or("unavailable")
            );
        }
        for window in &summary.windows {
            println!(
                "{}\t{}\t{:.1}%\t{}\tok",
                tool.label(),
                window.label,
                window.used_percent,
                window
                    .resets_at
                    .map(|at| at.to_rfc3339())
                    .unwrap_or_else(|| "-".into())
            );
        }
    }
    // Every tool failed, so a caller piping this into something else should see
    // a failure rather than a header row on its own.
    if snapshot
        .tools
        .values()
        .all(|summary| summary.windows.is_empty())
    {
        anyhow::bail!("no allowance data available");
    }
    Ok(())
}

fn capture(config: &Config) -> Result<()> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let windows = capture_claude(&input, &config.paths.claude_usage_cache)?;
    // The capture already validated the payload, so a session without any
    // recognizable fields still renders the allowance line on its own.
    let session = serde_json::from_str(&input).unwrap_or(serde_json::Value::Null);
    println!(
        "{}",
        statusline::render(
            &session,
            &windows,
            Local::now(),
            statusline::Palette::detect()
        )
    );
    Ok(())
}

fn setup_claude() -> Result<()> {
    let settings = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("home directory is unavailable"))?
        .join(".claude/settings.json");
    let mut value: serde_json::Value = if settings.exists() {
        serde_json::from_str(&fs::read_to_string(&settings)?)?
    } else {
        serde_json::json!({})
    };
    let executable = std::env::current_exe()?;
    let command = format!(
        "{} capture-claude",
        shell_quote(&executable.display().to_string())
    );
    if let Some(existing) = value.get("statusLine") {
        if existing["command"].as_str() == Some(&command) {
            if value["statusLine"].get("refreshInterval").is_some() {
                value["statusLine"]
                    .as_object_mut()
                    .expect("statusLine was checked as an object")
                    .remove("refreshInterval");
                write_claude_settings(&settings, &value)?;
                println!("Updated Claude Code connection via {}", settings.display());
            } else {
                println!("Claude Code is already connected.");
            }
            return Ok(());
        }
        anyhow::bail!(
            "Claude Code already has a custom statusLine; it was not overwritten.\n\
             Add `{command}` to your existing status-line script and pipe its stdin to it."
        );
    }
    value["statusLine"] = serde_json::json!({
        "type": "command",
        "command": command
    });
    write_claude_settings(&settings, &value)?;
    println!("Connected Claude Code via {}", settings.display());
    println!("Send one Claude Code message, then press r in costsee.");
    Ok(())
}

fn write_claude_settings(settings: &std::path::Path, value: &serde_json::Value) -> Result<()> {
    if let Some(parent) = settings.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = settings.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(&value)?)?;
    fs::rename(temporary, settings)?;
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
