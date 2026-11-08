# costsee

[![CI](https://github.com/lef237/costsee/actions/workflows/ci.yml/badge.svg)](https://github.com/lef237/costsee/actions/workflows/ci.yml)

A Rust TUI that shows how much of your Codex, Claude Code, and OpenCode
allowances you have used—all in one place.

```text
 COSTSEE   most constrained Claude Code · 7 days  93%             updated 18:08

┌ Codex · team ──────────────────────── Codex App Server · updated 07/31 18:08 ┐
│        5 hours  ███░░░░░░░░░░░░░░░░░░   12%  left 2h 59m   reset 07-31 21:08 │
│         7 days  █████████░░░░░░░░░░░░   41%  left 4d 23h   reset 08-05 18:08 │
│  spend control  █░░░░░░░░░░░░░░░░░░░░    4%  left 19d 23h  reset 08-20 18:08 │
└──────────────────────────────────────────────────────────────────────────────┘
┌ Claude Code ────────────────── reported by Claude Code · updated 07/31 18:08 ┐
│        5 hours  ████████████████░░░░░   78%  left 59m      reset 07-31 19:08 │
│         7 days  ████████████████████░   93%  left 1d 23h   reset 08-02 18:08 │
└──────────────────────────────────────────────────────────────────────────────┘
```

Bar widths are consistent across tools, so usage can be compared directly
between blocks. Borders turn yellow above 70% usage and red above 90%.

## Supported allowances

- Codex: usage percentages for five-hour, seven-day, and other windows reported
  by the official App Server
- Claude Code: five-hour and seven-day usage percentages (plus the model-scoped
  weekly windows on plans that have them), covering usage from every client on
  the account — the CLI, the desktop app, IDE extensions — not just the
  terminal costsee runs in
- OpenCode: recorded local cost divided by configured spending limits

OpenCode supports multiple providers and does not have a single shared
allowance. Configure the monthly USD limit for OpenCode Zen or your workspace
in the configuration file.

## Installation

Rust 1.89 or later is required to build costsee.

From crates.io:

```console
cargo install costsee
costsee
```

From a local checkout:

```console
cargo install --path .
costsee
```

Press `r` to refresh and `q` or `Esc` to quit. While a refresh is running, the
footer shows `refreshing…`; after completion it briefly shows `✓ refreshed`.
When costsee reloads automatically it shows `✓ auto-refreshed`: Claude Code
whenever any of its usage sources updates, and Codex once a minute while the
dashboard is open. Set `refresh_interval_secs` and `claude_refresh_interval_secs`
in the configuration to change how often Codex and Claude Code are reread.

## Setup

Codex is detected without additional configuration. Claude Code usage is also
read without configuration, as long as `claude` is on `PATH` — see
[Claude Code usage sources](#claude-code-usage-sources) below. Connect Claude
Code's official status line, which also draws the allowance bars below your
prompt, with:

```console
costsee setup
```

This adds a `statusLine` entry to `~/.claude/settings.json` only when one is not
already configured. It never overwrites an existing status line.

If you are upgrading from an earlier version, run `costsee setup` again. This
disables periodic updates from old, idle Claude Code sessions.

For Codex CLI's status line, run:

```console
costsee setup codex
```

This installs a preset in `~/.codex/config.toml` (or `$CODEX_HOME/config.toml`)
showing the model with reasoning effort, directory, Git branch, context used,
and remaining primary and weekly allowances. Restart Codex CLI to apply it.
Existing `tui.status_line` settings, including an empty list, are never
overwritten. Comments and other settings are preserved. Use `/statusline`
inside Codex to customize an existing configuration. `costsee setup claude`
is equivalent to `costsee setup`.

## Status line

### Claude Code

While it records usage data, costsee also draws Claude Code's status line. The
first row describes the session and the second the subscription allowances,
with the same bars and level colors as the dashboard:

```text
Opus[high] · costsee(main) · ctx ███░░░░░░░  28% · $1.23
5h ████████░░  78% left 58m · 7d █████████░  93% left 1d 22h
```

The brackets after the model hold the settings that change how it answers, and
only the ones that differ from a plain session appear: the reasoning effort
level, `fast` while fast mode is on, `think off` while thinking is disabled, and
the output style whenever it is not `default`. The parentheses after the
directory hold the branch checked out there, abbreviated to the commit when
`HEAD` is detached and shortened to 24 characters when the name runs long.

Every part of the first row is omitted while Claude Code has nothing to report,
so a fresh session starts with the model name alone and fills in as it runs.
Until the first API response returns allowance data, the second row reads
`allowance pending`. Set `NO_COLOR` to render the status line without colors.

### Codex CLI

Codex supports an ordered list of built-in status items through
[`tui.status_line`](https://developers.openai.com/codex/config-reference/),
rather than Claude Code's external command renderer. `costsee setup codex`
configures these native items:

```toml
[tui]
status_line = ["model-with-reasoning", "current-dir", "git-branch", "context-used", "five-hour-limit", "weekly-limit"]
```

Codex renders and updates this footer itself; costsee does not need to keep
running. The allowance items show **remaining** capacity, whereas costsee's
dashboard and Claude Code bars show **used** capacity. Items without data are
omitted by Codex. Custom bars, two-row layout, and costsee's colors are only
available in Claude Code's status line; use the costsee dashboard to see Codex
usage with those bars.

## Claude Code usage sources

Besides the status line, costsee reads Claude Code's account-wide usage from
up to three more sources, from most to least trusted:

1. Claude Code's own `get_usage` control request, sent over a short-lived,
   hook-disabled, persistence-free `claude -p` session that never writes a
   session to disk. This is the same account-wide allowance
   data the desktop app and IDE extensions show, so it reflects usage from
   every client on the account, not just costsee's own terminal.
2. The CLI's own cached snapshot in `~/.claude.json`, which Claude Code
   refreshes periodically on its own regardless of which client last used it.
3. The Claude desktop app's local usage history, read only when it is
   installed. It does not record reset times, so windows sourced from it show
   `reset unknown`; the block's detail text shows when that sample was taken
   instead.

Each source is independent and optional; costsee uses whichever it can reach.
Whichever produced the most recent still-valid reading for a window wins — a
reading with a known reset time always outranks one without, since only it can
be trusted to represent the current window. costsee polls these sources every
`claude_refresh_interval_secs` (default 300 seconds, matching how long Claude
Code's own cache stays fresh) while the dashboard is open, and once more on
every manual refresh. Set `commands.claude` if `claude` is not on `PATH`, or
`paths.claude_config_json` / `paths.claude_usage_history` if either file lives
somewhere nonstandard.

To configure time-window limits for OpenCode Go, run:

```console
costsee init
```

Then add the following official OpenCode Go limits to the generated
configuration:

```toml
[allowances.opencode_go]
five_hour_usd = 12.0
weekly_usd = 30.0
monthly_usd = 60.0
```

For OpenCode Go, costsee estimates usage from the recorded cost in local
OpenCode Go history over the last five hours, the last seven days, and the
current calendar month. Local history does not provide exact reset times for
the five-hour and weekly windows, so the UI displays `unknown`. The existing
`allowances.opencode_monthly_usd` setting remains supported for monthly usage.

The default configuration path is `~/.config/costsee/config.toml` on every
operating system. Use `--config` to specify a different path. If `codex` or
`claude` is not on `PATH`, set its executable path under `[commands]`:

```toml
[commands]
codex = "/path/to/codex"
claude = "/path/to/claude"
```

## Non-interactive reports

```console
costsee report
```

```text
tool        window   used   reset                       status
Codex       7 days   2.0%   2026-08-05T20:59:27+09:00  ok
Claude Code 5 hours  23.5%  2026-07-29T22:00:00+09:00  ok
```

If no tool can be read, costsee prints the reason on each row and exits with
status 1. If at least one tool succeeds, unavailable or unconfigured tools do
not change the exit status from 0.

## Privacy

- For Codex, costsee temporarily starts the official App Server over stdio and
  reads only the result of `account/rateLimits/read`. It does not read session
  logs or credentials.
- For OpenCode, costsee reads only timestamps, providers, and costs from the
  local SQLite database in read-only mode. It does not read conversation text.
- For Claude Code, costsee caches only usage percentages, reset times, and
  which of the sources below reported them. From the official status-line
  input, that also includes the model, directory, context, and cost shown in
  the status line, which are drawn straight from that input and never written
  to disk; the session ID and transcript path are neither displayed nor saved.
  The branch is read from the repository's own `HEAD` file, without running
  `git` and without touching commits, remotes, or working-tree contents. The
  account-wide `get_usage` request runs over a throwaway, hook-disabled `-p`
  session that persists no history, exactly like the official desktop app and
  IDE extensions already do; it never sends a prompt, so it never starts a
  conversation or spends tokens. The `~/.claude.json` and desktop-app history reads are limited to
  the same usage percentages, reset times, and sample timestamps; neither is
  ever asked for conversation content, tokens, or credentials.
- costsee does not collect API keys, call private HTTP APIs, or send
  telemetry. Every request goes through the official `codex` or `claude`
  binary using its own existing authentication; costsee never reads or
  transmits credentials itself.
- OpenCode does not provide an official API that supports accurate time-window
  aggregation without reading conversation text. For that reason, local
  database access is isolated, and costsee does not display estimates for
  unsupported data formats.
- Usage values are the latest values reported by each CLI. Claude Code's
  status-line cache updates are reflected automatically while costsee is
  running; its other sources are reread on `claude_refresh_interval_secs`
  (default 300 seconds), and Codex usage is reread on `refresh_interval_secs`
  (default 60 seconds). For OpenCode, communicate with the CLI once and then
  press `r` in costsee.
