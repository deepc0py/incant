#![cfg(unix)]

//! End-to-end integration tests: the real `incant` binary, a real Unix
//! socket, and a mock Ollama HTTP server. No network access.
//!
//! Each test gets a fully isolated environment (HOME, XDG_CONFIG_HOME,
//! XDG_RUNTIME_DIR all point into a tempdir), its own daemon process, and
//! its own mock backend on an ephemeral port.
//!
//! Wire frames are built by hand here — deliberately not reusing the
//! crate's serializers — so these tests double as an independent check of
//! the protocol contract: 4-byte big-endian length prefix + JSON.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// A canned-response mock Ollama server.
///
/// `GET /api/tags` always succeeds (daemon health check). `POST
/// /api/generate` answers with the configured status and body.
struct MockOllama {
    port: u16,
}

impl MockOllama {
    fn start(generate_status: u16, generate_body: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let body = generate_body.clone();
                std::thread::spawn(move || {
                    let _ = serve_one(&mut stream, generate_status, &body);
                });
            }
        });
        Self { port }
    }

    fn host(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// Serve exactly one HTTP/1.1 request on `stream`, then close.
fn serve_one(
    stream: &mut std::net::TcpStream,
    generate_status: u16,
    generate_body: &str,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;

    // Read until end of headers.
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let header_end = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let request_line = head.lines().next().unwrap_or_default().to_string();

    // Drain the body per Content-Length so keep-alive clients are happy.
    let content_length: usize = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse().unwrap_or(0))
        })
        .unwrap_or(0);
    let mut body_read = buf.len() - header_end;
    while body_read < content_length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body_read += n;
    }

    let (status, body) = if request_line.starts_with("GET /api/tags") {
        (200u16, r#"{"models":[{"name":"mock-model"}]}"#.to_string())
    } else if request_line.starts_with("POST /api/generate") {
        (generate_status, generate_body.to_string())
    } else {
        (404, r#"{"error":"not found"}"#.to_string())
    };

    let reason = if status < 400 { "OK" } else { "Error" };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

/// An isolated daemon process plus the paths it lives under.
struct DaemonFixture {
    child: Child,
    #[allow(dead_code)]
    home: tempfile::TempDir,
    runtime_dir: PathBuf,
    socket_path: PathBuf,
    _mock: MockOllama,
}

impl DaemonFixture {
    /// Start a daemon wired to a mock backend that answers `generate_body`.
    fn start(generate_status: u16, generate_body: &str) -> Self {
        let mock = MockOllama::start(generate_status, generate_body.to_string());

        let home = tempfile::tempdir().expect("tempdir");
        let config_home = home.path().join("config");
        let runtime_dir = home.path().join("runtime");
        std::fs::create_dir_all(config_home.join("incant")).unwrap();
        std::fs::create_dir_all(&runtime_dir).unwrap();
        std::fs::write(
            config_home.join("incant/config.toml"),
            format!(
                "[backend]\ntype = \"ollama\"\nhost = \"{}\"\ndefault_profile = \"default\"\n\n[profiles.default]\nmodel = \"mock-model\"\ntemperature = 0.1\n",
                mock.host()
            ),
        )
        .unwrap();

        let child = Command::new(env!("CARGO_BIN_EXE_incant"))
            .args(["daemon", "run"])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn daemon");

        let socket_path = runtime_dir.join("incant.sock");
        let fixture = Self {
            child,
            home,
            runtime_dir,
            socket_path,
            _mock: mock,
        };
        fixture.wait_for_socket();
        fixture
    }

    fn wait_for_socket(&self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if UnixStream::connect(&self.socket_path).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("daemon socket never became connectable");
    }

    fn connect(&self) -> UnixStream {
        let stream = UnixStream::connect(&self.socket_path).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
    }

    /// One query round-trip; returns the raw response JSON.
    fn query(&self, query: &str, explain: bool) -> serde_json::Value {
        let mut stream = self.connect();
        write_frame(
            &mut stream,
            &serde_json::json!({
                "type": "query",
                "query": query,
                "context": {"cwd": "/tmp", "shell": "/bin/sh", "os": "TestOS 1.0"},
                "explain": explain,
            }),
        );
        read_frame(&mut stream)
    }
}

impl Drop for DaemonFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Write one length-prefixed JSON frame (protocol: 4-byte BE length + JSON).
fn write_frame(stream: &mut UnixStream, value: &serde_json::Value) {
    let payload = serde_json::to_vec(value).unwrap();
    stream
        .write_all(&(payload.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(&payload).unwrap();
    stream.flush().unwrap();
}

/// Read one length-prefixed JSON frame.
fn read_frame(stream: &mut UnixStream) -> serde_json::Value {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).unwrap();
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).unwrap();
    serde_json::from_slice(&payload).unwrap()
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

// ── happy path ─────────────────────────────────────────────────────────

#[test]
fn generates_command_end_to_end() {
    let daemon = DaemonFixture::start(200, r#"{"response":"ls -la","done":true}"#);
    let resp = daemon.query("list files", false);
    assert_eq!(resp["command"], "ls -la");
    assert_eq!(resp["risk"]["level"], "safe");
    assert!(resp.get("error").is_none());
}

#[test]
fn destructive_command_carries_risk_assessment() {
    let daemon = DaemonFixture::start(200, r#"{"response":"rm -rf /","done":true}"#);
    let resp = daemon.query("delete everything", false);
    assert_eq!(resp["command"], "rm -rf /");
    assert_eq!(resp["risk"]["level"], "destructive");
    let findings = resp["risk"]["findings"].as_array().unwrap();
    assert!(findings
        .iter()
        .any(|f| f["rule"] == "rm-recursive-force-broad"));
}

#[test]
fn explain_request_returns_explanation() {
    let daemon = DaemonFixture::start(200, r#"{"response":"df -h","done":true}"#);
    let resp = daemon.query("disk usage", true);
    assert_eq!(resp["command"], "df -h");
    // The mock returns the same canned body for the explanation pass.
    assert_eq!(resp["explanation"], "df -h");
}

#[test]
fn plain_query_omits_explanation() {
    let daemon = DaemonFixture::start(200, r#"{"response":"df -h","done":true}"#);
    let resp = daemon.query("disk usage", false);
    assert!(resp.get("explanation").is_none());
}

#[test]
fn status_message_reports_backend() {
    let daemon = DaemonFixture::start(200, r#"{"response":"x","done":true}"#);
    let mut stream = daemon.connect();
    write_frame(&mut stream, &serde_json::json!({"type": "status"}));
    let resp = read_frame(&mut stream);
    let text = resp["command"].as_str().unwrap();
    assert!(text.contains("ollama"), "unexpected status: {text}");
    assert!(text.contains("mock-model"), "unexpected status: {text}");
}

// ── error propagation ──────────────────────────────────────────────────

#[test]
fn backend_error_propagates_to_client() {
    let daemon = DaemonFixture::start(500, r#"{"error":"model exploded"}"#);
    let resp = daemon.query("anything", false);
    assert!(resp.get("command").is_none());
    let error = resp["error"].as_str().unwrap();
    assert!(error.contains("500"), "error should carry status: {error}");
}

// ── protocol robustness ────────────────────────────────────────────────

#[test]
fn oversized_frame_is_rejected_and_daemon_survives() {
    let daemon = DaemonFixture::start(200, r#"{"response":"ok","done":true}"#);

    // Claim a 2 MB payload: over the 1 MB cap. The daemon must drop the
    // connection without reading the body.
    let mut stream = daemon.connect();
    stream.write_all(&(2_000_000u32).to_be_bytes()).unwrap();
    stream.flush().unwrap();
    let mut buf = [0u8; 1];
    assert_eq!(
        stream.read(&mut buf).unwrap_or(0),
        0,
        "connection should close without a response"
    );

    // The daemon must still serve subsequent clients.
    let resp = daemon.query("still alive?", false);
    assert_eq!(resp["command"], "ok");
}

#[test]
fn malformed_json_frame_is_rejected_and_daemon_survives() {
    let daemon = DaemonFixture::start(200, r#"{"response":"ok","done":true}"#);

    let mut stream = daemon.connect();
    let garbage = b"this is not json";
    stream
        .write_all(&(garbage.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(garbage).unwrap();
    stream.flush().unwrap();
    let mut buf = [0u8; 1];
    assert_eq!(stream.read(&mut buf).unwrap_or(0), 0);

    let resp = daemon.query("still alive?", false);
    assert_eq!(resp["command"], "ok");
}

#[test]
fn concurrent_clients_all_get_answers() {
    let daemon = DaemonFixture::start(200, r#"{"response":"echo hi","done":true}"#);

    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let daemon = &daemon;
                scope.spawn(move || {
                    let resp = daemon.query(&format!("query {i}"), false);
                    assert_eq!(resp["command"], "echo hi");
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    });
}

// ── security posture ───────────────────────────────────────────────────

#[test]
#[cfg(unix)]
fn socket_is_owner_only_inside_private_runtime_dir() {
    let daemon = DaemonFixture::start(200, r#"{"response":"x","done":true}"#);
    assert_eq!(mode_of(&daemon.socket_path), 0o600, "socket must be 0600");
    assert_eq!(
        mode_of(&daemon.runtime_dir),
        0o700,
        "runtime dir must be 0700"
    );
}

// ── lifecycle ──────────────────────────────────────────────────────────

#[test]
fn shutdown_message_stops_the_daemon() {
    let mut daemon = DaemonFixture::start(200, r#"{"response":"x","done":true}"#);
    let mut stream = daemon.connect();
    write_frame(&mut stream, &serde_json::json!({"type": "shutdown"}));

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match daemon.child.try_wait().unwrap() {
            Some(status) => {
                assert!(status.success(), "daemon should exit cleanly");
                break;
            }
            None if Instant::now() > deadline => panic!("daemon did not exit after shutdown"),
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    assert!(
        !daemon.socket_path.exists(),
        "socket file should be removed on shutdown"
    );
}
