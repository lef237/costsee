# costsee

Codex、Claude Code、OpenCode の「利用枠を何％使ったか」を一画面で確認する
Rust 製 TUI です。

```text
 COSTSEE  allowance dashboard   most constrained 41% used

 ┌ Codex · team ────────────────────────────────────────────────┐
 │ 7 days  resets Wed 20:59                                    │
 │ ███████░░░░░░░░░░░░░░░░░░░░░░░░░░░  20% used · 80% left   │
 └──────────────────────────────────────────────────────────────┘
```

## 対応する利用枠

- Codex: CLI がローカルログへ記録する5時間・7日間などの公式消化率
- Claude Code: 公式ステータスラインが提供する5時間・7日間の消化率
- OpenCode: 今月の記録済みコスト ÷ 設定した月間上限

OpenCode は複数のプロバイダーを利用でき、共通の契約枠が存在しません。そのため
OpenCode Zen やワークスペースで設定した月間 USD 上限を設定ファイルへ記載します。

## インストール

```console
cargo install --path .
costsee
```

`r` で再読込、`q` または `Esc` で終了します。

## 初期設定

Codex は追加設定なしで検出します。Claude Code の公式利用率フィードを接続します。

```console
costsee setup
```

これは `~/.claude/settings.json` に `statusLine` が未設定の場合だけ設定を追加します。
既存のステータスラインは上書きしません。接続後、Claude Code で一度メッセージを
送ると利用率が保存されます。

OpenCode の月間上限を設定する場合:

```console
costsee init
```

生成された設定の `[allowances]` に追記します。

```toml
[allowances]
opencode_monthly_usd = 20.0
```

標準の設定場所は macOS では
`~/Library/Application Support/costsee/config.toml`、Linux では
`~/.config/costsee/config.toml` です。`--config` で別の場所も指定できます。

## 非対話レポート

```console
costsee report
```

```text
tool        window   used   reset                       status
Codex       7 days   2.0%   2026-08-05T20:59:27+09:00  ok
Claude Code 5 hours  23.5%  2026-07-29T22:00:00+09:00  ok
```

## プライバシー

- Codex と OpenCode のデータは読み取り専用で取得します。
- Claude Code からは利用率とリセット時刻を含むステータスライン JSON をローカルに
  キャッシュします。
- API キーや会話本文は読み取らず、外部サーバーへの独自通信も行いません。
- 利用率は各 CLI が最後に報告した値です。最新化するには各 CLI で一度通信した後、
  costsee で `r` を押してください。
