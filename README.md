# costsee

Codex、Claude Code、OpenCode の「利用枠を何％使ったか」を一画面で確認する
Rust 製 TUI です。

```text
 COSTSEE  allowance dashboard   most constrained 41% used

 ┌ Codex · team ────────────────────────────────────────────────┐
 │ period  7 days | left 5d 20h                                │
 │ ███████░░░░░░░░░░░░░░░░░░░░░░░░░░░  20% used · 80% left   │
 │ reset   2026-08-05 20:59                                    │
 └──────────────────────────────────────────────────────────────┘
```

## 対応する利用枠

- Codex: 公式 App Server が提供する5時間・7日間などの消化率
- Claude Code: 公式ステータスラインが提供する5時間・7日間の消化率
- OpenCode: ローカル履歴の記録済みコスト ÷ 設定した上限

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

以前のバージョンから更新した場合も `costsee setup` を再実行してください。
アイドル中の古いClaude Codeセッションによる定期更新を無効化します。

OpenCode Go の期間別上限を設定する場合:

```console
costsee init
```

生成された設定に追記します。以下は OpenCode Go の公式上限です。

```toml
[allowances.opencode_go]
five_hour_usd = 12.0
weekly_usd = 30.0
monthly_usd = 60.0
```

OpenCode Go のローカル履歴だけを対象に、直近5時間、直近7日間、今月の記録済み
コストから利用率を推定します。5時間・週間枠の正確なリセット時刻はローカル履歴から
取得できないため、画面では `unknown` と表示します。既存の
`allowances.opencode_monthly_usd` も月間表示用として引き続き利用できます。

標準の設定場所はOSによらず `~/.config/costsee/config.toml` です。
`--config` で別の場所も指定できます。
`codex` が `PATH` にない場合は、設定の `[commands]` に実行ファイルのパスを指定できます。

```toml
[commands]
codex = "/path/to/codex"
```

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

- Codex の公式 App Server をstdioで一時起動し、`account/rateLimits/read` の結果だけを
  取得します。セッションログや認証情報は読み取りません。
- OpenCode のローカルSQLiteデータベースから、時刻・provider・costだけを読み取り専用で
  集計します。会話本文は取得しません。
- Claude Code の公式ステータスライン入力から、利用率とリセット時刻だけをローカルに
  キャッシュします。作業ディレクトリ、セッションID、transcriptパスなどは保存しません。
- API キーは取得せず、非公開APIへのアクセスやテレメトリー送信も行いません。Codex
  App Serverによる公式利用率の取得は、Codex自身の既存認証を使用します。
- OpenCodeについては、会話本文を取得せず期間別集計の精度を保てる公式APIがないため、
  ローカルDBアクセスを隔離して使用しています。未対応の形式では推測値を表示しません。
- 利用率は各 CLI が最後に報告した値です。Claude Code のキャッシュ更新は起動中の
  costsee に自動反映されます。ほかの値は各 CLI で一度通信した後、costsee で `r` を
  押してください。
