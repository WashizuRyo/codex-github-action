use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use axum::{
    Router,
    body::Bytes,
    extract::State as AxumState,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    process::Command,
    time::timeout,
};
use url::Url;

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct State {
    #[serde(default)]
    pub links: BTreeMap<String, Link>,
    #[serde(default)]
    pub deliveries: Vec<String>,
    #[serde(
        default,
        rename = "deliveryTimestamps",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub delivery_timestamps: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    pub thread_id: String,
    pub cwd: String,
}

pub struct Delivery {
    pub event: String,
    pub delivery_id: String,
    pub signature: String,
    pub body: Vec<u8>,
}

#[derive(Debug, Serialize)]
pub struct DeliveryResult {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

#[async_trait]
pub trait Actions: Send + Sync {
    async fn resume_codex(&self, link: &Link, prompt: &str) -> Result<()>;
    async fn review_has_comments(
        &self,
        repository: &str,
        pull_number: u64,
        review_id: u64,
    ) -> Result<bool>;
}

pub struct AcpActions {
    relay_dir: PathBuf,
    timeout: Duration,
}

impl AcpActions {
    pub fn new(relay_dir: PathBuf) -> Self {
        Self {
            relay_dir,
            timeout: Duration::from_secs(5),
        }
    }
}

#[async_trait]
impl Actions for AcpActions {
    async fn resume_codex(&self, link: &Link, prompt: &str) -> Result<()> {
        let mapping_path = self.relay_dir.join(format!("{}.json", link.thread_id));
        let mapping: Value = match tokio::fs::read(&mapping_path).await {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                bail!("No active ACP relay for Codex session {}", link.thread_id)
            }
            Err(error) => return Err(error.into()),
        };
        let socket_path = mapping["socketPath"]
            .as_str()
            .context("ACP relay mapping has no socketPath")?;
        let request = serde_json::to_vec(&serde_json::json!({
            "sessionId": link.thread_id,
            "prompt": prompt
        }))?;
        timeout(self.timeout, async {
            let mut stream = UnixStream::connect(socket_path).await?;
            stream.write_all(&request).await?;
            stream.write_all(b"\n").await?;
            stream.shutdown().await?;
            let mut response = Vec::new();
            stream.read_to_end(&mut response).await?;
            let response: Value =
                serde_json::from_slice(&response).context("Invalid ACP relay response")?;
            if let Some(error) = response["error"].as_str() {
                bail!(error.to_owned());
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("ACP relay timed out after 5000ms")??;
        Ok(())
    }

    async fn review_has_comments(
        &self,
        repository: &str,
        pull_number: u64,
        review_id: u64,
    ) -> Result<bool> {
        let endpoint = format!(
            "repos/{repository}/pulls/{pull_number}/reviews/{review_id}/comments?per_page=1"
        );
        let output = Command::new("gh").args(["api", &endpoint]).output().await?;
        if !output.status.success() {
            bail!("gh api failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        let comments: Vec<Value> = serde_json::from_slice(&output.stdout)?;
        Ok(!comments.is_empty())
    }
}

pub async fn handle_delivery(
    delivery: Delivery,
    secret: &str,
    state_file: &Path,
    actions: &(impl Actions + ?Sized),
) -> Result<DeliveryResult> {
    if !valid_signature(secret, &delivery.body, &delivery.signature) {
        return Ok(DeliveryResult {
            status: "rejected",
            reason: Some("invalid signature"),
        });
    }

    let payload: Value = serde_json::from_slice(&delivery.body)?;

    let (pull_number, prompt) = match delivery.event.as_str() {
        "workflow_run"
            if payload["action"] == "completed"
                && payload["workflow_run"]["conclusion"] == "failure"
                && payload["workflow_run"]["pull_requests"][0]["number"].is_u64() =>
        {
            let run = &payload["workflow_run"];
            let pull_number = run["pull_requests"][0]["number"].as_u64().unwrap();
            let prompt = format!(
                "GitHub Actions run {} failed for {}#{pull_number}. Inspect {}, fix the failure, run the relevant tests, commit, and push the fix.",
                run["id"].as_u64().unwrap_or_default(),
                payload["repository"]["full_name"]
                    .as_str()
                    .unwrap_or_default(),
                run["html_url"].as_str().unwrap_or_default()
            );
            (pull_number, prompt)
        }
        "pull_request_review"
            if payload["action"] == "submitted"
                && matches!(
                    payload["review"]["user"]["login"].as_str(),
                    Some("coderabbitai" | "coderabbitai[bot]")
                ) =>
        {
            let repository = payload["repository"]["full_name"]
                .as_str()
                .unwrap_or_default();
            let pull_number = payload["pull_request"]["number"]
                .as_u64()
                .unwrap_or_default();
            let review_id = payload["review"]["id"].as_u64().unwrap_or_default();
            if !actions
                .review_has_comments(repository, pull_number, review_id)
                .await?
            {
                return Ok(DeliveryResult {
                    status: "ignored",
                    reason: Some("review has no comments"),
                });
            }
            let prompt = format!(
                "CodeRabbit review {review_id} has inline comments on {}. Verify the comments against the current code, fix valid findings, run the relevant tests, commit, and push the fix.",
                payload["pull_request"]["html_url"]
                    .as_str()
                    .unwrap_or_default()
            );
            (pull_number, prompt)
        }
        _ => {
            return Ok(DeliveryResult {
                status: "ignored",
                reason: None,
            });
        }
    };

    let repository = payload["repository"]["full_name"]
        .as_str()
        .unwrap_or_default();
    let key = format!("{repository}#{pull_number}");
    let _state_lock = lock_state_async(state_file.to_owned()).await?;
    let mut state = read_state(state_file)?;
    let now = unix_timestamp();
    let state_changed = prune_deliveries(&mut state, now);
    if state.deliveries.contains(&delivery.delivery_id) {
        if state_changed {
            write_state(state_file, &state)?;
        }
        return Ok(DeliveryResult {
            status: "ignored",
            reason: Some("duplicate delivery"),
        });
    }
    let Some(link) = state.links.get(&key) else {
        return Ok(DeliveryResult {
            status: "ignored",
            reason: Some("PR is not linked"),
        });
    };

    actions.resume_codex(link, &prompt).await?;
    state.deliveries.push(delivery.delivery_id.clone());
    state.delivery_timestamps.insert(delivery.delivery_id, now);
    write_state(state_file, &state)?;
    Ok(DeliveryResult {
        status: "processed",
        reason: None,
    })
}

#[derive(Clone)]
struct WebhookState {
    secret: String,
    state_file: PathBuf,
    actions: Arc<dyn Actions>,
}

pub fn webhook_router(secret: String, state_file: PathBuf, actions: Arc<dyn Actions>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok\n" }))
        .route("/github/webhook", post(receive_webhook))
        .fallback(|| async { (StatusCode::NOT_FOUND, "not found\n") })
        .with_state(WebhookState {
            secret,
            state_file,
            actions,
        })
}

async fn receive_webhook(
    AxumState(state): AxumState<WebhookState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    let delivery = Delivery {
        event: header("x-github-event"),
        delivery_id: header("x-github-delivery"),
        signature: header("x-hub-signature-256"),
        body: body.to_vec(),
    };
    match handle_delivery(
        delivery,
        &state.secret,
        &state.state_file,
        state.actions.as_ref(),
    )
    .await
    {
        Ok(result) => {
            let status = if result.status == "rejected" {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::ACCEPTED
            };
            (status, axum::Json(result)).into_response()
        }
        Err(error) => {
            eprintln!("{error:#}");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error\n").into_response()
        }
    }
}

fn valid_signature(secret: &str, body: &[u8], signature: &str) -> bool {
    let Some(hex_signature) = signature.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(signature) = hex::decode(hex_signature) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&signature).is_ok()
}

pub fn link_pull_request(
    pull_request_url: &str,
    state_file: &Path,
    thread_id: &str,
    cwd: &Path,
) -> Result<String> {
    if thread_id.is_empty() {
        bail!("CODEX_THREAD_ID is required");
    }
    let url = Url::parse(pull_request_url).context("Expected a GitHub pull request URL")?;
    let path = url.path().strip_suffix('/').unwrap_or(url.path());
    let segments: Vec<_> = path.trim_start_matches('/').split('/').collect();
    if url.host_str() != Some("github.com")
        || segments.len() != 4
        || segments[2] != "pull"
        || segments[3].parse::<u64>().is_err()
    {
        bail!("Expected a GitHub pull request URL");
    }

    let key = format!("{}/{}#{}", segments[0], segments[1], segments[3]);
    let _state_lock = lock_state(state_file)?;
    let mut state = read_state(state_file)?;
    state.links.insert(
        key.clone(),
        Link {
            thread_id: thread_id.to_owned(),
            cwd: cwd.to_string_lossy().into_owned(),
        },
    );
    write_state(state_file, &state)?;
    Ok(key)
}

pub fn read_state(path: &Path) -> Result<State> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
        Err(error) => Err(error.into()),
    }
}

pub fn write_state(path: &Path, state: &State) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut bytes = serde_json::to_vec_pretty(state)?;
    bytes.push(b'\n');
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.json");
    let temporary = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

const DELIVERY_RETENTION_SECONDS: u64 = 3 * 24 * 60 * 60;

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn prune_deliveries(state: &mut State, now: u64) -> bool {
    let original_deliveries = state.deliveries.clone();
    let original_timestamps = state.delivery_timestamps.clone();
    state.deliveries.retain(|delivery_id| {
        state
            .delivery_timestamps
            .get(delivery_id)
            .is_none_or(|timestamp| now.saturating_sub(*timestamp) <= DELIVERY_RETENTION_SECONDS)
    });
    state
        .delivery_timestamps
        .retain(|delivery_id, _| state.deliveries.contains(delivery_id));
    for delivery_id in &state.deliveries {
        state
            .delivery_timestamps
            .entry(delivery_id.clone())
            .or_insert(now);
    }
    state.deliveries != original_deliveries || state.delivery_timestamps != original_timestamps
}

fn lock_state(path: &Path) -> Result<fs::File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut lock_name = path.as_os_str().to_owned();
    lock_name.push(".lock");
    let mut options = fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(PathBuf::from(lock_name))?;
    fs2::FileExt::lock_exclusive(&lock)?;
    Ok(lock)
}

async fn lock_state_async(path: PathBuf) -> Result<fs::File> {
    tokio::task::spawn_blocking(move || lock_state(&path)).await?
}
