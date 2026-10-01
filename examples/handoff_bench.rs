// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! What a cross-thread handoff actually costs.
//!
//! The transfer pipeline hands every 64 KiB record across a channel, so 8192 of
//! them per 512 MiB per stage. If a handoff costs tens of microseconds, the
//! pipelining is spending most of what it saves; if it costs one, the idea of
//! batching records into larger messages is not worth writing.
//!
//!     cargo run --release --example handoff_bench

use std::sync::mpsc::sync_channel;
use std::time::Instant;

const MESSAGES: usize = 8192;
const CHUNK: usize = 65_518;
const DEPTH: usize = 4;

fn main() {
    trivial();
    with_payload();
    batched();
    println!(
        "\nfor scale: a 512 MiB transfer is {MESSAGES} records of {CHUNK} bytes, and the\n\
         whole transfer currently takes about 0.87s."
    );
}

/// The bare cost of waking the other thread and moving a word.
fn trivial() {
    let (tx, rx) = sync_channel::<u64>(DEPTH);
    let worker = std::thread::spawn(move || while rx.recv().is_ok() {});
    let started = Instant::now();
    for i in 0..MESSAGES {
        tx.send(i as u64).unwrap();
    }
    drop(tx);
    worker.join().unwrap();
    report("handoff, no payload", started.elapsed());
}

/// With a real payload, and the buffer recycled so allocation is not what is
/// being measured — only the handoff and the move.
fn with_payload() {
    let (tx, rx) = sync_channel::<Vec<u8>>(DEPTH);
    let (back_tx, back_rx) = sync_channel::<Vec<u8>>(DEPTH);
    let size = CHUNK;

    let worker = std::thread::spawn(move || {
        while let Ok(buffer) = rx.recv() {
            if back_tx.send(buffer).is_err() {
                break;
            }
        }
    });

    let mut buffer = vec![0u8; size];
    let started = Instant::now();
    for _ in 0..MESSAGES {
        tx.send(buffer).unwrap();
        buffer = back_rx.recv().unwrap();
    }
    drop(tx);
    worker.join().unwrap();
    report("handoff, 64 KiB record", started.elapsed());
}

/// The same bytes, but handed over in 1 MiB messages.
fn batched() {
    const PER_MESSAGE: usize = 16;
    let messages = MESSAGES / PER_MESSAGE;
    let (tx, rx) = sync_channel::<Vec<u8>>(DEPTH);
    let (back_tx, back_rx) = sync_channel::<Vec<u8>>(DEPTH);

    let worker = std::thread::spawn(move || {
        while let Ok(buffer) = rx.recv() {
            if back_tx.send(buffer).is_err() {
                break;
            }
        }
    });

    let mut buffer = vec![0u8; CHUNK * PER_MESSAGE];
    let started = Instant::now();
    for _ in 0..messages {
        tx.send(buffer).unwrap();
        buffer = back_rx.recv().unwrap();
    }
    drop(tx);
    worker.join().unwrap();
    report("handoff, 1 MiB (16 records)", started.elapsed());
}

fn report(label: &str, elapsed: std::time::Duration) {
    let per = elapsed.as_secs_f64() / MESSAGES as f64;
    println!(
        "{label:<28}: {:>8.2} ms total   {:>7.2} us per record   ({:>6.1} MiB/s equivalent)",
        elapsed.as_secs_f64() * 1000.0,
        per * 1e6,
        (MESSAGES * CHUNK) as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64(),
    );
}
