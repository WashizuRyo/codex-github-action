# codex-github-bridge

GitHub Actionsの失敗とCodeRabbitのインラインレビューを、そのPRを作ったCodexセッションへ送る小さなローカルWebhookサーバーです。

## 1. インストール

```bash
brew install cloudflared
npm install
npm link
cp .env.example .env
```

Webhook secretを作成し、出力を `.env` の `WEBHOOK_SECRET` に設定します。

```bash
openssl rand -hex 32
```

`.env` には、この環境をセルフホストするユーザー自身の設定を記入します。

```dotenv
WEBHOOK_SECRET=<GitHub Webhookと共有するsecret>
PORT=8787
CLOUDFLARE_WORKER_NAME=<デプロイするWorker名>
CLOUDFLARE_TUNNEL_NAME=<管理型Tunnel名またはID>
CLOUDFLARE_VPC_SERVICE_ID=<localhost:8787を指すVPC Service ID>
```

`.env` はGit管理されません。

## 2. 起動

Zedの `settings.json` でCodexをリレー経由にします。

```json
"codex-bridge": {
  "type": "custom",
  "command": "bridge-acp-relay",
  "args": ["codex-acp"],
  "default_config_options": { "mode": "agent-full-access" }
}
```

設定後に新しいCodexセッションを開きます。

```bash
npm start
```

デフォルトでは `127.0.0.1:8787` で待ち受けます。状態は
`~/Library/Application Support/codex-github-bridge/state.json` に保存されます。

## 3. Cloudflare Tunnel

Webhookの公開URLには固定のCloudflare Workers URLを使います。独自ドメインは不要です。

Cloudflareアカウントで、以下のリソースを作成します。

- 管理型Cloudflare Tunnel
- そのTunnelを使って `localhost:8787` を指すHTTP VPC Service
- このリポジトリからデプロイするWorker

Wranglerへログインし、管理型Tunnelを作成します。Dashboardから作成しても構いません。

```bash
npx wrangler login
npx wrangler tunnel create <TUNNEL_NAME>
```

表示されたTunnel IDを使ってVPC Serviceを作成します。

```bash
npx wrangler vpc service create <VPC_SERVICE_NAME> \
  --type http \
  --tunnel-id <TUNNEL_ID> \
  --hostname localhost \
  --http-port 8787
```

作成したTunnel名とVPC Service IDを `.env` に設定してください。npmスクリプトは
`wrangler.example.jsonc` と `.env` からgit管理外の `.wrangler.generated.jsonc` を生成します。

`cloudflared` がシステムサービスとして動いていない場合は、別のターミナルで管理型Tunnelを起動します。

```bash
npm run tunnel
```

Workerをデプロイします。

```bash
npm run worker:deploy
```

デプロイ結果に表示された固定URLを、GitHub Webhookで使用します。

```text
https://<CLOUDFLARE_WORKER_NAME>.<Cloudflareアカウントのサブドメイン>.workers.dev
```

## 4. GitHub Webhook

対象リポジトリの **Settings > Webhooks > Add webhook** で設定します。

- Payload URL: デプロイ結果の固定URLに `/github/webhook` を付けたもの
- Content type: `application/json`
- Secret: `.env` の `WEBHOOK_SECRET` と同じ値
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
- Workerは `POST /github/webhook` と `GET /healthz` 以外を拒否します。
- Workerはリクエスト本文とGitHub署名ヘッダーを変更せず、VPC Service経由でローカルサーバーへ転送します。

## 設定

| 環境変数 | 既定値 |
| --- | --- |
| `WEBHOOK_SECRET` | 必須 |
| `PORT` | `8787` |
| `BRIDGE_STATE_FILE` | `~/Library/Application Support/codex-github-bridge/state.json` |
| `CLOUDFLARE_WORKER_NAME` | 必須。デプロイするWorker名 |
| `CLOUDFLARE_TUNNEL_NAME` | 必須。管理型Tunnel名またはID |
| `CLOUDFLARE_VPC_SERVICE_ID` | 必須。WorkerへbindingするVPC Service ID |

## テスト

```bash
npm test
npm run worker:check
```
