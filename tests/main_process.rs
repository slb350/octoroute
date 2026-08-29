//! Process-level contracts for executable startup and shutdown.

#[cfg(unix)]
use octoroute::cli::generate_config_template;
#[cfg(unix)]
use std::{
    fs,
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::Path,
};
use std::{
    io,
    process::{Child, Command, ExitStatus, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

const POLL_INTERVAL: Duration = Duration::from_millis(25);
#[cfg(unix)]
const CONNECT_TIMEOUT: Duration = Duration::from_millis(100);
#[cfg(unix)]
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(unix)]
const LIVENESS_OBSERVATION: Duration = Duration::from_millis(500);

struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        command.spawn().map(|child| Self(Some(child)))
    }

    fn child_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("child is present")
    }

    #[cfg(unix)]
    fn child(&self) -> &Child {
        self.0.as_ref().expect("child is present")
    }

    #[cfg(unix)]
    fn kill_if_running(&mut self) -> io::Result<()> {
        let child = self.child_mut();
        if child.try_wait()?.is_none() {
            child.kill()?;
        }
        Ok(())
    }

    fn wait_with_output(mut self) -> io::Result<Output> {
        self.0.take().expect("child is present").wait_with_output()
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let Some(child) = self.0.as_mut() else {
            return;
        };
        if child.try_wait().ok().flatten().is_none() {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}

#[test]
fn missing_config_exits_nonzero_with_prefixed_error() {
    let directory = TempDir::new().expect("temporary directory");
    let config_path = directory.path().join("missing.toml");
    assert!(!config_path.exists());

    let mut command = Command::new(env!("CARGO_BIN_EXE_octoroute"));
    command
        .arg("--config")
        .arg(&config_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut process = ChildGuard::spawn(&mut command).expect("start Octoroute");

    let status = wait_for_exit(process.child_mut(), EXIT_TIMEOUT)
        .expect("poll Octoroute process")
        .expect("Octoroute must exit before the deadline");
    let output = process.wait_with_output().expect("collect process output");
    assert_eq!(output.status, status);

    let stderr = String::from_utf8(output.stderr).expect("UTF-8 stderr");
    assert!(!status.success(), "unexpected success; stderr: {stderr}");
    assert!(
        stderr.starts_with("octoroute: "),
        "stderr lacks the CLI error prefix: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn server_stays_running_until_sigterm_then_exits_cleanly() {
    let directory = TempDir::new().expect("temporary directory");
    let server_port = ephemeral_port();
    // A destination port of zero cannot name a listening upstream service.
    let unavailable_upstream_port = 0;
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, server_port));
    let unavailable_address = SocketAddr::from((Ipv4Addr::LOCALHOST, unavailable_upstream_port));
    assert!(
        TcpStream::connect_timeout(&unavailable_address, CONNECT_TIMEOUT).is_err(),
        "upstream fixture port unexpectedly accepts connections"
    );

    let missing_codex = directory.path().join("missing-codex");
    assert!(!missing_codex.exists());
    let config = isolated_config(server_port, unavailable_upstream_port, &missing_codex);
    let config_path = directory.path().join("octoroute.toml");
    fs::write(&config_path, config).expect("write process-test configuration");

    let mut command = Command::new(env!("CARGO_BIN_EXE_octoroute"));
    command
        .arg("--config")
        .arg(&config_path)
        .env("OCTOROUTE_API_KEY", "process-test-inbound-bearer")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut process = ChildGuard::spawn(&mut command).expect("start Octoroute");

    match wait_until_serving(process.child_mut(), address, STARTUP_TIMEOUT) {
        Ok(()) => {}
        Err(reason) => {
            process
                .kill_if_running()
                .expect("stop Octoroute after failed startup");
            let output = process.wait_with_output().expect("collect failed startup");
            panic!(
                "Octoroute did not start ({reason}); stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    assert!(
        TcpStream::connect_timeout(&unavailable_address, CONNECT_TIMEOUT).is_err(),
        "startup unexpectedly made an upstream fixture reachable"
    );
    assert_remains_running(process.child_mut(), LIVENESS_OBSERVATION)
        .expect("poll running Octoroute process");

    send_sigterm(process.child()).expect("send SIGTERM");
    let status = wait_for_exit(process.child_mut(), EXIT_TIMEOUT)
        .expect("poll shutdown")
        .expect("Octoroute must exit after SIGTERM before the deadline");
    let output = process.wait_with_output().expect("collect shutdown output");

    assert_eq!(output.status, status);
    assert!(
        status.success(),
        "SIGTERM shutdown failed with {status}; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        sleep_until_next_poll(deadline);
    }
}

#[cfg(unix)]
fn wait_until_serving(
    child: &mut Child,
    address: SocketAddr,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("could not poll process: {error}"))?
        {
            return Err(format!("process exited with {status}"));
        }
        if live_endpoint_responds(address) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("startup deadline of {timeout:?} elapsed"));
        }
        sleep_until_next_poll(deadline);
    }
}

#[cfg(unix)]
fn assert_remains_running(child: &mut Child, observation: Duration) -> io::Result<()> {
    let deadline = Instant::now() + observation;
    loop {
        if let Some(status) = child.try_wait()? {
            panic!("Octoroute exited during the liveness observation window with {status}");
        }
        if Instant::now() >= deadline {
            return Ok(());
        }
        sleep_until_next_poll(deadline);
    }
}

#[cfg(unix)]
fn live_endpoint_responds(address: SocketAddr) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) else {
        return false;
    };
    if stream.set_read_timeout(Some(CONNECT_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(CONNECT_TIMEOUT)).is_err()
    {
        return false;
    }
    if stream
        .write_all(b"GET /health/live HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }

    let mut response = [0_u8; 64];
    matches!(stream.read(&mut response), Ok(read) if response[..read].starts_with(b"HTTP/1.1 200"))
}

fn sleep_until_next_poll(deadline: Instant) {
    let remaining = deadline.saturating_duration_since(Instant::now());
    thread::sleep(POLL_INTERVAL.min(remaining));
}

#[cfg(unix)]
fn ephemeral_port() -> u16 {
    let server = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve server port");
    server.local_addr().expect("server address").port()
}

#[cfg(unix)]
fn isolated_config(server_port: u16, upstream_port: u16, missing_codex: &Path) -> String {
    let mut config = generate_config_template().to_owned();
    config = replace_required(
        config,
        "host = \"0.0.0.0\"\nport = 8081",
        &format!("host = \"127.0.0.1\"\nport = {server_port}"),
    );
    for member in [
        "192.168.1.20",
        "192.168.1.21",
        "192.168.1.22",
        "192.168.1.30",
    ] {
        config = replace_required(
            config,
            &format!("http://{member}:8000"),
            &format!("http://127.0.0.1:{upstream_port}"),
        );
    }
    for endpoint in [
        "https://api.kimi.com/coding/v1",
        "https://api.z.ai/api/coding/paas/v4",
        "https://openrouter.ai/api/v1",
        "https://api.openai.com/v1",
    ] {
        config = replace_required(
            config,
            endpoint,
            &format!("https://127.0.0.1:{upstream_port}"),
        );
    }
    replace_required(
        config,
        "executable = \"codex\"",
        &format!("executable = {:?}", missing_codex.display().to_string()),
    )
}

#[cfg(unix)]
fn replace_required(input: String, anchor: &str, replacement: &str) -> String {
    assert!(input.contains(anchor), "missing config anchor: {anchor}");
    input.replacen(anchor, replacement, 1)
}

#[cfg(unix)]
fn send_sigterm(child: &Child) -> io::Result<()> {
    let status = Command::new("/bin/kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("/bin/kill exited with {status}")))
    }
}
