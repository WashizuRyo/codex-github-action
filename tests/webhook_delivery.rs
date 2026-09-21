use std::{
    fs,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use codex_github_bridge::{Actions, Delivery, Link, handle_delivery, webhook_router};
use hmac::{Hmac, Mac};
use sha2::Sha256;

#[derive(Default)]
struct RecordingActions {
    resumed: Mutex<Vec<(Link, String)>>,
    reviews: Mutex<Vec<(String, u64, u64)>>,
    has_comments: bool,
    fail_resume: bool,
}

#[derive(Default)]
struct SlowActions {
    resume_count: AtomicUsize,
}

#[async_trait]
impl Actions for SlowActions {
    async fn resume_codex(&self, _link: &Link, _prompt: &str) -> anyhow::Result<()> {
        self.resume_count.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok(())
    }

    async fn review_has_comments(
        &self,
        _repository: &str,
        _pull_number: u64,
        _review_id: u64,
    ) -> anyhow::Result<bool> {
        Ok(false)
    }
}

#[async_trait]
impl Actions for RecordingActions {
    async fn resume_codex(&self, link: &Link, prompt: &str) -> anyhow::Result<()> {
        self.resumed
            .lock()
            .unwrap()
            .push((link.clone(), prompt.to_owned()));
        if self.fail_resume {
            anyhow::bail!("Codex failed");
        }
        Ok(())
    }

    async fn review_has_comments(
        &self,
        repository: &str,
        pull_number: u64,
        review_id: u64,
    ) -> anyhow::Result<bool> {
        self.reviews
            .lock()
            .unwrap()
            .push((repository.to_owned(), pull_number, review_id));
        Ok(self.has_comments)
    }
}

#[tokio::test]
async fn coderabbit_review_with_inline_comments_resumes_the_linked_session() {
    let directory = tempfile::tempdir().unwrap();
    let state_file = directory.path().join("state.json");
    fs::write(
        &state_file,
        serde_json::to_vec(&serde_json::json!({
            "links": {
                "WashizuRyo/menu#27": {
                    "threadId": "thread-27",
                    "cwd": "/workspace/menu"
                }
            },
            "deliveries": []
        }))
        .unwrap(),
    )
    .unwrap();
    let body = serde_json::to_vec(&serde_json::json!({
        "action": "submitted",
        "repository": { "full_name": "WashizuRyo/menu" },
        "pull_request": {
            "number": 27,
            "html_url": "https://github.com/WashizuRyo/menu/pull/27"
        },
        "review": { "id": 5254559716_u64, "user": { "login": "coderabbitai[bot]" } }
    }))
    .unwrap();
    let secret = "webhook-secret";
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(&body);
    let actions = RecordingActions {
        has_comments: true,
        ..Default::default()
    };

    let result = handle_delivery(
        Delivery {
            event: "pull_request_review".into(),
            delivery_id: "delivery-2".into(),
            signature: format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
            body,
        },
        secret,
        &state_file,
        &actions,
    )
    .await
    .unwrap();

    assert_eq!(result.status, "processed");
    assert_eq!(
        *actions.reviews.lock().unwrap(),
        vec![("WashizuRyo/menu".into(), 27, 5254559716)]
    );
    let resumed = actions.resumed.lock().unwrap();
    assert_eq!(resumed[0].0.thread_id, "thread-27");
    assert!(resumed[0].1.contains("CodeRabbit review 5254559716"));
}

#[tokio::test]
async fn signed_failed_ci_resumes_the_linked_session() {
    let directory = tempfile::tempdir().unwrap();
    let state_file = directory.path().join("state.json");
    fs::write(
        &state_file,
        serde_json::to_vec(&serde_json::json!({
            "links": {
                "WashizuRyo/menu#27": {
                    "threadId": "thread-27",
                    "cwd": "/workspace/menu"
                }
            },
            "deliveries": []
        }))
        .unwrap(),
    )
    .unwrap();
    let body = serde_json::to_vec(&serde_json::json!({
        "action": "completed",
        "repository": { "full_name": "WashizuRyo/menu" },
        "workflow_run": {
            "id": 1234,
            "conclusion": "failure",
            "html_url": "https://github.com/WashizuRyo/menu/actions/runs/1234",
            "pull_requests": [{ "number": 27 }]
        }
    }))
    .unwrap();
    let secret = "webhook-secret";
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(&body);
    let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
    let actions = RecordingActions::default();

    let result = handle_delivery(
        Delivery {
            event: "workflow_run".into(),
            delivery_id: "delivery-1".into(),
            signature,
            body,
        },
        secret,
        &state_file,
        &actions,
    )
    .await
    .unwrap();

    assert_eq!(result.status, "processed");
    let resumed = actions.resumed.lock().unwrap();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].0.thread_id, "thread-27");
    assert!(resumed[0].1.contains("1234"));
    let state: serde_json::Value = serde_json::from_slice(&fs::read(state_file).unwrap()).unwrap();
    assert_eq!(state["deliveries"], serde_json::json!(["delivery-1"]));
}

#[tokio::test]
async fn webhook_http_endpoint_accepts_a_signed_delivery() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let directory = tempfile::tempdir().unwrap();
    let state_file = directory.path().join("state.json");
    fs::write(
        &state_file,
        serde_json::to_vec(&serde_json::json!({
            "links": {
                "WashizuRyo/menu#27": { "threadId": "thread-27", "cwd": "/workspace/menu" }
            },
            "deliveries": []
        }))
        .unwrap(),
    )
    .unwrap();
    let body = serde_json::to_vec(&serde_json::json!({
        "action": "completed",
        "repository": { "full_name": "WashizuRyo/menu" },
        "workflow_run": {
            "id": 1234,
            "conclusion": "failure",
            "html_url": "https://github.com/WashizuRyo/menu/actions/runs/1234",
            "pull_requests": [{ "number": 27 }]
        }
    }))
    .unwrap();
    let secret = "webhook-secret";
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(&body);
    let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
    let actions = Arc::new(RecordingActions::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = webhook_router(secret.into(), state_file, actions.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await });

    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let request = format!(
        "POST /github/webhook HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nX-GitHub-Event: workflow_run\r\nX-GitHub-Delivery: delivery-http\r\nX-Hub-Signature-256: {signature}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.write_all(&body).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();

    assert!(response.starts_with("HTTP/1.1 202 Accepted"), "{response}");
    assert_eq!(actions.resumed.lock().unwrap().len(), 1);
    server.abort();
}

#[tokio::test]
async fn invalid_webhook_signature_is_rejected_without_resuming() {
    let directory = tempfile::tempdir().unwrap();
    let actions = RecordingActions::default();
    let result = handle_delivery(
        Delivery {
            event: "workflow_run".into(),
            delivery_id: "delivery-invalid".into(),
            signature: "sha256=invalid".into(),
            body: b"{}".to_vec(),
        },
        "webhook-secret",
        &directory.path().join("state.json"),
        &actions,
    )
    .await
    .unwrap();

    assert_eq!(result.status, "rejected");
    assert!(actions.resumed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_same_delivery_is_processed_only_once() {
    let directory = tempfile::tempdir().unwrap();
    let state_file = directory.path().join("state.json");
    fs::write(
        &state_file,
        serde_json::to_vec(&serde_json::json!({
            "links": { "WashizuRyo/menu#27": { "threadId": "thread-27", "cwd": "/workspace/menu" } },
            "deliveries": []
        })).unwrap(),
    ).unwrap();
    let body = serde_json::to_vec(&serde_json::json!({
        "action": "completed",
        "repository": { "full_name": "WashizuRyo/menu" },
        "workflow_run": {
            "id": 1234, "conclusion": "failure", "html_url": "https://example.test/run",
            "pull_requests": [{ "number": 27 }]
        }
    }))
    .unwrap();
    let secret = "webhook-secret";
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(&body);
    let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
    let actions = RecordingActions::default();

    for expected in ["processed", "ignored"] {
        let result = handle_delivery(
            Delivery {
                event: "workflow_run".into(),
                delivery_id: "same-delivery".into(),
                signature: signature.clone(),
                body: body.clone(),
            },
            secret,
            &state_file,
            &actions,
        )
        .await
        .unwrap();
        assert_eq!(result.status, expected);
    }
    assert_eq!(actions.resumed.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn concurrent_copies_of_a_delivery_resume_only_once() {
    let directory = tempfile::tempdir().unwrap();
    let state_file = directory.path().join("state.json");
    fs::write(
        &state_file,
        serde_json::to_vec(&serde_json::json!({
            "links": { "WashizuRyo/menu#27": { "threadId": "thread-27", "cwd": "/workspace/menu" } },
            "deliveries": []
        }))
        .unwrap(),
    )
    .unwrap();
    let body = serde_json::to_vec(&serde_json::json!({
        "action": "completed",
        "repository": { "full_name": "WashizuRyo/menu" },
        "workflow_run": {
            "id": 1234, "conclusion": "failure", "html_url": "https://example.test/run",
            "pull_requests": [{ "number": 27 }]
        }
    }))
    .unwrap();
    let secret = "webhook-secret";
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(&body);
    let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
    let actions = SlowActions::default();
    let barrier = tokio::sync::Barrier::new(3);
    let make_delivery = || Delivery {
        event: "workflow_run".into(),
        delivery_id: "concurrent-delivery".into(),
        signature: signature.clone(),
        body: body.clone(),
    };
    let first = async {
        barrier.wait().await;
        handle_delivery(make_delivery(), secret, &state_file, &actions).await
    };
    let second = async {
        barrier.wait().await;
        handle_delivery(make_delivery(), secret, &state_file, &actions).await
    };
    let (_, (first, second)) = tokio::join!(barrier.wait(), async { tokio::join!(first, second) });
    let statuses = [first.unwrap().status, second.unwrap().status];

    assert!(statuses.contains(&"processed"));
    assert!(statuses.contains(&"ignored"));
    assert_eq!(actions.resume_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn delivery_ids_are_not_evicted_after_one_hundred_newer_deliveries() {
    let directory = tempfile::tempdir().unwrap();
    let state_file = directory.path().join("state.json");
    let deliveries: Vec<_> = (0..100).map(|index| format!("delivery-{index}")).collect();
    fs::write(
        &state_file,
        serde_json::to_vec(&serde_json::json!({
            "links": { "WashizuRyo/menu#27": { "threadId": "thread-27", "cwd": "/workspace/menu" } },
            "deliveries": deliveries
        }))
        .unwrap(),
    )
    .unwrap();
    let body = serde_json::to_vec(&serde_json::json!({
        "action": "completed",
        "repository": { "full_name": "WashizuRyo/menu" },
        "workflow_run": {
            "id": 1234, "conclusion": "failure", "html_url": "https://example.test/run",
            "pull_requests": [{ "number": 27 }]
        }
    }))
    .unwrap();
    let secret = "webhook-secret";
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(&body);
    let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
    let actions = RecordingActions::default();

    for (delivery_id, expected) in [("delivery-100", "processed"), ("delivery-0", "ignored")] {
        let result = handle_delivery(
            Delivery {
                event: "workflow_run".into(),
                delivery_id: delivery_id.into(),
                signature: signature.clone(),
                body: body.clone(),
            },
            secret,
            &state_file,
            &actions,
        )
        .await
        .unwrap();
        assert_eq!(result.status, expected);
    }
    assert_eq!(actions.resumed.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn delivery_ids_older_than_three_days_are_pruned() {
    use std::time::{SystemTime, UNIX_EPOCH};

    let directory = tempfile::tempdir().unwrap();
    let state_file = directory.path().join("state.json");
    let expired = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - (3 * 24 * 60 * 60 + 1);
    fs::write(
        &state_file,
        serde_json::to_vec(&serde_json::json!({
            "links": { "WashizuRyo/menu#27": { "threadId": "thread-27", "cwd": "/workspace/menu" } },
            "deliveries": ["expired-delivery"],
            "deliveryTimestamps": { "expired-delivery": expired }
        }))
        .unwrap(),
    )
    .unwrap();
    let body = serde_json::to_vec(&serde_json::json!({
        "action": "completed",
        "repository": { "full_name": "WashizuRyo/menu" },
        "workflow_run": {
            "id": 1234, "conclusion": "failure", "html_url": "https://example.test/run",
            "pull_requests": [{ "number": 27 }]
        }
    }))
    .unwrap();
    let secret = "webhook-secret";
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(&body);
    let actions = RecordingActions::default();

    let result = handle_delivery(
        Delivery {
            event: "workflow_run".into(),
            delivery_id: "expired-delivery".into(),
            signature: format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
            body,
        },
        secret,
        &state_file,
        &actions,
    )
    .await
    .unwrap();

    assert_eq!(result.status, "processed");
    assert_eq!(actions.resumed.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_failed_resume_leaves_the_delivery_retryable() {
    let directory = tempfile::tempdir().unwrap();
    let state_file = directory.path().join("state.json");
    fs::write(
        &state_file,
        serde_json::to_vec(&serde_json::json!({
            "links": { "WashizuRyo/menu#27": { "threadId": "thread-27", "cwd": "/workspace/menu" } },
            "deliveries": []
        })).unwrap(),
    ).unwrap();
    let body = serde_json::to_vec(&serde_json::json!({
        "action": "completed",
        "repository": { "full_name": "WashizuRyo/menu" },
        "workflow_run": {
            "id": 1234, "conclusion": "failure", "html_url": "https://example.test/run",
            "pull_requests": [{ "number": 27 }]
        }
    }))
    .unwrap();
    let secret = "webhook-secret";
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(&body);
    let actions = RecordingActions {
        fail_resume: true,
        ..Default::default()
    };

    let error = handle_delivery(
        Delivery {
            event: "workflow_run".into(),
            delivery_id: "delivery-retryable".into(),
            signature: format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
            body,
        },
        secret,
        &state_file,
        &actions,
    )
    .await
    .unwrap_err();

    assert!(error.to_string().contains("Codex failed"));
    let state: serde_json::Value = serde_json::from_slice(&fs::read(state_file).unwrap()).unwrap();
    assert_eq!(state["deliveries"], serde_json::json!([]));
}

#[tokio::test]
async fn coderabbit_review_without_inline_comments_is_ignored() {
    let directory = tempfile::tempdir().unwrap();
    let state_file = directory.path().join("state.json");
    fs::write(
        &state_file,
        serde_json::to_vec(&serde_json::json!({
            "links": { "WashizuRyo/menu#27": { "threadId": "thread-27", "cwd": "/workspace/menu" } },
            "deliveries": []
        })).unwrap(),
    ).unwrap();
    let body = serde_json::to_vec(&serde_json::json!({
        "action": "submitted",
        "repository": { "full_name": "WashizuRyo/menu" },
        "pull_request": { "number": 27, "html_url": "https://github.com/WashizuRyo/menu/pull/27" },
        "review": { "id": 99, "user": { "login": "coderabbitai[bot]" } }
    }))
    .unwrap();
    let secret = "webhook-secret";
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(&body);
    let actions = RecordingActions::default();

    let result = handle_delivery(
        Delivery {
            event: "pull_request_review".into(),
            delivery_id: "delivery-no-comments".into(),
            signature: format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
            body,
        },
        secret,
        &state_file,
        &actions,
    )
    .await
    .unwrap();

    assert_eq!(result.status, "ignored");
    assert_eq!(result.reason, Some("review has no comments"));
    assert!(actions.resumed.lock().unwrap().is_empty());
}
