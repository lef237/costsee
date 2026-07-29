use chrono::{DateTime, Local};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tool {
    Codex,
    Claude,
    OpenCode,
}

impl Tool {
    pub const ALL: [Tool; 3] = [Tool::Codex, Tool::Claude, Tool::OpenCode];

    pub fn label(self) -> &'static str {
        match self {
            Tool::Codex => "Codex",
            Tool::Claude => "Claude Code",
            Tool::OpenCode => "OpenCode",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LimitWindow {
    pub label: String,
    pub used_percent: f64,
    pub resets_at: Option<DateTime<Local>>,
    pub detail: String,
}

#[derive(Debug, Clone, Default)]
pub struct QuotaSummary {
    pub windows: Vec<LimitWindow>,
    pub plan: Option<String>,
    pub note: Option<String>,
    pub updated_at: Option<DateTime<Local>>,
}

#[derive(Debug, Clone, Default)]
pub struct QuotaSnapshot {
    pub tools: BTreeMap<Tool, QuotaSummary>,
}
