#![cfg(windows)]

//! Windows end-to-end coverage for detached lifecycle and secure named-pipe IPC.

use std::io::{Read, Seek, SeekFrom, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(15);

struct MockOllama {
    port: u16,
}

impl MockOllama {
    fn start(generate_body: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let body = generate_body.clone();
                std::thread::spawn(move || {
                    let _ = serve_one(&mut stream, &body);
                });
            }
        });
        Self { port }
    }

    fn host(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

fn serve_one(stream: &mut std::net::TcpStream, generate_body: &str) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..read]);
    }
    let header_end = buf
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let request_line = head.lines().next().unwrap_or_default().to_string();
    let content_length = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|value| value.trim().parse::<usize>().unwrap_or(0))
        })
        .unwrap_or(0);
    let mut body_read = buf.len() - header_end;
    while body_read < content_length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        body_read += read;
    }

    let (status, body) = if request_line.starts_with("GET /api/tags") {
        (200, r#"{"models":[{"name":"mock-model"}]}"#)
    } else if request_line.starts_with("POST /api/generate") {
        (200, generate_body)
    } else {
        (404, r#"{"error":"not found"}"#)
    };
    let reason = if status < 400 { "OK" } else { "Error" };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

struct IncantProcess {
    child: Child,
    stdout: std::fs::File,
    stderr: std::fs::File,
    deadline: Instant,
}

struct TestEnvironment {
    home: tempfile::TempDir,
    local_app_data: PathBuf,
    runtime_dir: PathBuf,
    _mock: MockOllama,
}

impl TestEnvironment {
    fn new() -> Self {
        let mock = MockOllama::start(r#"{"response":"echo windows","done":true}"#.to_string());
        let home = tempfile::tempdir().expect("tempdir");
        let local_app_data = home.path().join("LocalAppData");
        let config_dir = local_app_data.join("incant");
        let runtime_dir = config_dir.join("run");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            format!(
                "[backend]\ntype = \"ollama\"\nhost = \"{}\"\ndefault_profile = \"default\"\n\n[profiles.default]\nmodel = \"mock-model\"\ntemperature = 0.1\n",
                mock.host()
            ),
        )
        .unwrap();
        Self {
            home,
            local_app_data,
            runtime_dir,
            _mock: mock,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_incant"));
        command
            .env_clear()
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("LOCALAPPDATA", &self.local_app_data);
        for name in ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
    }

    fn spawn(&self, args: &[&str]) -> IncantProcess {
        let stdout = tempfile::tempfile().expect("create incant stdout capture");
        let stderr = tempfile::tempfile().expect("create incant stderr capture");
        let mut command = self.command();
        command
            .args(args)
            .stdout(Stdio::from(
                stdout.try_clone().expect("clone stdout capture"),
            ))
            .stderr(Stdio::from(
                stderr.try_clone().expect("clone stderr capture"),
            ));
        let child = command
            .spawn()
            .unwrap_or_else(|error| panic!("spawn incant {args:?}: {error}"));
        IncantProcess {
            child,
            stdout,
            stderr,
            deadline: Instant::now() + PROCESS_TIMEOUT,
        }
    }

    fn wait_for_output(&self, process: IncantProcess, args: &[&str]) -> Output {
        wait_for_incant(process, args)
    }

    fn run(&self, args: &[&str]) -> Output {
        let process = self.spawn(args);
        self.wait_for_output(process, args)
    }

    fn assert_success(&self, args: &[&str]) -> Output {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "incant {args:?} failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn wait_until_stopped(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let output = self.run(&["daemon", "status"]);
            if output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("Daemon: not running")
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("daemon did not stop");
    }

    fn pid_path(&self) -> PathBuf {
        self.runtime_dir.join("incant.pid")
    }

    fn startup_path(&self) -> PathBuf {
        self.runtime_dir.join("incant.startup")
    }
}

impl Drop for TestEnvironment {
    fn drop(&mut self) {
        let _ = self.run(&["daemon", "stop"]);
    }
}

fn wait_for_incant(mut process: IncantProcess, args: &[&str]) -> Output {
    let timed_out = loop {
        match process.child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) if Instant::now() < process.deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = process.child.kill();
                break true;
            }
            Err(error) => panic!("poll incant {args:?}: {error}"),
        }
    };
    let status = process
        .child
        .wait()
        .unwrap_or_else(|error| panic!("reap incant {args:?}: {error}"));
    let mut stdout = Vec::new();
    process.stdout.seek(SeekFrom::Start(0)).unwrap();
    process.stdout.read_to_end(&mut stdout).unwrap();
    let mut stderr = Vec::new();
    process.stderr.seek(SeekFrom::Start(0)).unwrap();
    process.stderr.read_to_end(&mut stderr).unwrap();
    let output = Output {
        status,
        stdout,
        stderr,
    };
    assert!(
        !timed_out,
        "incant {args:?} timed out after {PROCESS_TIMEOUT:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

fn assert_hung_pipe_probe_is_bounded(
    environment: &TestEnvironment,
    pipe_name: &str,
    args: &[&str],
) {
    eprintln!("stage: hung lifecycle probe {args:?}");
    use tokio::net::windows::named_pipe::ServerOptions;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .expect("build Tokio runtime");
    let _runtime_guard = runtime.enter();
    let server = ServerOptions::new()
        .reject_remote_clients(true)
        .first_pipe_instance(true)
        .create(pipe_name)
        .expect("create hung named-pipe server");

    let started = Instant::now();
    let output = environment.run(args);
    let elapsed = started.elapsed();
    drop(server);

    assert!(
        !output.status.success(),
        "hung probe unexpectedly succeeded"
    );
    assert!(
        text(&output.stderr).contains("daemon status probe timed out"),
        "unexpected probe error: {}",
        text(&output.stderr)
    );
    assert!(
        elapsed >= Duration::from_millis(1500) && elapsed < Duration::from_secs(4),
        "lifecycle probe returned outside strict bound: {elapsed:?}"
    );
    if args == ["daemon", "start"] {
        assert!(
            !text(&output.stderr).contains("Starting daemon (PID"),
            "start spawned a duplicate daemon after a timed-out probe"
        );
    }
}

#[test]
fn windows_named_pipe_roundtrip_and_detached_lifecycle() {
    let environment = TestEnvironment::new();
    eprintln!("stage: detached start");

    let start = environment.assert_success(&["daemon", "start"]);
    assert!(text(&start.stderr).contains("Daemon is ready"));

    eprintln!("stage: status and round trip");
    let status = environment.assert_success(&["daemon", "status"]);
    let status = text(&status.stdout);
    assert!(status.contains("Daemon: running"));
    let pipe_name = status
        .lines()
        .find_map(|line| line.strip_prefix("Endpoint: "))
        .expect("status includes daemon endpoint")
        .to_string();
    assert!(pipe_name.starts_with(r"\\.\pipe\incant-S-1-"));
    let query = environment.assert_success(&["--pipe", "list files"]);
    assert_eq!(text(&query.stdout), "echo windows");

    eprintln!("stage: concurrent clients");
    let mut clients = Vec::new();
    for _ in 0..4 {
        let args = ["--pipe", "concurrent request"];
        clients.push((environment.spawn(&args), args));
    }
    for (client, args) in clients {
        let output = environment.wait_for_output(client, &args);
        assert!(output.status.success(), "concurrent client failed");
        assert_eq!(text(&output.stdout), "echo windows");
    }

    eprintln!("stage: detached stop");
    environment.assert_success(&["daemon", "stop"]);
    environment.wait_until_stopped();

    eprintln!("stage: bounded hung lifecycle probes");
    // A live, correctly owned pipe that never answers Status must fail every
    // lifecycle command on one total deadline. Start must not treat the timeout
    // as absence and launch a duplicate daemon.
    for args in [
        &["daemon", "start"][..],
        &["daemon", "status"][..],
        &["daemon", "stop"][..],
    ] {
        assert_hung_pipe_probe_is_bounded(&environment, &pipe_name, args);
    }

    eprintln!("stage: stale state");
    // Stale filesystem state must not imply liveness on Windows; named pipes
    // themselves leave no filesystem entry to clean up.
    std::fs::write(environment.pid_path(), "4294967295").unwrap();
    std::fs::write(environment.startup_path(), "OK").unwrap();
    let stale_status = environment.assert_success(&["daemon", "status"]);
    assert!(text(&stale_status.stdout).contains("Daemon: not running"));
    let unavailable = environment.run(&["--pipe", "must fail closed"]);
    assert!(!unavailable.status.success());

    eprintln!("stage: restart after stale state");
    // A new detached start removes stale startup state and remains stoppable.
    environment.assert_success(&["daemon", "start"]);
    environment.assert_success(&["daemon", "stop"]);
    environment.wait_until_stopped();
    assert!(!environment.pid_path().exists());
    eprintln!("stage: complete");
}
