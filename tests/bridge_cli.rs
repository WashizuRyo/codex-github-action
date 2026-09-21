use std::{fs, process::Command};

#[test]
fn link_stores_the_current_codex_session_for_a_pull_request() {
    let directory = tempfile::tempdir().unwrap();
    let state_file = directory.path().join("state.json");

    let output = Command::new(env!("CARGO_BIN_EXE_bridge"))
        .args(["link", "https://github.com/WashizuRyo/menu/pull/27"])
        .current_dir(directory.path())
        .env("CODEX_THREAD_ID", "thread-27")
        .env("BRIDGE_STATE_FILE", &state_file)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("WashizuRyo/menu#27"));
    let state: serde_json::Value = serde_json::from_slice(&fs::read(state_file).unwrap()).unwrap();
    assert_eq!(
        state["links"]["WashizuRyo/menu#27"]["threadId"],
        "thread-27"
    );
    assert_eq!(
        state["links"]["WashizuRyo/menu#27"]["cwd"],
        fs::canonicalize(directory.path())
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert_eq!(state["deliveries"], serde_json::json!([]));
}

#[test]
fn serve_exposes_the_health_endpoint() {
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        thread,
        time::Duration,
    };

    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut child = Command::new(env!("CARGO_BIN_EXE_bridge"))
        .arg("serve")
        .env("WEBHOOK_SECRET", "webhook-secret")
        .env("PORT", port.to_string())
        .env("BRIDGE_STATE_FILE", directory.path().join("state.json"))
        .env("BRIDGE_RELAY_DIR", directory.path().join("relays"))
        .spawn()
        .unwrap();

    let mut stream = (0..50)
        .find_map(|_| match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => Some(stream),
            Err(_) => {
                thread::sleep(Duration::from_millis(20));
                None
            }
        })
        .expect("bridge serve did not listen");
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    child.kill().unwrap();
    child.wait().unwrap();

    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.ends_with("ok\n"), "{response}");
}

#[test]
fn link_accepts_a_pull_request_url_with_a_trailing_slash() {
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_bridge"))
        .args(["link", "https://github.com/WashizuRyo/menu/pull/27/"])
        .current_dir(directory.path())
        .env("CODEX_THREAD_ID", "thread-27")
        .env("BRIDGE_STATE_FILE", directory.path().join("state.json"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn link_creates_a_private_state_file() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let state_file = directory.path().join("state.json");
    let output = Command::new(env!("CARGO_BIN_EXE_bridge"))
        .args(["link", "https://github.com/WashizuRyo/menu/pull/27"])
        .current_dir(directory.path())
        .env("CODEX_THREAD_ID", "thread-27")
        .env("BRIDGE_STATE_FILE", &state_file)
        .output()
        .unwrap();
    assert!(output.status.success());

    assert_eq!(
        fs::metadata(state_file).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn serve_rejects_an_empty_webhook_secret() {
    let output = Command::new(env!("CARGO_BIN_EXE_bridge"))
        .arg("serve")
        .env("WEBHOOK_SECRET", "")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("WEBHOOK_SECRET is required"));
}

#[cfg(unix)]
#[test]
fn link_replaces_the_state_file_atomically() {
    use std::os::unix::fs::MetadataExt;

    let directory = tempfile::tempdir().unwrap();
    let state_file = directory.path().join("state.json");
    fs::write(&state_file, b"{\"links\":{},\"deliveries\":[]}").unwrap();
    let original_inode = fs::metadata(&state_file).unwrap().ino();

    let output = Command::new(env!("CARGO_BIN_EXE_bridge"))
        .args(["link", "https://github.com/WashizuRyo/menu/pull/27"])
        .current_dir(directory.path())
        .env("CODEX_THREAD_ID", "thread-27")
        .env("BRIDGE_STATE_FILE", &state_file)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_ne!(fs::metadata(state_file).unwrap().ino(), original_inode);
}
