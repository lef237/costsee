//! Presentation helpers shared by the dashboard and the Claude Code status line.

use chrono::Duration as ChronoDuration;

/// Usage at or above this share of an allowance is worth noticing.
pub const WARNING_PERCENT: f64 = 70.0;
/// Usage at or above this share of an allowance is nearly exhausted.
pub const CRITICAL_PERCENT: f64 = 90.0;

/// Rounds to the nearest cell, but keeps any non-zero usage visible.
pub fn bar_fill(percent: f64, width: usize) -> usize {
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

pub fn remaining_reset_time(remaining: ChronoDuration) -> String {
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

    #[test]
    fn bar_keeps_any_non_zero_usage_visible() {
        assert_eq!(bar_fill(0.0, 20), 0);
        assert_eq!(bar_fill(0.4, 20), 1);
        assert_eq!(bar_fill(50.0, 20), 10);
        assert_eq!(bar_fill(100.0, 20), 20);
        assert_eq!(bar_fill(12.0, 0), 0);
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
}
