// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! CLI-level end-to-end tests that exercise the real binary.
//!
//! These run against an isolated config directory so they never touch the user's
//! real identity or settings.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_landrop-cli");

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("temp dir"),
        }
    }

    fn config_dir(&self) -> PathBuf {
        self.dir.path().join("config")
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(BIN)
            .args(args)
            .env("LANDROP_CLI_CONFIG_DIR", self.config_dir())
            .output()
            .expect("failed to launch landrop-cli")
    }
}

#[test]
fn selftest_transfers_a_payload_intact() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["selftest", "--size", "2000000"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "selftest failed.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("selftest OK"),
        "unexpected output: {stdout}"
    );
}

#[test]
fn identity_is_created_once_and_reused() {
    let sandbox = Sandbox::new();

    let first = sandbox.run(&["identity", "--json"]);
    assert!(first.status.success());
    let first_json: serde_json::Value =
        serde_json::from_slice(&first.stdout).expect("valid JSON on stdout");
    let first_key = first_json["public_key"].as_str().unwrap().to_string();
    assert_eq!(
        first_key.len(),
        44,
        "compressed secp256k1 key is 44 base64 chars"
    );

    // A second invocation must report the same identity, or every peer would treat
    // the CLI as a brand-new device on each run.
    let second = sandbox.run(&["identity", "--json"]);
    let second_json: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second_json["public_key"].as_str().unwrap(), first_key);

    // The secret must not leak unless explicitly requested.
    assert!(first_json["secret_key"].is_null());
    let revealed = sandbox.run(&["identity", "--json", "--show-secret"]);
    let revealed_json: serde_json::Value = serde_json::from_slice(&revealed.stdout).unwrap();
    assert!(revealed_json["secret_key"].is_string());
}

#[test]
fn identity_file_is_restricted_on_unix() {
    let sandbox = Sandbox::new();
    assert!(sandbox.run(&["identity"]).status.success());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = sandbox.config_dir().join("identity.json");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the private key must not be world readable");
    }
}

#[test]
fn error_paths_exit_nonzero_and_emit_json() {
    let sandbox = Sandbox::new();
    let payload = sandbox.dir.path().join("payload.txt");
    std::fs::write(&payload, b"hello").unwrap();

    // Port 1 on loopback is not listening.
    let output = sandbox.run(&[
        "send",
        payload.to_str().unwrap(),
        "--to",
        "127.0.0.1:1",
        "--json",
    ]);
    assert!(!output.status.success(), "an unreachable target must fail");

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must stay valid JSON");
    assert_eq!(json["status"], "error");
    assert!(json["error"].as_str().unwrap().contains("127.0.0.1:1"));
}

#[test]
fn send_requires_something_to_send() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["send", "--to", "127.0.0.1:1", "--json", "--stdin"]);
    // With --stdin and no piped data the input list is empty.
    assert!(!output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(json["error"].as_str().unwrap().contains("no files to send"));
}

#[test]
fn missing_path_reports_a_clear_error() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&[
        "send",
        "/definitely/not/here/xyz",
        "--to",
        "127.0.0.1:1",
        "--json",
    ]);
    assert!(!output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(json["error"].as_str().unwrap().contains("cannot stat"));
}

#[test]
fn devices_cache_round_trips_through_the_cli() {
    let sandbox = Sandbox::new();
    let empty = sandbox.run(&["devices", "--json"]);
    assert!(empty.status.success());
    let json: serde_json::Value = serde_json::from_slice(&empty.stdout).unwrap();
    assert_eq!(json["devices"].as_array().unwrap().len(), 0);

    // Forgetting an unknown device must fail loudly rather than silently succeed.
    let missing = sandbox.run(&["devices", "--forget", "nobody", "--json"]);
    assert!(!missing.status.success());
}

#[test]
fn help_lists_every_subcommand() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["--help"]);
    let text = String::from_utf8_lossy(&output.stdout);
    for command in [
        "discover", "send", "receive", "text", "devices", "identity", "selftest",
    ] {
        assert!(text.contains(command), "--help is missing {command}");
    }
}

/// Remember a device at `address:port`, the way a completed discovery would.
fn remember_device_at(config_dir: &std::path::Path, public_key: &str, port: u16) {
    std::fs::create_dir_all(config_dir).expect("config dir");
    let settings = serde_json::json!({
        "known_devices": [{
            "name": "build-server",
            "type": "linux",
            "public_key": public_key,
            "last_address": "127.0.0.1",
            "last_port": port,
            "last_seen": 1_700_000_000,
        }]
    });
    std::fs::write(
        config_dir.join("settings.json"),
        serde_json::to_vec_pretty(&settings).unwrap(),
    )
    .expect("writing settings.json");
}

fn payload(sandbox: &Sandbox) -> PathBuf {
    let file = sandbox.dir.path().join("payload.txt");
    std::fs::write(&file, b"hello").unwrap();
    file
}

/// A remembered address is a hint, not a fact: the peer picks a fresh listening
/// port on every run, so a cached one goes stale the moment the peer restarts.
/// Targeting a device by public key has to re-resolve before giving up, or the
/// stable identifier is worth no more than the address it resolves to.
#[test]
fn stale_remembered_address_is_re_resolved() {
    let sandbox = Sandbox::new();
    let key = "AkxBTkRST1AtRVhBTVBMRS1LRVktT05FLi4uLi4uLi4u";
    // Port 1 is reserved and nothing listens there, so this stands in for a port
    // the peer has since abandoned.
    remember_device_at(&sandbox.config_dir(), key, 1);
    let file = payload(&sandbox);

    let output = sandbox.run(&["send", file.to_str().unwrap(), "--to", key, "--json"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        !output.status.success(),
        "no peer exists to accept, so this must fail; stdout: {stdout}"
    );
    assert!(
        stderr.contains("asking the network again"),
        "the stale address should have been re-resolved; stderr: {stderr}"
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json error body");
    assert_eq!(json["status"], "error");
    let error = json["error"].as_str().unwrap();
    assert!(
        error.contains("did not find the device again"),
        "the re-discovery failure must be reported, not just the first error; got: {error}"
    );
    // `{:#}` renders the whole anyhow chain, so the original connect failure has
    // to survive underneath the re-discovery one.
    assert!(
        error.contains("could not connect"),
        "the original failure must stay in the chain; got: {error}"
    );
}

/// An explicit `host:port` is the caller pinning an address. Re-resolving it
/// would mean quietly sending somewhere the caller did not name, so it has to
/// fail fast instead.
#[test]
fn explicit_address_is_never_re_resolved() {
    let sandbox = Sandbox::new();
    let file = payload(&sandbox);

    let output = sandbox.run(&[
        "send",
        file.to_str().unwrap(),
        "--to",
        "127.0.0.1:1",
        "--json",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(!output.status.success(), "stdout: {stdout}");
    assert!(
        !stderr.contains("asking the network again"),
        "an explicit address must never be re-resolved; stderr: {stderr}"
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json error body");
    assert!(
        json["error"]
            .as_str()
            .unwrap()
            .contains("could not connect"),
        "got: {}",
        json["error"]
    );
}

/// Two devices sending at the same time must both land — and must not overwrite
/// each other even though both files carry the same name.
#[test]
fn two_senders_at_once_both_arrive() {
    let sandbox = Sandbox::new();
    let inbox = sandbox.dir.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();

    // Take a free port, then release it for the receiver to claim.
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();

    let mut receiver = Command::new(BIN)
        .args([
            "receive",
            "--yes",
            "--max",
            "2",
            "--quiet",
            "--dir",
            inbox.to_str().unwrap(),
            "--port",
            &port.to_string(),
        ])
        .env(
            "LANDROP_CLI_CONFIG_DIR",
            sandbox.config_dir().join("receiver"),
        )
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to launch the receiver");

    // Wait until it listens. Connecting and hanging up is not a transfer, so this
    // must not consume one of the two `--max` slots.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            std::time::Instant::now() < deadline,
            "the receiver never started listening"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Same filename from both sides, big enough that the two transfers overlap.
    let mut senders = Vec::new();
    for who in ["a", "b"] {
        let dir = sandbox.dir.path().join(who);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("same.txt");
        std::fs::write(&src, vec![b'x'; 1 << 20]).unwrap();
        senders.push(
            Command::new(BIN)
                .args([
                    "send",
                    src.to_str().unwrap(),
                    "--to",
                    &format!("127.0.0.1:{port}"),
                    "--json",
                    "--reply-timeout",
                    "30",
                ])
                .env("LANDROP_CLI_CONFIG_DIR", sandbox.config_dir().join(who))
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("failed to launch a sender"),
        );
    }

    for sender in &mut senders {
        let status = sender.wait().expect("waiting for a sender");
        assert!(status.success(), "a concurrent send failed: {status}");
    }

    // `--max 2` means it stops by itself once both are served.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        match receiver.try_wait().expect("try_wait on the receiver") {
            Some(status) => {
                assert!(status.success(), "the receiver exited with {status}");
                break;
            }
            None if std::time::Instant::now() > deadline => {
                let _ = receiver.kill();
                panic!("the receiver did not stop after --max 2");
            }
            None => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }

    // Checked only now: a sender is finished once the bytes are on the wire, which
    // is before the receiver has necessarily written them to disk. The receiver
    // exiting is the point at which both transfers are fully served.
    let received: Vec<std::fs::DirEntry> = std::fs::read_dir(&inbox)
        .unwrap()
        .map(|entry| entry.unwrap())
        .collect();
    assert_eq!(
        received.len(),
        2,
        "both transfers must land as separate files, got {:?}",
        received
            .iter()
            .map(std::fs::DirEntry::file_name)
            .collect::<Vec<_>>()
    );
    for entry in &received {
        assert_eq!(
            entry.metadata().unwrap().len(),
            1 << 20,
            "{:?} is not complete — the two writes may have collided",
            entry.file_name()
        );
    }
}

/// `--once` means "exit after one transfer", not "exit immediately".
///
/// The limit is checked before every accept, so it has to be a count of what has
/// been served. Written as a flag it is true on the very first pass, and the
/// receiver exits without ever listening — which is what happened, and what the
/// `--max` test did not catch.
#[test]
fn once_serves_one_transfer_then_stops() {
    let sandbox = Sandbox::new();
    let inbox = sandbox.dir.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();

    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();

    let mut receiver = Command::new(BIN)
        .args([
            "receive",
            "--yes",
            "--once",
            "--quiet",
            "--dir",
            inbox.to_str().unwrap(),
            "--port",
            &port.to_string(),
        ])
        .env(
            "LANDROP_CLI_CONFIG_DIR",
            sandbox.config_dir().join("receiver"),
        )
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to launch the receiver");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            std::time::Instant::now() < deadline,
            "the receiver exited before listening: --once was treated as a stop, not a limit"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let src = sandbox.dir.path().join("payload.txt");
    std::fs::write(&src, b"hello world").unwrap();
    let status = Command::new(BIN)
        .args([
            "send",
            src.to_str().unwrap(),
            "--to",
            &format!("127.0.0.1:{port}"),
            "--json",
            "--wait-for-close",
            "--reply-timeout",
            "30",
        ])
        .env(
            "LANDROP_CLI_CONFIG_DIR",
            sandbox.config_dir().join("sender"),
        )
        .output()
        .expect("failed to launch the sender");
    assert!(
        status.status.success(),
        "send failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    // `--wait-for-close` is the only completion signal the protocol offers, so it
    // is also what proves the receiver got as far as hanging up.
    let summary: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(summary["peer_closed"], serde_json::Value::Bool(true));
    assert_eq!(summary["sent_bytes"], serde_json::json!(11));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        match receiver.try_wait().expect("try_wait on the receiver") {
            Some(status) => {
                assert!(status.success(), "the receiver exited with {status}");
                break;
            }
            None if std::time::Instant::now() > deadline => {
                let _ = receiver.kill();
                panic!("--once did not stop the receiver after one transfer");
            }
            None => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }

    let received: Vec<_> = std::fs::read_dir(&inbox)
        .unwrap()
        .map(|entry| entry.unwrap())
        .collect();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].metadata().unwrap().len(), 11);
}
