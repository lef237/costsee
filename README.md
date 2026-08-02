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
- Claude Code: five-hour and seven-day usage percentages reported by the
  official status line
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
When costsee reloads automatically after Claude Code updates its usage data, it
shows `✓ auto-refreshed`.

## Setup

Codex is detected without additional configuration. Connect Claude Code's
official status-line usage feed with:

```console
costsee setup
```

This adds a `statusLine` entry to `~/.claude/settings.json` only when one is not
already configured. It never overwrites an existing status line. After setup,
send one message in Claude Code to save the latest usage data.

If you are upgrading from an earlier version, run `costsee setup` again. This
disables periodic updates from old, idle Claude Code sessions.

## Status line

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
operating system. Use `--config` to specify a different path. If `codex` is not
on `PATH`, set its executable path under `[commands]`:

```toml
[commands]
codex = "/path/to/codex"
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
- For Claude Code, costsee caches only usage percentages and reset times from
  the official status-line input. The model, directory, context, and cost shown
  in the status line are drawn straight from that input and never written to
  disk, and the session ID and transcript path are neither displayed nor saved.
  The branch is read from the repository's own `HEAD` file, without running
  `git` and without touching commits, remotes, or working-tree contents.
- costsee does not collect API keys, call private APIs, or send telemetry. The
  official Codex App Server uses Codex's existing authentication when costsee
  requests rate-limit data.
- OpenCode does not provide an official API that supports accurate time-window
  aggregation without reading conversation text. For that reason, local
  database access is isolated, and costsee does not display estimates for
  unsupported data formats.
- Usage values are the latest values reported by each CLI. Claude Code cache
  updates are reflected automatically while costsee is running. For other
  tools, communicate with the CLI once and then press `r` in costsee.
