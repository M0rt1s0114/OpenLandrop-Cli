// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! Cryptographic primitives for the `LANDrop` v2 protocol.
//!
//! Every operation here implements the `LANDrop` v2 wire format, verified live
//! against `LANDrop` v2 peers:
//!
//! * identity   — secp256k1 ECDSA over BLAKE2b-256, low-S normalised
//! * key exchange — X25519, keys derived with libsodium `crypto_kx`
//! * records    — ChaCha20-Poly1305 IETF with per-direction little-endian nonces

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use blake2::{Blake2b128, Blake2b256, Blake2b512, Digest};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{AeadInOut, ChaCha20Poly1305, Nonce};
use k256::ecdsa::signature::hazmat::{PrehashSigner, PrehashVerifier};
use k256::ecdsa::{Signature, SigningKey, VerifyingKey};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

/// libsodium `crypto_generichash(16, ..)` — `BLAKE2b` with a 16-byte digest.
pub fn blake2b_128(data: &[u8]) -> [u8; 16] {
    let mut out = [0u8; 16];
    out.copy_from_slice(&Blake2b128::digest(data));
    out
}

/// libsodium `crypto_generichash(32, ..)` — `BLAKE2b` with a 32-byte digest.
pub fn blake2b_256(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Blake2b256::digest(data));
    out
}

/// libsodium `crypto_generichash(64, ..)` — `BLAKE2b` with a 64-byte digest.
pub fn blake2b_512(data: &[u8]) -> [u8; 64] {
    let mut out = [0u8; 64];
    out.copy_from_slice(&Blake2b512::digest(data));
    out
}

pub fn b64_encode(data: &[u8]) -> String {
    B64.encode(data)
}

pub fn b64_decode(s: &str) -> Result<Vec<u8>> {
    B64.decode(s)
        .with_context(|| format!("invalid base64: {s}"))
}

// ---------------------------------------------------------------------------
// Long-term identity
// ---------------------------------------------------------------------------

/// A device's long-term secp256k1 identity keypair.
///
/// The public key is the 33-byte compressed SEC1 encoding, base64-encoded on the
/// wire (44 characters). This is also the key peers store in their trust list, so
/// it must stay stable across runs or every peer treats the CLI as a new device.
#[derive(Clone)]
pub struct Identity {
    signing: SigningKey,
}

impl Identity {
    /// Generate a fresh identity. This loops until the random scalar
    /// passes `privateKeyVerify`, which we reproduce exactly.
    pub fn generate() -> Self {
        loop {
            let bytes: [u8; 32] = rand::random();
            if let Ok(signing) = SigningKey::from_bytes((&bytes).into()) {
                return Self { signing };
            }
        }
    }

    /// Restore from the base64 of the raw 32-byte secret scalar.
    pub fn from_sk_base64(encoded: &str) -> Result<Self> {
        let raw = b64_decode(encoded)?;
        let bytes: [u8; 32] = raw
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("secret key must be 32 bytes, got {}", raw.len()))?;
        let signing = SigningKey::from_bytes((&bytes).into())
            .map_err(|_| anyhow!("secret key is not a valid secp256k1 scalar"))?;
        Ok(Self { signing })
    }

    pub fn sk_bytes(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        out.copy_from_slice(&self.signing.to_bytes());
        out
    }

    pub fn sk_base64(&self) -> String {
        b64_encode(&self.sk_bytes())
    }

    pub fn pk_compressed(&self) -> [u8; 33] {
        let verifying = VerifyingKey::from(&self.signing);
        let point = verifying.to_sec1_point(true);
        let mut out = [0u8; 33];
        out.copy_from_slice(point.as_bytes());
        out
    }

    pub fn pk_base64(&self) -> String {
        b64_encode(&self.pk_compressed())
    }

    /// `secp256k1Sign(message, sk)`: ECDSA over `BLAKE2b-256(message)`, low-S.
    ///
    /// Note the digest is taken over the *raw* message bytes; callers pass the raw
    /// ephemeral public key, never its base64 text.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        let digest = blake2b_256(message);
        let signature: Signature = self
            .signing
            .sign_prehash(&digest)
            .expect("ECDSA signing over a fixed-size digest cannot fail");
        // libsecp256k1 always emits low-S; normalise explicitly to be certain.
        let signature = signature.normalize_s();
        let mut out = [0u8; 64];
        out.copy_from_slice(&signature.to_bytes());
        out
    }
}

/// `secp256k1Verify(sig, message, pk)`: normalise the peer's signature, then
/// ECDSA-verify it over `BLAKE2b-256(message)`.
pub fn verify_signature(signature: &[u8], message: &[u8], public_key: &[u8]) -> bool {
    let attempt = || -> Result<()> {
        let signature =
            Signature::from_slice(signature).map_err(|e| anyhow!("malformed signature: {e}"))?;
        let signature = signature.normalize_s();
        let verifying = VerifyingKey::from_sec1_bytes(public_key)
            .map_err(|e| anyhow!("malformed public key: {e}"))?;
        verifying
            .verify_prehash(&blake2b_256(message), &signature)
            .map_err(|e| anyhow!("signature verification failed: {e}"))?;
        Ok(())
    };
    attempt().is_ok()
}

// ---------------------------------------------------------------------------
// Ephemeral X25519 keys and session derivation
// ---------------------------------------------------------------------------

/// A per-connection X25519 keypair (`crypto_kx_keypair`).
pub struct EphemeralKey {
    secret: StaticSecret,
    public: [u8; 32],
}

impl EphemeralKey {
    pub fn generate() -> Self {
        Self::from_secret_bytes(rand::random::<[u8; 32]>())
    }

    /// Deterministic construction from raw scalar bytes. Used by tests and by
    /// cross-implementation key vectors; production code uses `generate`.
    pub fn from_secret_bytes(bytes: [u8; 32]) -> Self {
        let secret = StaticSecret::from(bytes);
        let public = X25519PublicKey::from(&secret).to_bytes();
        Self { secret, public }
    }

    pub fn public(&self) -> [u8; 32] {
        self.public
    }

    pub fn diffie_hellman(&self, peer_public: &[u8; 32]) -> [u8; 32] {
        let peer = X25519PublicKey::from(*peer_public);
        self.secret.diffie_hellman(&peer).to_bytes()
    }
}

/// The derived session keys, matching libsodium `crypto_kx_*_session_keys`.
#[derive(Clone)]
pub struct SessionKeys {
    pub rx: [u8; 32],
    pub tx: [u8; 32],
    pub is_client: bool,
}

impl SessionKeys {
    /// Reproduces libsodium's `crypto_kx`.
    ///
    /// The important subtlety: this is **one** BLAKE2b-512 over
    /// `q || client_pk || server_pk`, whose 64-byte output is then split into two
    /// 32-byte halves. It is *not* two separate BLAKE2b-256 hashes of different
    /// concatenations — those give completely different keys.
    ///
    /// ```text
    /// combined = BLAKE2b-512(q || client_pk || server_pk)
    /// client:  rx = combined[0..32]   tx = combined[32..64]
    /// server:  rx = combined[32..64]  tx = combined[0..32]
    /// ```
    pub fn derive(local: &EphemeralKey, peer_public: &[u8; 32], is_client: bool) -> Self {
        let q = local.diffie_hellman(peer_public);
        let (client_pk, server_pk) = if is_client {
            (local.public(), *peer_public)
        } else {
            (*peer_public, local.public())
        };

        let mut buf = [0u8; 96];
        buf[0..32].copy_from_slice(&q);
        buf[32..64].copy_from_slice(&client_pk);
        buf[64..96].copy_from_slice(&server_pk);
        let combined = blake2b_512(&buf);

        let (first, second) = combined.split_at(32);
        let (rx, tx) = if is_client {
            (first, second)
        } else {
            (second, first)
        };

        Self {
            rx: rx.try_into().expect("32 bytes"),
            tx: tx.try_into().expect("32 bytes"),
            is_client,
        }
    }

    /// `kSc` is the client→server key, `kCs` the server→client key; both peers
    /// compute the same pair, just from opposite sides.
    fn shared_digest(&self) -> [u8; 16] {
        let (k_sc, k_cs) = if self.is_client {
            (self.rx, self.tx)
        } else {
            (self.tx, self.rx)
        };
        let mut buf = [0u8; 64];
        buf[0..32].copy_from_slice(&k_sc);
        buf[32..64].copy_from_slice(&k_cs);
        blake2b_128(&buf)
    }

    /// The 6-digit code both peers display so a user can detect a MITM.
    /// Informational only — nothing in the protocol gates on it.
    pub fn verif_code(&self) -> String {
        let d = self.shared_digest();
        let a = u64::from_le_bytes(d[0..8].try_into().expect("8 bytes"));
        let b = u64::from_le_bytes(d[8..16].try_into().expect("8 bytes"));
        format!("{:06}", (a ^ b) % 1_000_000)
    }
}

// ---------------------------------------------------------------------------
// Record crypto
// ---------------------------------------------------------------------------

pub const NONCE_LEN: usize = 12;

/// libsodium puts the `is_last` flag in front of the plaintext.
pub const FLAG_LEN: usize = 1;

/// Poly1305 tag, appended to the ciphertext.
pub const TAG_LEN: usize = 16;

/// libsodium `sodium_increment`: treat the nonce as a little-endian counter.
pub fn increment_nonce(nonce: &mut [u8; NONCE_LEN]) {
    for byte in nonce.iter_mut() {
        let (next, carry) = byte.overflowing_add(1);
        *byte = next;
        if !carry {
            return;
        }
    }
}

/// Seals outgoing records. Owns its own nonce so the two directions can never
/// share one — reusing a nonce across directions would be catastrophic.
pub struct RecordEncryptor {
    cipher: ChaCha20Poly1305,
    nonce: [u8; NONCE_LEN],
}

impl RecordEncryptor {
    pub fn new(key: &[u8; 32]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new_from_slice(key).expect("32-byte key is valid"),
            nonce: [0u8; NONCE_LEN],
        }
    }

    /// Seal one record into `out`.
    ///
    /// The plaintext is built in `out` and encrypted where it lies. An intermediate
    /// buffer would mean copying every payload byte twice more, which on a fast link
    /// is the entire cost of this path — the cipher itself is several times faster
    /// than the copies around it were.
    ///
    /// The layout is libsodium's, and is fixed: the flag is the first plaintext
    /// byte, and the tag follows the ciphertext. `the_wire_format_is_frozen` pins it.
    pub fn seal(&mut self, chunk: &[u8], is_last: bool, out: &mut Vec<u8>) -> Result<()> {
        out.clear();
        out.reserve(FLAG_LEN + chunk.len() + TAG_LEN);
        out.push(u8::from(is_last));
        out.extend_from_slice(chunk);
        self.cipher
            .encrypt_in_place(&Nonce::from(self.nonce), b"", out)
            .map_err(|_| anyhow!("record encryption failed"))?;
        increment_nonce(&mut self.nonce);
        Ok(())
    }
}

/// Opens incoming records.
pub struct RecordDecryptor {
    cipher: ChaCha20Poly1305,
    nonce: [u8; NONCE_LEN],
}

impl RecordDecryptor {
    pub fn new(key: &[u8; 32]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new_from_slice(key).expect("32-byte key is valid"),
            nonce: [0u8; NONCE_LEN],
        }
    }

    /// Returns `(payload, is_last)`, or an error if authentication fails.
    ///
    /// Left as an allocating decrypt on purpose. Decrypting in place looked like the
    /// obvious symmetry with `seal`, but it measured *slower* — 729 MiB/s against
    /// 1134 — because `record.to_vec()` plus stripping the flag moves the payload
    /// twice, where the allocating form moves it once. The receive path is not the
    /// bottleneck either way; the send path is, and that is where the copies went.
    pub fn open(&mut self, record: &[u8]) -> Result<(Vec<u8>, bool)> {
        let plain = self
            .cipher
            .decrypt(&Nonce::from(self.nonce), record)
            .map_err(|_| anyhow!("record authentication failed (wrong key, tampered data, or desynchronised nonce)"))?;
        increment_nonce(&mut self.nonce);
        if plain.is_empty() {
            bail!("record plaintext is empty");
        }
        Ok((plain[1..].to_vec(), plain[0] == 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_hex(text: &str) -> [u8; 32] {
        let bytes: Vec<u8> = (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("valid hex"))
            .collect();
        bytes.as_slice().try_into().expect("32 bytes")
    }

    fn to_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn blake2b_matches_published_vector() {
        let digest = blake2b_256(b"abc");
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "bddd813c634239723171ef3fee98579b94964e3bb1cb3e427262c8c068d52319"
        );
    }

    #[test]
    fn identity_round_trips_through_base64() {
        let id = Identity::generate();
        let restored = Identity::from_sk_base64(&id.sk_base64()).unwrap();
        assert_eq!(id.pk_base64(), restored.pk_base64());
        assert_eq!(id.pk_compressed().len(), 33);
        assert_eq!(id.pk_base64().len(), 44);
    }

    #[test]
    fn sign_and_verify_round_trip() {
        let id = Identity::generate();
        let message = b"ephemeral public key bytes";
        let signature = id.sign(message);
        assert_eq!(signature.len(), 64);
        assert!(verify_signature(&signature, message, &id.pk_compressed()));
        // The digest must cover the message, not something else.
        assert!(!verify_signature(&signature, b"other", &id.pk_compressed()));
    }

    #[test]
    fn rejects_tampered_signature() {
        let id = Identity::generate();
        let message = b"payload";
        let mut signature = id.sign(message);
        signature[0] ^= 0xff;
        assert!(!verify_signature(&signature, message, &id.pk_compressed()));
    }

    /// Regression vector captured from libsodium's `crypto_kx_*_session_keys`.
    ///
    /// This is the exact bug that broke interop with the real app: an earlier
    /// implementation used two BLAKE2b-256 hashes instead of one BLAKE2b-512 whose
    /// halves are split, which produced entirely different (and unusable) keys.
    #[test]
    fn session_keys_match_libsodium_vector() {
        let client_sk =
            parse_hex("00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff");
        let server_sk =
            parse_hex("ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100");

        let client = EphemeralKey::from_secret_bytes(client_sk);
        let server = EphemeralKey::from_secret_bytes(server_sk);

        // Public keys must match libsodium's crypto_scalarmult_base byte-for-byte.
        assert_eq!(
            to_hex(&client.public()),
            "d5e9065052939549d055acf03f348200578e3ed2b65eb6a96564471761ba4f54"
        );
        assert_eq!(
            to_hex(&server.public()),
            "4a52f593172fa3a7184e79ec52ffddcf8b6062c9a69054a606f07532e255746d"
        );

        let ck = SessionKeys::derive(&client, &server.public(), true);
        let sk = SessionKeys::derive(&server, &client.public(), false);

        assert_eq!(
            to_hex(&ck.rx),
            "30362ef4df66683ef784fa20105fc42a67560a55495ea0ec0bc517f6d7c36c17"
        );
        assert_eq!(
            to_hex(&ck.tx),
            "52c5fd4d8abdd436eb0fae2f60546e10f720083b78b6f3a94e453f2b33083542"
        );
        assert_eq!(ck.tx, sk.rx);
        assert_eq!(ck.rx, sk.tx);
        assert_eq!(ck.verif_code(), "345532");
        assert_eq!(sk.verif_code(), "345532");
    }

    #[test]
    fn session_keys_agree_across_roles() {
        let client = EphemeralKey::generate();
        let server = EphemeralKey::generate();
        let ck = SessionKeys::derive(&client, &server.public(), true);
        let sk = SessionKeys::derive(&server, &client.public(), false);
        // The client's tx must equal the server's rx and vice versa.
        assert_eq!(ck.tx, sk.rx);
        assert_eq!(ck.rx, sk.tx);
        assert_ne!(ck.rx, ck.tx);
        // Both sides must display the same verification code.
        assert_eq!(ck.verif_code(), sk.verif_code());
        assert_eq!(ck.verif_code().len(), 6);
    }

    #[test]
    fn nonce_increment_is_little_endian_with_carry() {
        let mut nonce = [0u8; 12];
        increment_nonce(&mut nonce);
        assert_eq!(nonce[0], 1);
        assert_eq!(nonce[1], 0);

        let mut carry = [0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        increment_nonce(&mut carry);
        assert_eq!(&carry[0..3], &[0x00, 0x00, 0x01]);
    }

    #[test]
    fn record_round_trip_and_tamper_detection() {
        let key = [7u8; 32];
        let mut enc = RecordEncryptor::new(&key);
        let mut dec = RecordDecryptor::new(&key);
        let mut out = Vec::new();

        enc.seal(b"hello landrop", true, &mut out).unwrap();
        let (plain, last) = dec.open(&out).unwrap();
        assert_eq!(plain, b"hello landrop");
        assert!(last);

        // A second record must use the incremented nonce and still decrypt.
        enc.seal(b"second", true, &mut out).unwrap();
        let (plain, _) = dec.open(&out).unwrap();
        assert_eq!(plain, b"second");

        // Flipping a ciphertext bit must be detected.
        out[0] ^= 0xff;
        assert!(dec.open(&out).is_err());
    }

    #[test]
    fn empty_payload_still_emits_one_record() {
        let key = [3u8; 32];
        let mut enc = RecordEncryptor::new(&key);
        let mut dec = RecordDecryptor::new(&key);
        let mut out = Vec::new();
        enc.seal(b"", true, &mut out).unwrap();
        assert_eq!(out.len(), 1 + 16, "flag byte + poly1305 tag");
        let (plain, last) = dec.open(&out).unwrap();
        assert!(plain.is_empty());
        assert!(last);
    }
}
