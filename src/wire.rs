// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! TCP framing and the encrypted record layer.
//!
//! Frame format (both directions, whole connection lifetime):
//!
//! ```text
//! ┌────────────────┬──────────────────────────────┐
//! │ uint16 BE len  │ len bytes of payload         │
//! └────────────────┴──────────────────────────────┘
//! ```
//!
//! Before the handshake completes the payload is plaintext JSON. Afterwards it is
//! always one ChaCha20-Poly1305 record.

use crate::crypto::{RecordDecryptor, RecordEncryptor, SessionKeys};
use crate::messages::Message;
use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::{self, ErrorKind, Read, Write};

/// `65535` (max uint16 length) minus the flag byte and the Poly1305 tag.
pub const MAX_BLOCK_SIZE: usize = 65_518;
pub const MAX_FRAME_LEN: usize = 65_535;

fn write_frame<W: Write>(writer: &mut W, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_FRAME_LEN {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            format!(
                "frame of {} bytes exceeds the uint16 length field",
                payload.len()
            ),
        ));
    }
    let len = (payload.len() as u16).to_be_bytes();
    writer.write_all(&len)?;
    writer.write_all(payload)?;
    writer.flush()
}

/// Fill `buf` completely. Returns `false` if EOF arrived before the first byte,
/// and an error if EOF arrived part-way through (a truncated frame).
fn read_full<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => {
                if filled == 0 {
                    return Ok(false);
                }
                return Err(io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "connection closed mid-frame",
                ));
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// Returns `None` on a clean end-of-stream at a frame boundary.
fn read_frame<R: Read>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 2];
    if !read_full(reader, &mut len_buf)? {
        return Ok(None);
    }
    let len = u16::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    if !read_full(reader, &mut payload)? {
        return Err(io::Error::new(
            ErrorKind::UnexpectedEof,
            "connection closed mid-frame",
        ));
    }
    Ok(Some(payload))
}

/// A framed, optionally encrypted connection.
///
/// Generic over the reader/writer so the protocol can be exercised over in-memory
/// streams in tests.
pub struct Connection<R: Read, W: Write> {
    reader: R,
    writer: W,
    rx: Option<RecordDecryptor>,
    tx: Option<RecordEncryptor>,
    keys: Option<SessionKeys>,
    peer_public_key: Option<[u8; 33]>,
    tx_buf: Vec<u8>,
}

impl<R: Read, W: Write> Connection<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            rx: None,
            tx: None,
            keys: None,
            peer_public_key: None,
            tx_buf: Vec::with_capacity(MAX_BLOCK_SIZE + 64),
        }
    }

    /// Take the writer back, for callers that own the connection outright — tests
    /// and benchmarks that need to read what was framed.
    pub fn into_writer(self) -> W {
        self.writer
    }

    /// Switch the connection to encrypted mode once the handshake has derived keys.
    pub fn enable_encryption(&mut self, keys: SessionKeys) {
        self.tx = Some(RecordEncryptor::new(&keys.tx));
        self.rx = Some(RecordDecryptor::new(&keys.rx));
        self.keys = Some(keys);
    }

    pub fn keys(&self) -> Option<&SessionKeys> {
        self.keys.as_ref()
    }

    pub fn verif_code(&self) -> Option<String> {
        self.keys.as_ref().map(SessionKeys::verif_code)
    }

    /// Access the underlying writer, e.g. to adjust socket-level timeouts. Both
    /// halves of a cloned `TcpStream` share the same socket options.
    pub fn writer(&self) -> &W {
        &self.writer
    }

    /// Access the underlying reader, so callers can adjust socket-level options
    /// that actually affect reads.
    pub fn reader(&self) -> &R {
        &self.reader
    }

    pub fn set_peer_public_key(&mut self, pk: [u8; 33]) {
        self.peer_public_key = Some(pk);
    }

    pub fn peer_public_key(&self) -> Option<[u8; 33]> {
        self.peer_public_key
    }

    // -- plaintext handshake frames -----------------------------------------

    pub fn send_plain_json<T: Serialize>(&mut self, value: &T) -> Result<()> {
        let bytes = serde_json::to_vec(value).context("serialising handshake frame")?;
        write_frame(&mut self.writer, &bytes).context("sending handshake frame")
    }

    pub fn recv_plain_json<T: DeserializeOwned>(&mut self) -> Result<Option<T>> {
        match read_frame(&mut self.reader).context("reading handshake frame")? {
            None => Ok(None),
            Some(bytes) => {
                let value = serde_json::from_slice(&bytes).context("parsing handshake frame")?;
                Ok(Some(value))
            }
        }
    }

    // -- encrypted application messages -------------------------------------

    pub fn send_message(&mut self, message: &Message) -> Result<()> {
        let bytes = serde_json::to_vec(message).context("serialising message")?;
        self.send_bytes(&bytes)
    }

    pub fn recv_message(&mut self) -> Result<Option<Message>> {
        match self.recv_bytes()? {
            None => Ok(None),
            Some(bytes) => Ok(Some(
                serde_json::from_slice(&bytes).context("parsing message")?,
            )),
        }
    }

    /// Take the transmit cipher out.
    ///
    /// Used when records are produced on another thread and only written here, so
    /// that sealing and the socket write overlap. Give it back with
    /// `restore_encryptor`: the nonce has to continue where it left off, or every
    /// later record would be encrypted with a reused one.
    pub fn take_encryptor(&mut self) -> Option<RecordEncryptor> {
        self.tx.take()
    }

    pub fn restore_encryptor(&mut self, tx: RecordEncryptor) {
        self.tx = Some(tx);
    }

    /// Write a record that `RecordEncryptor::seal` has already produced.
    ///
    /// Split out from `send_bytes` for the same reason as `take_encryptor`: the
    /// sealing and the writing can then happen on different threads.
    pub fn write_sealed(&mut self, sealed: &[u8]) -> Result<()> {
        write_frame(&mut self.writer, sealed).context("sending record")
    }

    /// Encrypt and send `data`, splitting into `MAX_BLOCK_SIZE` chunks and marking
    /// the final chunk with the flag byte. A zero-length payload still produces one
    /// record (with the flag set), as the protocol requires.
    pub fn send_bytes(&mut self, data: &[u8]) -> Result<()> {
        let Self {
            writer, tx, tx_buf, ..
        } = self;
        let tx = tx
            .as_mut()
            .ok_or_else(|| anyhow!("connection is not encrypted"))?;

        let mut rest = data;
        loop {
            let is_last = rest.len() <= MAX_BLOCK_SIZE;
            let chunk = if is_last {
                rest
            } else {
                &rest[..MAX_BLOCK_SIZE]
            };
            rest = if is_last {
                &[]
            } else {
                &rest[MAX_BLOCK_SIZE..]
            };

            tx.seal(chunk, is_last, tx_buf)?;
            write_frame(writer, &tx_buf[..]).context("sending record")?;
            if is_last {
                return Ok(());
            }
        }
    }

    /// Read records until one carries the final-chunk flag. `None` on clean EOF.
    pub fn recv_bytes(&mut self) -> Result<Option<Vec<u8>>> {
        let Self { reader, rx, .. } = self;
        let rx = rx
            .as_mut()
            .ok_or_else(|| anyhow!("connection is not encrypted"))?;

        let mut assembled: Vec<u8> = Vec::new();
        loop {
            let Some(record) = read_frame(reader).context("reading record")? else {
                return Ok(None);
            };
            let (payload, is_last) = rx.open(&record)?;
            assembled.extend_from_slice(&payload);
            if is_last {
                return Ok(Some(assembled));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_length_is_big_endian() {
        let mut writer = Vec::new();
        write_frame(&mut writer, b"hi").unwrap();
        assert_eq!(writer, vec![0x00, 0x02, b'h', b'i']);
    }

    #[test]
    fn frame_round_trips() {
        let mut writer = Vec::new();
        write_frame(&mut writer, b"hello").unwrap();
        write_frame(&mut writer, b"").unwrap();
        write_frame(&mut writer, &vec![0xabu8; 1000]).unwrap();

        let mut reader = io::Cursor::new(writer);
        assert_eq!(read_frame(&mut reader).unwrap().unwrap(), b"hello");
        assert_eq!(read_frame(&mut reader).unwrap().unwrap(), b"");
        assert_eq!(
            read_frame(&mut reader).unwrap().unwrap(),
            vec![0xabu8; 1000]
        );
        // Clean EOF at a frame boundary.
        assert!(read_frame(&mut reader).unwrap().is_none());
    }

    #[test]
    fn truncated_frame_is_an_error_not_eof() {
        let mut reader = io::Cursor::new(vec![0x00, 0x10, 0x01, 0x02]);
        assert!(read_frame(&mut reader).is_err());
    }

    #[test]
    fn oversized_frame_is_rejected() {
        let mut writer = Vec::new();
        let too_big = vec![0u8; MAX_FRAME_LEN + 1];
        assert!(write_frame(&mut writer, &too_big).is_err());
    }

    #[test]
    fn encrypt_then_decrypt_across_a_connection_pair() {
        use crate::crypto::{EphemeralKey, SessionKeys};

        let client_eph = EphemeralKey::generate();
        let server_eph = EphemeralKey::generate();
        let client_keys = SessionKeys::derive(&client_eph, &server_eph.public(), true);
        let server_keys = SessionKeys::derive(&server_eph, &client_eph.public(), false);

        // The client writes into a buffer; the server reads from it.
        let mut client: Connection<io::Cursor<Vec<u8>>, Vec<u8>> =
            Connection::new(io::Cursor::new(Vec::new()), Vec::new());
        client.enable_encryption(client_keys);
        client
            .send_message(&Message::device_info("cli", "windows"))
            .unwrap();

        let wire = client.writer;
        let mut server = Connection::new(io::Cursor::new(wire), Vec::new());
        server.enable_encryption(server_keys);

        let message = server.recv_message().unwrap().unwrap();
        assert_eq!(message.kind, "device_info");
        let info = message.device_info_of().unwrap();
        assert_eq!(info.name, "cli");
        assert_eq!(info.device_type, "windows");
    }

    /// A deterministic connection, so the bytes it produces can be pinned.
    fn fixed_connection() -> Connection<io::Cursor<Vec<u8>>, Vec<u8>> {
        use crate::crypto::{EphemeralKey, SessionKeys};

        let client = EphemeralKey::from_secret_bytes([7u8; 32]);
        let server = EphemeralKey::from_secret_bytes([9u8; 32]);
        let mut conn: Connection<io::Cursor<Vec<u8>>, Vec<u8>> =
            Connection::new(io::Cursor::new(Vec::new()), Vec::new());
        conn.enable_encryption(SessionKeys::derive(&client, &server.public(), true));
        conn
    }

    /// The bytes produced here go to a `LANDrop` v2 peer, so they are pinned.
    ///
    /// A send-path optimisation must not change a single byte: the peer
    /// has to keep accepting what we send. These vectors were captured *before* the
    /// send path stopped copying, and they are what proves the change was invisible
    /// on the wire rather than merely believed to be.
    #[test]
    fn the_wire_format_is_frozen() {
        let mut conn = fixed_connection();
        conn.send_bytes(b"landrop v2 wire format, frozen").unwrap();
        let wire = conn.into_writer();
        let hex: String = wire.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "002f66be8ad0ba207c4f3fbc282d07dfca9bd2ef40e767c73c7d16cb4892abb2c72aeb78562cc6575201d84427e3238d1a",
            "a single record changed on the wire"
        );

        // Multi-record, where the framing boundaries and the nonce progression both
        // matter. Hashed rather than spelled out: 197 KB of hex proves nothing more.
        let mut conn = fixed_connection();
        let payload: Vec<u8> = (0..(MAX_BLOCK_SIZE * 3 + 1234))
            .map(|i| (i % 251) as u8)
            .collect();
        conn.send_bytes(&payload).unwrap();
        let wire = conn.into_writer();
        assert_eq!(wire.len(), 197_864, "record framing changed size");
        assert_eq!(
            crate::crypto::b64_encode(&crate::crypto::blake2b_256(&wire)),
            "5bcDpP+dxQTxgy7Bl8UJ6/lFz5WWcZvfvfeKTBwY2ek=",
            "the multi-record stream changed on the wire"
        );
    }

    #[test]
    fn multi_record_payload_round_trips_exactly() {
        use crate::crypto::{EphemeralKey, SessionKeys};

        // Three full blocks plus a remainder forces four records.
        let payload: Vec<u8> = (0..(MAX_BLOCK_SIZE * 3 + 1234))
            .map(|i| (i % 251) as u8)
            .collect();

        let client_eph = EphemeralKey::generate();
        let server_eph = EphemeralKey::generate();
        let mut client: Connection<io::Cursor<Vec<u8>>, Vec<u8>> =
            Connection::new(io::Cursor::new(Vec::new()), Vec::new());
        client.enable_encryption(SessionKeys::derive(&client_eph, &server_eph.public(), true));
        client.send_bytes(&payload).unwrap();

        let wire = client.writer;
        let mut server = Connection::new(io::Cursor::new(wire), Vec::new());
        server.enable_encryption(SessionKeys::derive(
            &server_eph,
            &client_eph.public(),
            false,
        ));

        let received = server.recv_bytes().unwrap().unwrap();
        assert_eq!(received.len(), payload.len());
        assert_eq!(received, payload);
    }
}
