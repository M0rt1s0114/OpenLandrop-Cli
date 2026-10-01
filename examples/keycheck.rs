// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! Cross-implementation key vector: derives session keys from two fixed X25519
//! secrets so the output can be diffed against libsodium's `crypto_kx`.
//!
//! Run with: cargo run --release --example keycheck <`client_sk_hex`> <`server_sk_hex`>

use landrop_cli::crypto::{EphemeralKey, SessionKeys};

fn parse_hex(text: &str) -> [u8; 32] {
    let bytes: Vec<u8> = (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("valid hex"))
        .collect();
    bytes.as_slice().try_into().expect("32 bytes")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let client_sk = parse_hex(&args[1]);
    let server_sk = parse_hex(&args[2]);

    let client = EphemeralKey::from_secret_bytes(client_sk);
    let server = EphemeralKey::from_secret_bytes(server_sk);

    let ck = SessionKeys::derive(&client, &server.public(), true);
    let sk = SessionKeys::derive(&server, &client.public(), false);

    println!("client_pk       = {}", hex(&client.public()));
    println!("server_pk       = {}", hex(&server.public()));
    println!("client.rx       = {}", hex(&ck.rx));
    println!("client.tx       = {}", hex(&ck.tx));
    println!("server.rx       = {}", hex(&sk.rx));
    println!("server.tx       = {}", hex(&sk.tx));
    println!("client.verif    = {}", ck.verif_code());
    println!("server.verif    = {}", sk.verif_code());
    println!(
        "keys_agree      = {}",
        ck.tx == sk.rx && ck.rx == sk.tx && ck.verif_code() == sk.verif_code()
    );
}
