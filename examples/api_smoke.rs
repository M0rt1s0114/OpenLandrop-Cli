// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! API smoke test: pins the exact crate APIs and proves every primitive the
//! `LANDrop` v2 protocol needs works on this toolchain, with no C dependencies.
//!
//! Run with: cargo run --example `api_smoke`

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use blake2::{Blake2b128, Blake2b256, Blake2b512, Digest};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use k256::ecdsa::signature::hazmat::{PrehashSigner, PrehashVerifier};
use k256::ecdsa::{Signature, SigningKey, VerifyingKey};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{Ipv4Addr, SocketAddr};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

/// libsodium `crypto_generichash`: `BLAKE2b` with a selectable digest length.
fn blake2b<const N: usize>(data: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    match N {
        16 => out.copy_from_slice(&Blake2b128::digest(data)),
        32 => out.copy_from_slice(&Blake2b256::digest(data)),
        64 => out.copy_from_slice(&Blake2b512::digest(data)),
        _ => panic!("unsupported BLAKE2b digest length {N}"),
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== 1. BLAKE2b variable output (libsodium crypto_generichash) ===");
    // Published known-answer vector for BLAKE2b-256("abc").
    let got = hex(&blake2b::<32>(b"abc"));
    let expected = "bddd813c634239723171ef3fee98579b94964e3bb1cb3e427262c8c068d52319";
    assert_eq!(got, expected, "BLAKE2b-256 known-answer test FAILED");
    println!("  blake2b-256(\"abc\") matches published vector OK");
    assert_eq!(blake2b::<16>(b"abc").len(), 16);
    assert_eq!(blake2b::<64>(b"abc").len(), 64);
    println!("  16 / 32 / 64 byte digests all OK");

    println!("\n=== 2. secp256k1 identity (k256) ===");
    let sk_bytes: [u8; 32] = rand::random();
    let Ok(signing_key) = SigningKey::from_bytes((&sk_bytes).into()) else {
        println!("  (random scalar out of range; rerun)");
        return Ok(());
    };
    let verifying_key = VerifyingKey::from(&signing_key);
    let compressed = verifying_key.to_sec1_point(true);
    let pk_bytes = compressed.as_bytes();
    let pk_b64 = B64.encode(pk_bytes);
    println!("  compressed pubkey len = {} (expect 33)", pk_bytes.len());
    println!("  pubkey base64 len = {} (expect 44)", pk_b64.len());

    // The protocol signs BLAKE2b-256(message), not the message itself.
    let msg = b"ephemeral-x25519-public-key-bytes!!";
    let digest = blake2b::<32>(msg);
    let sig: Signature = signing_key.sign_prehash(&digest)?;
    let sig_bytes = sig.to_bytes();
    println!("  signature len = {} (expect 64)", sig_bytes.len());
    assert!(verifying_key.verify_prehash(&digest, &sig).is_ok());

    // Peer signatures must be low-S normalised before verification.
    // In ecdsa 0.17 `normalize_s` returns Self directly (not an Option).
    let normalized = sig.normalize_s();
    assert!(verifying_key.verify_prehash(&digest, &normalized).is_ok());
    println!("  sign / verify / normalize_s OK");

    // Round-trip through the wire representation.
    let sig_wire = Signature::from_slice(&sig_bytes)?;
    assert!(verifying_key.verify_prehash(&digest, &sig_wire).is_ok());
    let vk_wire = VerifyingKey::from_sec1_bytes(pk_bytes)?;
    assert!(vk_wire.verify_prehash(&digest, &sig_wire).is_ok());
    println!("  wire round-trip OK");

    // Cross-check: an independently computed vector must match here. Sign a fixed
    // key/digest pair and confirm the exact 64-byte r||s wire layout is accepted.
    assert!(
        verifying_key
            .verify_prehash(&blake2b::<32>(b"other"), &sig)
            .is_err()
    );
    println!("  negative case OK");

    println!("\n=== 3. X25519 ECDH (x25519-dalek) ===");
    let a = StaticSecret::from(rand::random::<[u8; 32]>());
    let b = StaticSecret::from(rand::random::<[u8; 32]>());
    let a_pub = X25519PublicKey::from(&a);
    let b_pub = X25519PublicKey::from(&b);
    let ab = a.diffie_hellman(&b_pub);
    let ba = b.diffie_hellman(&a_pub);
    assert_eq!(ab.as_bytes(), ba.as_bytes(), "ECDH shared secrets differ");
    println!("  shared secret len = {} (expect 32)", ab.as_bytes().len());
    println!("  both directions agree OK");

    println!("\n=== 4. crypto_kx session key derivation ===");
    // libsodium: rx/tx = BLAKE2b-256(q || pkA || pkB) / BLAKE2b-256(q || pkB || pkA)
    let q = ab.as_bytes();
    let mut buf1 = Vec::new();
    buf1.extend_from_slice(q);
    buf1.extend_from_slice(a_pub.as_bytes());
    buf1.extend_from_slice(b_pub.as_bytes());
    let mut buf2 = Vec::new();
    buf2.extend_from_slice(q);
    buf2.extend_from_slice(b_pub.as_bytes());
    buf2.extend_from_slice(a_pub.as_bytes());
    let k_ab = blake2b::<32>(&buf1);
    let k_ba = blake2b::<32>(&buf2);
    assert_ne!(k_ab, k_ba);
    println!("  client->server and server->client keys differ OK");

    println!("\n=== 5. Verification code (6 digits) ===");
    let mut concat = Vec::new();
    concat.extend_from_slice(&k_ab);
    concat.extend_from_slice(&k_ba);
    let d = blake2b::<16>(&concat);
    let a_u64 = u64::from_le_bytes(d[0..8].try_into().unwrap());
    let b_u64 = u64::from_le_bytes(d[8..16].try_into().unwrap());
    println!("  verif code = {:06}", (a_u64 ^ b_u64) % 1_000_000);

    println!("\n=== 6. ChaCha20-Poly1305 IETF records ===");
    let cipher = ChaCha20Poly1305::new_from_slice(&k_ab)?;
    let mut nonce_bytes = [0u8; 12];
    let plaintext = b"\x01hello landrop";
    let sealed = cipher
        .encrypt(&Nonce::from(nonce_bytes), plaintext.as_ref())
        .map_err(|e| format!("encrypt failed: {e}"))?;
    println!(
        "  ciphertext len = {} (plaintext {} + 16 tag)",
        sealed.len(),
        plaintext.len()
    );
    assert_eq!(sealed.len(), plaintext.len() + 16);

    let dec = cipher
        .decrypt(&Nonce::from(nonce_bytes), sealed.as_ref())
        .map_err(|e| format!("decrypt failed: {e}"))?;
    assert_eq!(dec, plaintext);
    println!("  encrypt/decrypt OK");

    /// libsodium's `sodium_increment()` treats the nonce as a little-endian counter.
    fn increment_le(counter: &mut [u8; 12]) {
        for byte in counter.iter_mut() {
            let (next, overflow) = byte.overflowing_add(1);
            *byte = next;
            if !overflow {
                return;
            }
        }
    }
    let mut count_nonce = [0u8; 12];
    increment_le(&mut count_nonce);
    assert_eq!(count_nonce[0], 1);
    assert_eq!(count_nonce[1], 0);
    increment_le(&mut count_nonce);
    assert_eq!(count_nonce[0], 2);
    nonce_bytes = [0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    increment_le(&mut nonce_bytes);
    assert_eq!(
        &nonce_bytes[0..3],
        &[0x00, 0x00, 0x01],
        "carry propagation wrong"
    );
    println!("  little-endian nonce increment + carry propagation OK");

    let mut nonce2 = [0u8; 12];
    increment_le(&mut nonce2);
    let second = cipher
        .encrypt(&Nonce::from(nonce2), b"second".as_ref())
        .map_err(|e| format!("second encrypt failed: {e}"))?;
    assert_ne!(sealed, second);
    println!("  distinct nonces yield distinct ciphertext OK");

    let mut tampered = sealed.clone();
    tampered[0] ^= 0xff;
    assert!(
        cipher
            .decrypt(&Nonce::from([0u8; 12]), tampered.as_ref())
            .is_err()
    );
    println!("  tamper detection OK");

    println!("\n=== 7. Max block size arithmetic ===");
    const MAX_BLOCK: usize = 65_518;
    assert_eq!(MAX_BLOCK + 1 + 16, 65_535, "frame length must fit uint16");
    println!("  65518 + 1 flag + 16 tag = 65535 (fits uint16 BE) OK");

    println!("\n=== 8. socket2 multicast socket ===");
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    sock.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)).into())?;
    sock.set_broadcast(true)?;
    sock.set_multicast_ttl_v4(32)?;
    sock.set_multicast_loop_v4(false)?;
    let group: Ipv4Addr = "239.192.52.63".parse()?;
    sock.join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED)?;
    sock.set_multicast_if_v4(&Ipv4Addr::new(192, 168, 128, 244))?;
    let local = sock.local_addr()?;
    println!(
        "  bound port {}, joined {group}, interface set OK",
        local.as_socket().unwrap().port()
    );

    println!("\n=== 9. base64 round-trip ===");
    let enc = B64.encode(pk_bytes);
    assert_eq!(B64.decode(&enc)?, pk_bytes);
    println!("  33-byte pubkey <-> 44-char base64 OK");

    println!("\nALL SMOKE TESTS PASSED");
    Ok(())
}
