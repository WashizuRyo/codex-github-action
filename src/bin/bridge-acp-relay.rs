use std::{
    collections::HashSet,
    env,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::{
    fs,
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    process::{ChildStdin, Command},
    sync::Mutex,
};
use uuid::Uuid;

fn default_relay_dir() -> PathBuf {
    let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join("Library/Application Support/codex-github-bridge/relays")
}

fn main() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run());
    runtime.shutdown_timeout(Duration::from_millis(100));
    result
}

async fn run() -> Result<()> {
    let command: Vec<String> = env::args().skip(1).collect();
    if command.is_empty() {
        bail!("ACP agent command is required");
    }
    let relay_dir = env::var_os("BRIDGE_RELAY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(default_relay_dir);
    fs::create_dir_all(&relay_dir).await?;
    set_mode(&relay_dir, 0o700).await?;
    let socket_path = relay_dir.join(format!("relay-{}.sock", std::process::id()));
    if fs::try_exists(&socket_path).await? {
        fs::remove_file(&socket_path).await?;
    }

    let mut child = Command::new(&command[0])
        .args(&command[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let child_stdin = Arc::new(Mutex::new(
        child.stdin.take().context("missing child stdin")?,
    ));
    let child_stdout = child.stdout.take().context("missing child stdout")?;
    let client_stdout = Arc::new(Mutex::new(tokio::io::stdout()));
    let bridge_ids = Arc::new(Mutex::new(HashSet::<String>::new()));
    let sessions = Arc::new(Mutex::new(HashSet::<String>::new()));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    let input_task = tokio::spawn(forward_client_input(
        relay_dir.clone(),
        socket_path.clone(),
        child_stdin.clone(),
        sessions.clone(),
    ));
    let output_task = tokio::spawn(forward_agent_output(
        child_stdout,
        client_stdout.clone(),
        bridge_ids.clone(),
    ));
    let listener = UnixListener::bind(&socket_path)?;

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (connection, _) = accepted?;
                tokio::spawn(handle_relay_request(
                    connection, child_stdin.clone(), client_stdout.clone(), bridge_ids.clone(),
                ));
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = terminate.recv() => break,
            status = child.wait() => {
                status?;
                break;
            }
        }
    }

    input_task.abort();
    output_task.abort();
    let _ = child.kill().await;
    cleanup(&relay_dir, &socket_path, &sessions).await;
    Ok(())
}

async fn forward_client_input(
    relay_dir: PathBuf,
    socket_path: PathBuf,
    child_stdin: Arc<Mutex<ChildStdin>>,
    sessions: Arc<Mutex<HashSet<String>>>,
) -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let message: Value = serde_json::from_str(&line)?;
        if let Some(session_id) = message["params"]["sessionId"].as_str() {
            if !session_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                bail!("Invalid ACP session ID");
            }
            sessions.lock().await.insert(session_id.to_owned());
            let mapping = relay_dir.join(format!("{session_id}.json"));
            fs::write(
                &mapping,
                format!("{}\n", json!({ "socketPath": socket_path })),
            )
            .await?;
            set_mode(&mapping, 0o600).await?;
        }
        let mut stdin = child_stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
    }
    Ok(())
}

async fn forward_agent_output(
    child_stdout: tokio::process::ChildStdout,
    client_stdout: Arc<Mutex<tokio::io::Stdout>>,
    bridge_ids: Arc<Mutex<HashSet<String>>>,
) -> Result<()> {
    let mut lines = BufReader::new(child_stdout).lines();
    while let Some(line) = lines.next_line().await? {
        let message: Value = serde_json::from_str(&line)?;
        let is_bridge_response = if let Some(id) = message["id"].as_str() {
            bridge_ids.lock().await.remove(id)
        } else {
            false
        };
        if !is_bridge_response {
            let mut stdout = client_stdout.lock().await;
            stdout.write_all(line.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

async fn handle_relay_request(
    connection: UnixStream,
    child_stdin: Arc<Mutex<ChildStdin>>,
    client_stdout: Arc<Mutex<tokio::io::Stdout>>,
    bridge_ids: Arc<Mutex<HashSet<String>>>,
) {
    let (reader, mut writer) = connection.into_split();
    let result = async {
        let mut line = String::new();
        BufReader::new(reader).read_line(&mut line).await?;
        let request: Value = serde_json::from_str(&line)?;
        let session_id = request["sessionId"]
            .as_str()
            .context("sessionId is required")?;
        let prompt = request["prompt"].as_str().context("prompt is required")?;
        let id = format!("bridge:{}", Uuid::new_v4());
        bridge_ids.lock().await.insert(id.clone());

        let update = json!({
            "jsonrpc": "2.0", "method": "session/update",
            "params": {
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "user_message_chunk",
                    "content": { "type": "text", "text": prompt }
                }
            }
        });
        let agent_prompt = json!({
            "jsonrpc": "2.0", "id": id, "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": prompt }]
            }
        });
        write_json_line(&client_stdout, &update).await?;
        let mut stdin = child_stdin.lock().await;
        stdin.write_all(agent_prompt.to_string().as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;

    let response = match result {
        Ok(()) => json!({ "status": "accepted" }),
        Err(error) => json!({ "error": error.to_string() }),
    };
    let _ = writer.write_all(format!("{response}\n").as_bytes()).await;
}

async fn write_json_line(output: &Arc<Mutex<tokio::io::Stdout>>, value: &Value) -> Result<()> {
    let mut output = output.lock().await;
    output.write_all(value.to_string().as_bytes()).await?;
    output.write_all(b"\n").await?;
    output.flush().await?;
    Ok(())
}

async fn cleanup(relay_dir: &Path, socket_path: &Path, sessions: &Arc<Mutex<HashSet<String>>>) {
    let _ = fs::remove_file(socket_path).await;
    for session_id in sessions.lock().await.iter() {
        let path = relay_dir.join(format!("{session_id}.json"));
        let is_ours = fs::read(&path)
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|value| value["socketPath"].as_str().map(PathBuf::from))
            .is_some_and(|mapped| mapped == socket_path);
        if is_ours {
            let _ = fs::remove_file(path).await;
        }
    }
}

#[cfg(unix)]
async fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await?;
    Ok(())
}
