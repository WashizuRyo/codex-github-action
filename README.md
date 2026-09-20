# codex-github-bridge

GitHub Actionsの失敗とCodeRabbitのインラインレビューを、そのPRを作ったCodexセッションへ送る小さなローカルWebhookサーバーです。

## 1. インストール

```bash
npm link
brew install cloudflared
```

Webhook secretを作成します。

```bash
openssl rand -hex 32
```

## 2. 起動

```bash
npm start
```

デフォルトでは `127.0.0.1:8787` で待ち受けます。状態は
`~/Library/Application Support/codex-github-bridge/state.json` に保存されます。

## 3. Quick Tunnel

別のターミナルで起動します。Cloudflareアカウントや独自ドメインは不要です。

```bash
npm run tunnel
```

表示された `https://<ランダム文字列>.trycloudflare.com` をコピーします。Quick Tunnelを再起動するとURLが変わるため、その都度GitHub WebhookのPayload URLも更新します。

## 4. GitHub Webhook

対象リポジトリの **Settings > Webhooks > Add webhook** で設定します。

- Payload URL: `https://<ランダム文字列>.trycloudflare.com/github/webhook`
- Content type: `application/json`
- Secret: `bridge serve` に渡したものと同じ値
- Events: **Workflow runs** と **Pull request reviews** のみ

## 5. PRとの紐付け

Codexセッション内では `CODEX_THREAD_ID` が自動設定されます。

```bash
bridge link https://github.com/OWNER/REPO/pull/123
```

`/pr` スキルはPR作成後にこのコマンドを実行します。

## 動作

- PRに紐づくGitHub Actionsが失敗すると、元のCodexセッションを再開します。
- CodeRabbitがレビューをsubmitしたら、そのreview IDに属するインラインコメントをGitHub APIで確認します。コメントがある場合だけ元のCodexセッションを再開します。
- Codexには修正、テスト、commit、pushを依頼します。
- 同じGitHub delivery IDは再処理しません。

## 設定

| 環境変数 | 既定値 |
| --- | --- |
| `WEBHOOK_SECRET` | 必須 |
| `PORT` | `8787` |
| `BRIDGE_STATE_FILE` | `~/Library/Application Support/codex-github-bridge/state.json` |
| `CODEX_BIN` | ChatGPT.app同梱Codex、なければPATH上の `codex` |

## テスト

```bash
npm test
```
