use std::{env, path::PathBuf, sync::Arc};

use anyhow::{Result, bail};
use codex_github_bridge::{AcpActions, link_pull_request, webhook_router};

fn default_state_file() -> PathBuf {
    let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join("Library/Application Support/codex-github-bridge/state.json")
}

fn default_relay_dir() -> PathBuf {
    default_state_file().parent().unwrap().join("relays")
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    match (args.next().as_deref(), args.next()) {
        (Some("link"), Some(url)) => {
            let state_file = env::var_os("BRIDGE_STATE_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(default_state_file);
            let thread_id = env::var("CODEX_THREAD_ID").unwrap_or_default();
            let cwd = env::current_dir()?;
            let key = link_pull_request(&url, &state_file, &thread_id, &cwd)?;
            println!("Linked {key} to {thread_id}");
            Ok(())
        }
        (Some("serve"), None) => {
            let secret = env::var("WEBHOOK_SECRET")
                .ok()
                .filter(|secret| !secret.is_empty())
                .ok_or_else(|| anyhow::anyhow!("WEBHOOK_SECRET is required"))?;
            let port: u16 = env::var("PORT").unwrap_or_else(|_| "8787".into()).parse()?;
            let state_file = env::var_os("BRIDGE_STATE_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(default_state_file);
            let relay_dir = env::var_os("BRIDGE_RELAY_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(default_relay_dir);
            let router = webhook_router(secret, state_file, Arc::new(AcpActions::new(relay_dir)));
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
            println!("Listening on http://127.0.0.1:{port}");
            axum::serve(listener, router).await?;
            Ok(())
        }
        _ => bail!("Usage: bridge <serve|link> [argument]"),
    }
}
