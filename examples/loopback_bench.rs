// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! The machine's raw loopback TCP ceiling, shaped like a transfer.
//!
//! 512 MiB pushed through loopback in `MAX_BLOCK_SIZE` writes, with no encryption
//! and no disk on either end. Anything the real transfer does on top of this is our
//! code; anything it cannot exceed is the machine.
//!
//!     cargo run --release --example loopback_bench

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Instant;

use landrop_cli::wire::MAX_BLOCK_SIZE;

const TOTAL: usize = 512 * 1024 * 1024;

fn main() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let reader = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_nodelay(true).unwrap();
        // Read with the same 1 MiB buffer the real receiver uses.
        let mut sink = vec![0u8; 1 << 20];
        let mut total = 0usize;
        while total < TOTAL {
            match stream.read(&mut sink) {
                Ok(0) | Err(_) => break,
                Ok(n) => total += n,
            }
        }
        total
    });

    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_nodelay(true).unwrap();
    let chunk = vec![0x5au8; MAX_BLOCK_SIZE];

    let started = Instant::now();
    let mut sent = 0usize;
    while sent < TOTAL {
        stream.write_all(&chunk).unwrap();
        sent += chunk.len();
    }
    stream.flush().unwrap();
    let elapsed = started.elapsed();
    let received = reader.join().unwrap();

    let mib = |bytes: usize| bytes as f64 / (1024.0 * 1024.0);
    println!(
        "loopback TCP  : {:>7.1} MiB/s   ({:.0} MiB in {:.3}s, {:.1} Gbps)",
        mib(sent) / elapsed.as_secs_f64(),
        mib(sent),
        elapsed.as_secs_f64(),
        8.0 * sent as f64 / elapsed.as_secs_f64() / 1e9,
    );
    println!("  received {received} of {sent} bytes");
    println!(
        "\nanything the real transfer adds on top of this is our code; anything it\n\
         cannot reach is this machine's loopback, not the network."
    );
}
