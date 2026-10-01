// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! Isolated throughput of the encrypt/frame and deframe/decrypt pipeline.
//!
//! No disk and no sockets, so what this measures is the CPU cost the transfer path
//! adds on top of the network. That is the number that decides whether framing and
//! cryptography can become the bottleneck on a fast link.
//!
//!     cargo run --release --example pipe_bench send
//!     cargo run --release --example pipe_bench recv
//!     cargo run --release --example pipe_bench both
//!
//! One phase per process by default: run in the same process, the first phase's
//! allocations are still shaping the allocator when the second one runs, and the
//! two numbers move together in a way that does not reflect either path. That is
//! not a theory — measuring both at once produced a send figure that rose 45% while
//! the untouched receive figure fell 36%, which is a measurement artefact, not a
//! trade.

use std::io::Cursor;
use std::time::{Duration, Instant};

use landrop_cli::crypto::{EphemeralKey, SessionKeys};
use landrop_cli::wire::{Connection, MAX_BLOCK_SIZE};

/// One application chunk, the unit the file sender hands to the connection.
const CHUNK: usize = MAX_BLOCK_SIZE;
const TOTAL: usize = 256 * 1024 * 1024;

fn main() {
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "both".to_string());
    if !matches!(mode.as_str(), "send" | "recv" | "both") {
        eprintln!("usage: pipe_bench [send|recv|both]");
        std::process::exit(2);
    }

    let rounds = TOTAL / CHUNK;

    if mode == "send" || mode == "both" {
        report("seal + frame", seal_throughput(rounds), rounds * CHUNK);
    }
    if mode == "recv" || mode == "both" {
        report("open + deframe", open_throughput(rounds), rounds * CHUNK);
    }
}

fn keys() -> (SessionKeys, SessionKeys) {
    let client = EphemeralKey::from_secret_bytes([7u8; 32]);
    let server = EphemeralKey::from_secret_bytes([9u8; 32]);
    (
        SessionKeys::derive(&client, &server.public(), true),
        SessionKeys::derive(&server, &client.public(), false),
    )
}

/// Encrypt and frame `rounds` chunks, discarding the output.
fn seal_throughput(rounds: usize) -> Duration {
    let payload = vec![0x5au8; CHUNK];
    let (tx, _) = keys();
    let mut conn: Connection<Cursor<Vec<u8>>, std::io::Sink> =
        Connection::new(Cursor::new(Vec::new()), std::io::sink());
    conn.enable_encryption(tx);

    let started = Instant::now();
    for _ in 0..rounds {
        conn.send_bytes(&payload).unwrap();
    }
    started.elapsed()
}

/// Deframe and decrypt a wire buffer that has already been built.
fn open_throughput(rounds: usize) -> Duration {
    let payload = vec![0x5au8; CHUNK];
    let (tx, _) = keys();

    // Setup, deliberately untimed: produce the exact bytes the receive half reads.
    //
    // Its output buffer is sized up front. Left to grow, this phase's reallocations
    // put the allocator in a state that depends on how `seal` happens to be written,
    // and the timed loop below then inherits it — which made an untouched receive
    // path appear to lose 39% when only the send path had changed.
    let mut producer: Connection<Cursor<Vec<u8>>, Vec<u8>> = Connection::new(
        Cursor::new(Vec::new()),
        Vec::with_capacity(rounds * (CHUNK + 32)),
    );
    producer.enable_encryption(tx);
    for _ in 0..rounds {
        producer.send_bytes(&payload).unwrap();
    }
    let wire = producer.into_writer();

    // Two passes, reporting the second. The first inherits whatever allocator state
    // the setup left behind, and that state depends on how `seal` is written rather
    // than on anything the receive path does — it made an untouched receive path
    // measure 39% slower purely because the send path had stopped allocating.
    let mut elapsed = Duration::ZERO;
    for pass in 0..2 {
        // Re-derived each pass so both start from the same nonce.
        let (_, rx) = keys();
        let mut conn = Connection::new(Cursor::new(wire.clone()), std::io::sink());
        conn.enable_encryption(rx);

        let started = Instant::now();
        for _ in 0..rounds {
            let chunk = conn.recv_bytes().unwrap().unwrap();
            std::hint::black_box(&chunk);
        }
        if pass == 1 {
            elapsed = started.elapsed();
        }
    }
    elapsed
}

fn report(label: &str, elapsed: Duration, bytes: usize) {
    let mib = bytes as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64();
    println!(
        "{label:<14}: {mib:>7.1} MiB/s   ({:.0} MiB in {:.3}s, {:.1} Gbps)",
        bytes as f64 / (1024.0 * 1024.0),
        elapsed.as_secs_f64(),
        8.0 * bytes as f64 / elapsed.as_secs_f64() / 1e9,
    );
}
