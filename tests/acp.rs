use std::fs;

use codex_github_bridge::{AcpActions, Actions, Link};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn acp_action_sends_a_prompt_to_the_linked_relay() {
    let directory = tempfile::tempdir().unwrap();
    let socket_path = directory.path().join("relay.sock");
    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    fs::write(
        directory.path().join("thread-27.json"),
        serde_json::to_vec(&serde_json::json!({ "socketPath": socket_path })).unwrap(),
    )
    .unwrap();
    let received = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut line = String::new();
        BufReader::new(reader).read_line(&mut line).await.unwrap();
        writer
            .write_all(b"{\"status\":\"accepted\"}\n")
            .await
            .unwrap();
        serde_json::from_str::<serde_json::Value>(&line).unwrap()
    });

    AcpActions::new(directory.path().to_owned())
        .resume_codex(
            &Link {
                thread_id: "thread-27".into(),
                cwd: "/workspace/menu".into(),
            },
            "Webhook prompt",
        )
        .await
        .unwrap();

    let request = received.await.unwrap();
    assert_eq!(request["sessionId"], "thread-27");
    assert_eq!(request["prompt"], "Webhook prompt");
}

#[tokio::test]
async fn relay_registers_a_session_and_displays_a_webhook_prompt() {
    use std::{process::Stdio, time::Duration};
    use tokio::process::Command;

    let directory = tempfile::tempdir().unwrap();
    let mut relay = Command::new(env!("CARGO_BIN_EXE_bridge-acp-relay"))
        .args([
            "/bin/sh",
            "-c",
            "while IFS= read -r line; do printf '%s\\n' \"$line\"; done",
        ])
        .env("BRIDGE_RELAY_DIR", directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = relay.stdin.take().unwrap();
    let mut stdout = BufReader::new(relay.stdout.take().unwrap()).lines();
    stdin
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"session/prompt\",\"params\":{\"sessionId\":\"thread-27\",\"prompt\":[]}}\n",
        )
        .await
        .unwrap();
    let echoed = tokio::time::timeout(Duration::from_secs(2), stdout.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&echoed).unwrap()["id"],
        1
    );

    AcpActions::new(directory.path().to_owned())
        .resume_codex(
            &Link {
                thread_id: "thread-27".into(),
                cwd: "/workspace/menu".into(),
            },
            "Webhook prompt",
        )
        .await
        .unwrap();
    let update = tokio::time::timeout(Duration::from_secs(2), stdout.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let update: serde_json::Value = serde_json::from_str(&update).unwrap();
    assert_eq!(update["method"], "session/update");
    assert_eq!(update["params"]["sessionId"], "thread-27");
    assert_eq!(
        update["params"]["update"]["content"]["text"],
        "Webhook prompt"
    );

    relay.kill().await.unwrap();
    relay.wait().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn relay_removes_its_session_mapping_on_sigterm() {
    use std::{process::Stdio, time::Duration};
    use tokio::process::Command;

    let directory = tempfile::tempdir().unwrap();
    let mut relay = Command::new(env!("CARGO_BIN_EXE_bridge-acp-relay"))
        .arg("/bin/cat")
        .env("BRIDGE_RELAY_DIR", directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = relay.stdin.take().unwrap();
    let mut stdout = BufReader::new(relay.stdout.take().unwrap()).lines();
    stdin
        .write_all(b"{\"id\":1,\"params\":{\"sessionId\":\"thread-cleanup\"}}\n")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), stdout.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let mapping = directory.path().join("thread-cleanup.json");
    assert!(mapping.exists());

    Command::new("kill")
        .args(["-TERM", &relay.id().unwrap().to_string()])
        .status()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), relay.wait())
        .await
        .unwrap()
        .unwrap();

    assert!(!mapping.exists());
}

#[tokio::test]
async fn relay_exits_and_removes_mappings_when_client_input_closes() {
    use std::{process::Stdio, time::Duration};
    use tokio::process::Command;

    let directory = tempfile::tempdir().unwrap();
    let mut relay = Command::new(env!("CARGO_BIN_EXE_bridge-acp-relay"))
        .arg("/bin/cat")
        .env("BRIDGE_RELAY_DIR", directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = relay.stdin.take().unwrap();
    let mut stdout = BufReader::new(relay.stdout.take().unwrap()).lines();
    stdin
        .write_all(b"{\"id\":1,\"params\":{\"sessionId\":\"thread-eof\"}}\n")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), stdout.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let mapping = directory.path().join("thread-eof.json");
    assert!(mapping.exists());

    drop(stdin);
    tokio::time::timeout(Duration::from_secs(2), relay.wait())
        .await
        .unwrap()
        .unwrap();

    assert!(!mapping.exists());
}

#[tokio::test]
async fn relay_publishes_a_session_mapping_only_once() {
    use std::{process::Stdio, time::Duration};
    use tokio::process::Command;

    let directory = tempfile::tempdir().unwrap();
    let mut relay = Command::new(env!("CARGO_BIN_EXE_bridge-acp-relay"))
        .arg("/bin/cat")
        .env("BRIDGE_RELAY_DIR", directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = relay.stdin.take().unwrap();
    let mut stdout = BufReader::new(relay.stdout.take().unwrap()).lines();
    let message = b"{\"id\":1,\"params\":{\"sessionId\":\"thread-once\"}}\n";
    stdin.write_all(message).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), stdout.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let mapping = directory.path().join("thread-once.json");
    let first_modified = fs::metadata(&mapping).unwrap().modified().unwrap();

    tokio::time::sleep(Duration::from_millis(20)).await;
    stdin.write_all(message).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), stdout.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    assert_eq!(
        fs::metadata(mapping).unwrap().modified().unwrap(),
        first_modified
    );
    relay.kill().await.unwrap();
    relay.wait().await.unwrap();
}
