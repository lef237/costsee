use anyhow::Result;
use clap::{Parser, Subcommand};
use costsee::{
    collect::{capture_claude, collect_quotas},
    config::Config,
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
    if matches!(cli.command, Some(Command::Init)) {
        let path = Config::init(cli.config)?;
        println!("created {}", path.display());
        return Ok(());
    }
    let (config, path) = Config::load(cli.config)?;
    match cli.command {
        Some(Command::Setup) => setup_claude(),
        Some(Command::CaptureClaude) => capture(&config),
        Some(Command::Report) => report(&config),
        Some(Command::Init) => unreachable!(),
        None => costsee::ui::run(config, path),
    }
}

fn report(config: &Config) -> Result<()> {
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
    Ok(())
}

fn capture(config: &Config) -> Result<()> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let windows = capture_claude(&input, &config.paths.claude_usage_cache)?;
    let status = windows
        .iter()
        .map(|w| format!("{} {:.0}%", w.label, w.used_percent))
        .collect::<Vec<_>>()
        .join(" · ");
    println!("allowance {status}");
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
            println!("Claude Code is already connected.");
            return Ok(());
        }
        anyhow::bail!(
            "Claude Code already has a custom statusLine; it was not overwritten.\n\
             Add `{command}` to your existing status-line script and pipe its stdin to it."
        );
    }
    value["statusLine"] = serde_json::json!({
        "type": "command",
        "command": command,
        "refreshInterval": 60
    });
    if let Some(parent) = settings.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = settings.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(&value)?)?;
    fs::rename(temporary, &settings)?;
    println!("Connected Claude Code via {}", settings.display());
    println!("Send one Claude Code message, then press r in costsee.");
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
