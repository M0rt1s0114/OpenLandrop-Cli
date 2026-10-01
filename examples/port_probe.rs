// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! Diagnostic: what happens when a second receiver claims UDP 52637 while the
//! desktop app already holds it?
//!
//! `receive` has to bind 52637 to hear discovery requests, and the desktop app
//! binds the same port. The app reacts to `EADDRINUSE` by falling back to a
//! random port; we currently fail outright, with `WSAEACCES` rather than
//! `EADDRINUSE`, which suggests the two are not even hitting the same wall.
//!
//! This tries each bind variant and reports the exact error, then — for whichever
//! variant works — listens for a while and reports what actually arrives. Run it
//! with the desktop app open:
//!
//! ```console
//! cargo run --release --example port_probe
//! ```
//!
//! Afterwards, run `landrop-cli discover` to confirm the app is still answering:
//! a second socket on a shared port can divert traffic away from the first.

use landrop_cli::discovery;
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

const PORT: u16 = 52_637;

fn describe(error: &std::io::Error) -> String {
    let text = error.to_string();
    // On Windows `Display` already appends "(os error N)"; do not say it twice.
    match error.raw_os_error() {
        Some(code) if !text.contains("os error") => format!("{text} (os error {code})"),
        _ => text,
    }
}

/// Bind `0.0.0.0:52637`, optionally setting `SO_REUSEADDR` first.
fn attempt(label: &str, reuse_address: bool) -> Option<UdpSocket> {
    let socket = match Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)) {
        Ok(socket) => socket,
        Err(error) => {
            println!("  {label:<22} socket() failed: {}", describe(&error));
            return None;
        }
    };
    if reuse_address && let Err(error) = socket.set_reuse_address(true) {
        println!(
            "  {label:<22} set_reuse_address failed: {}",
            describe(&error)
        );
        return None;
    }
    let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT));
    match socket.bind(&addr.into()) {
        Ok(()) => {
            println!("  {label:<22} bind OK");
            socket.set_broadcast(true).ok();
            socket.set_multicast_loop_v4(false).ok();
            Some(socket.into())
        }
        Err(error) => {
            println!("  {label:<22} bind FAILED: {}", describe(&error));
            None
        }
    }
}

/// Listen for discovery traffic and report every datagram that arrives.
fn observe(udp: &UdpSocket, seconds: u64) {
    let ifaces = discovery::interfaces(false).unwrap_or_default();
    for iface in &ifaces {
        match udp.join_multicast_v4(&discovery::MULTICAST_GROUP, &iface.address) {
            Ok(()) => println!(
                "  joined {} on {}",
                discovery::MULTICAST_GROUP,
                iface.address
            ),
            Err(error) => println!(
                "  could not join {} on {}: {}",
                discovery::MULTICAST_GROUP,
                iface.address,
                describe(&error)
            ),
        }
    }
    if let Err(error) = udp.set_read_timeout(Some(Duration::from_millis(500))) {
        println!("  set_read_timeout failed: {}", describe(&error));
        return;
    }

    println!("\n  listening {seconds}s for anything on port {PORT} ...");
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut buf = vec![0u8; 65_536];
    let mut total = 0usize;
    let mut kinds: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();

    while Instant::now() < deadline {
        match udp.recv_from(&mut buf) {
            Ok((n, from)) => {
                total += 1;
                let text = String::from_utf8_lossy(&buf[..n]).to_string();
                // The packet has a boolean `request`: false is an announcement,
                // true is somebody asking who is out there.
                let kind = if text.contains("\"request\":true") {
                    "request"
                } else if text.contains("\"request\":false") {
                    "announcement"
                } else {
                    "unparsed"
                };
                *kinds.entry(kind.to_string()).or_default() += 1;
                if total <= 6 {
                    let head: String = text.chars().take(90).collect();
                    println!("    #{total} from {from}  {n} B  [{kind}]  {head}");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(error) => {
                println!("    recv error: {}", describe(&error));
                break;
            }
        }
    }

    println!("\n  total {total} datagram(s) in {seconds}s");
    for (kind, count) in &kinds {
        println!("    {kind:<14} {count}");
    }
}

fn main() -> std::process::ExitCode {
    println!("UDP {PORT} — who can bind it?\n");
    println!("bind attempts:");
    let plain = attempt("plain", false);
    if plain.is_some() {
        println!("\n  note: the plain bind succeeded, so nothing else holds the port");
        println!("        (the desktop app is probably not running).");
    }
    let reused = attempt("SO_REUSEADDR", true);

    // Prefer the plain socket for observation: it is the one that proves the port
    // was genuinely free. Otherwise watch whatever did bind.
    match plain.or(reused) {
        Some(udp) => {
            observe(&udp, 8);
            println!("\nconclusion: a second receiver CAN share the port.");
            println!("now check the app is still answering: landrop-cli discover --json");
        }
        None => {
            println!("\nconclusion: neither variant can bind {PORT} while the app holds it.");
            println!("           SO_REUSEADDR does not buy coexistence here.");
        }
    }
    std::process::ExitCode::SUCCESS
}
