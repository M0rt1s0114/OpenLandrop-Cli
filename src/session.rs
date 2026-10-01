// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! Client and server sides of the `LANDrop` v2 connection state machine.

use crate::crypto::{
    EphemeralKey, Identity, RecordEncryptor, SessionKeys, b64_decode, b64_encode, verify_signature,
};
use crate::messages::{
    DeviceInfo, FileDescriptor, FileSendRequest, Message, SUPPORTED_MESSAGE_TYPES,
};
use crate::transfer::{FileLeaf, IncomingFile, apply_metadata, prepare_incoming, total_size};
use crate::wire::{Connection, MAX_BLOCK_SIZE};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

/// Our own advertised identity.
#[derive(Debug, Clone)]
pub struct LocalDevice {
    pub name: String,
    pub device_type: String,
}

// ---------------------------------------------------------------------------
// Handshake frames
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Ecdhe {
    public_key: String,
    sig: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClientHello {
    supported_versions: Vec<String>,
    public_key: String,
    ecdhe: Ecdhe,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ServerHello {
    version: String,
    public_key: String,
    ecdhe: Ecdhe,
}

/// The signature binds the *raw* ephemeral public key bytes, so both sides check it
/// the same way. Returns `(identity_pk, ephemeral_pk)`.
fn verify_hello(public_key_b64: &str, ecdhe: &Ecdhe) -> Result<([u8; 33], [u8; 32])> {
    let identity_pk = b64_decode(public_key_b64).context("decoding peer identity key")?;
    let ephemeral_pk = b64_decode(&ecdhe.public_key).context("decoding peer ephemeral key")?;
    let signature = b64_decode(&ecdhe.sig).context("decoding peer signature")?;

    if !verify_signature(&signature, &ephemeral_pk, &identity_pk) {
        bail!("handshake failed: the peer's ephemeral key signature did not verify");
    }

    let identity_pk: [u8; 33] = identity_pk
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("peer identity key must be 33 bytes"))?;
    let ephemeral_pk: [u8; 32] = ephemeral_pk
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("peer ephemeral key must be 32 bytes"))?;
    Ok((identity_pk, ephemeral_pk))
}

pub type TcpConnection = Connection<BufReader<TcpStream>, TcpStream>;

/// Wrap an accepted/connected socket with a large read buffer.
///
/// `TCP_NODELAY` keeps latency low; the large `BufReader`
/// capacity keeps the per-record syscall count low.
pub fn wrap_tcp(stream: TcpStream) -> Result<TcpConnection> {
    stream.set_nodelay(true).context("setting TCP_NODELAY")?;
    let writer = stream.try_clone().context("cloning the TCP stream")?;
    let reader = BufReader::with_capacity(1 << 20, stream);
    Ok(Connection::new(reader, writer))
}

/// Whether an error is a read timeout rather than a real failure.
///
/// A socket read timeout surfaces as `WouldBlock` on Windows and as `WouldBlock`
/// or `TimedOut` depending on the platform, always wrapped by our own context, so
/// the whole chain has to be searched.
fn is_read_timeout(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|io| {
            matches!(
                io.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            )
        })
    })
}

// ---------------------------------------------------------------------------
// Client (the sending side)
// ---------------------------------------------------------------------------

pub struct ClientSession {
    conn: TcpConnection,
    pub peer_device: Option<DeviceInfo>,
    pub peer_supported_types: Option<Vec<String>>,
}

impl ClientSession {
    /// Connect and complete the cryptographic handshake as the client role.
    pub fn connect(
        addr: SocketAddr,
        identity: &Identity,
        expected_peer_public_key: Option<&str>,
        connect_timeout: Duration,
    ) -> Result<Self> {
        let stream = match TcpStream::connect_timeout(&addr, connect_timeout) {
            Ok(stream) => stream,
            // A refusal means the host answered and nothing is listening there.
            // Silence means the packets are being dropped rather than rejected,
            // and a firewall on that device is the usual reason. Saying so is
            // worth the branch: from here it is otherwise indistinguishable from
            // the peer simply being slow to start.
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                bail!(
                    "no answer from {addr} after {connect_timeout:?}: the connection timed \
                     out rather than being refused, so the packets are being dropped. A \
                     firewall on that device is the usual cause — a receiver running there \
                     prints the exact rule to add when it finds one missing."
                );
            }
            Err(error) => {
                return Err(error).with_context(|| format!("could not connect to {addr}"));
            }
        };
        stream
            .set_read_timeout(Some(connect_timeout))
            .context("setting handshake read timeout")?;
        let mut conn = wrap_tcp(stream)?;

        let ephemeral = EphemeralKey::generate();
        let hello = ClientHello {
            supported_versions: vec![crate::PROTOCOL_VERSION.to_string()],
            public_key: identity.pk_base64(),
            ecdhe: Ecdhe {
                public_key: b64_encode(&ephemeral.public()),
                sig: b64_encode(&identity.sign(&ephemeral.public())),
            },
        };
        conn.send_plain_json(&hello)
            .context("sending ClientHello")?;

        let reply: ServerHello = conn
            .recv_plain_json()
            .context("reading ServerHello")?
            .ok_or_else(|| anyhow!("peer closed the connection before sending a ServerHello"))?;

        if reply.version != crate::PROTOCOL_VERSION {
            bail!(
                "peer speaks version {:?}, which this client does not support",
                reply.version
            );
        }
        let (peer_identity_pk, peer_ephemeral_pk) = verify_hello(&reply.public_key, &reply.ecdhe)?;

        if let Some(expected) = expected_peer_public_key
            && reply.public_key != expected
        {
            bail!(
                "peer identity mismatch: expected {expected}, got {}",
                reply.public_key
            );
        }

        let keys = SessionKeys::derive(&ephemeral, &peer_ephemeral_pk, true);
        conn.enable_encryption(keys);
        conn.set_peer_public_key(peer_identity_pk);

        Ok(Self {
            conn,
            peer_device: None,
            peer_supported_types: None,
        })
    }

    pub fn peer_public_key_base64(&self) -> String {
        self.conn
            .peer_public_key()
            .map(|pk| b64_encode(&pk))
            .unwrap_or_default()
    }

    /// The 6-digit code the user can compare against the receiving device.
    pub fn verif_code(&self) -> String {
        self.conn.verif_code().unwrap_or_default()
    }

    /// Give the user time to walk over and press Accept on the receiving device.
    ///
    /// The timeout is applied to the reader's stream explicitly: the write half is
    /// a duplicated handle of the same socket, and relying on the option being
    /// shared proved unreliable on Windows.
    pub fn set_reply_timeout(&self, timeout: Duration) -> Result<()> {
        self.conn
            .reader()
            .get_ref()
            .set_read_timeout(Some(timeout))
            .context("setting the reply timeout on the read half")?;
        let _ = self.conn.writer().set_read_timeout(Some(timeout));
        Ok(())
    }

    /// Wait for the peer to close the connection.
    ///
    /// The protocol has no end-of-transfer acknowledgement: the receiver writes the
    /// bytes and then goes quiet, so the connection closing is the only signal that
    /// it has finished with them. Returns `false` if the timeout expired first,
    /// which is not a failure — a peer is free to keep the connection open.
    pub fn wait_for_close(&mut self, timeout: Duration) -> Result<bool> {
        self.set_reply_timeout(timeout)?;
        loop {
            match self.conn.recv_message() {
                // A clean shutdown is exactly what we are waiting for.
                Ok(None) => return Ok(true),
                // Anything else arriving is not a completion signal.
                Ok(Some(_)) => {}
                Err(error) if is_read_timeout(&error) => return Ok(false),
                Err(error) => return Err(error),
            }
        }
    }

    pub fn exchange_supported_message_types(&mut self) -> Result<Vec<String>> {
        self.conn
            .send_message(&Message::supported_message_types())?;
        let reply = self
            .conn
            .recv_message()?
            .ok_or_else(|| anyhow!("peer closed during the capability exchange"))?;
        if reply.kind != "supported_message_types" {
            bail!("expected supported_message_types, got {:?}", reply.kind);
        }
        let types: Vec<String> = reply
            .data
            .get("types")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        self.peer_supported_types = Some(types.clone());
        Ok(types)
    }

    pub fn exchange_device_info(&mut self, local: &LocalDevice) -> Result<DeviceInfo> {
        self.conn
            .send_message(&Message::device_info(&local.name, &local.device_type))?;
        let reply = self
            .conn
            .recv_message()?
            .ok_or_else(|| anyhow!("peer closed during the device_info exchange"))?;
        if reply.kind != "device_info" {
            bail!("expected device_info, got {:?}", reply.kind);
        }
        let info = reply
            .device_info_of()
            .ok_or_else(|| anyhow!("malformed device_info payload"))?;
        self.peer_device = Some(info.clone());
        Ok(info)
    }

    /// Offer the files and, if accepted, stream their contents.
    ///
    /// Returns the number of payload bytes written. Empty files contribute nothing:
    /// there is nothing to read, and they are created anyway.
    pub fn send_files(
        &mut self,
        leaves: &[FileLeaf],
        descriptors: &[FileDescriptor],
        on_progress: &mut dyn FnMut(u64, u64),
    ) -> Result<u64> {
        let expected_total = total_size(descriptors);
        self.conn
            .send_message(&Message::file_send_request(descriptors))?;

        let reply = self
            .conn
            .recv_message()?
            .ok_or_else(|| anyhow!("the recipient closed the connection"))?;
        if reply.kind != "file_send_request_reply" {
            bail!(
                "the recipient does not support receiving files (it replied {:?})",
                reply.kind
            );
        }
        let reply = reply
            .file_send_request_reply_of()
            .ok_or_else(|| anyhow!("malformed file_send_request_reply"))?;
        if !reply.accept {
            bail!("the recipient declined the transfer");
        }

        // Reading and sealing move to a worker so they overlap with the previous
        // record going out. Over 512 MiB on this machine that is 0.074s of reading
        // plus 0.35s of sealing against a 0.21s socket write: run in one thread they
        // add up, run in two the write hides inside the sealing.
        let tx = self
            .conn
            .take_encryptor()
            .ok_or_else(|| anyhow!("connection is not encrypted"))?;

        let sources: Vec<Source> = leaves
            .iter()
            .zip(descriptors.iter())
            .filter(|(_, descriptor)| descriptor.size > 0) // nothing to read, no bytes to send
            .map(|(leaf, descriptor)| Source {
                path: leaf.path.clone(),
                name: leaf.abstract_name.clone(),
                size: descriptor.size,
            })
            .collect();

        let (frames, queue) = sync_channel::<SealedFrame>(SEND_QUEUE_DEPTH);
        let (outcome, result) = sync_channel::<Result<(RecordEncryptor, u64)>>(1);

        let worker = std::thread::Builder::new()
            .name("landrop-reader".to_string())
            .spawn(move || {
                let _ = outcome.send(seal_files(tx, &sources, &frames));
            })
            .context("spawning the file reader")?;

        // Writing is all this thread does, which is the point: it never waits on a
        // disk read or a cipher, so the worker can stay ahead of it.
        let mut written: u64 = 0;
        let mut write_error = None;
        while let Ok(frame) = queue.recv() {
            if let Err(error) = self.conn.write_sealed(&frame.bytes) {
                write_error = Some(error);
                break;
            }
            written += frame.plain_len;
            on_progress(written, expected_total);
        }

        // Dropping the queue ends the worker at its next send, so it always reaches
        // the outcome channel and the cipher always comes back.
        drop(queue);
        let sealed = result
            .recv()
            .unwrap_or_else(|_| Err(anyhow!("the file reader stopped without reporting")));
        let _ = worker.join();

        let (tx, sent) = sealed?;
        self.conn.restore_encryptor(tx);
        if let Some(error) = write_error {
            return Err(error);
        }
        debug_assert_eq!(
            sent, written,
            "every sealed byte must reach the socket before this returns"
        );
        Ok(sent)
    }

    pub fn send_text(&mut self, text: &str) -> Result<()> {
        self.conn.send_message(&Message::text_send(text))?;
        // One message comes back and is discarded: the format defines no other
        // answer to a text message, and waiting for it makes the send synchronous.
        let _ = self.conn.recv_message()?;
        Ok(())
    }

    pub fn close(self) {
        // Dropping the connection closes the socket.
    }
}

/// How many sealed records may wait between the reader and the socket.
///
/// Lookahead to cover the socket write, not a buffer to absorb a transfer: without
/// a bound, a fast disk would read a whole file into memory ahead of a slow link.
const SEND_QUEUE_DEPTH: usize = 4;

/// One file to read, resolved before the worker starts so it owns no borrows.
struct Source {
    path: PathBuf,
    name: String,
    size: u64,
}

/// One sealed record on its way to the socket.
struct SealedFrame {
    /// Exactly what goes on the wire: flag, ciphertext, tag.
    bytes: Vec<u8>,
    /// Plaintext bytes carried, for progress and for the final count.
    plain_len: u64,
}

/// Read and seal every source, handing finished records to the writing thread.
///
/// The declared size is authoritative: the receiver reads exactly that many bytes
/// and there is no in-band end-of-file marker, so every read is capped at the
/// remaining count and a file that turns out to be shorter fails loudly. Silently
/// sending fewer bytes would leave the receiver blocked forever waiting for the rest.
fn seal_files(
    mut tx: RecordEncryptor,
    sources: &[Source],
    frames: &SyncSender<SealedFrame>,
) -> Result<(RecordEncryptor, u64)> {
    let mut frame: Vec<u8> = Vec::with_capacity(MAX_BLOCK_SIZE + 64);
    let mut buffer = vec![0u8; MAX_BLOCK_SIZE];
    let mut sent: u64 = 0;

    for source in sources {
        let mut file = File::open(&source.path)
            .with_context(|| format!("cannot open {}", source.path.display()))?;
        let mut remaining = source.size;

        while remaining > 0 {
            let want = remaining.min(MAX_BLOCK_SIZE as u64) as usize;
            let read = file
                .read(&mut buffer[..want])
                .with_context(|| format!("reading {}", source.path.display()))?;
            if read == 0 {
                bail!(
                    "{} shrank while it was being sent ({} of {} bytes available); \
                     the recipient would wait forever for the rest",
                    source.name,
                    source.size - remaining,
                    source.size
                );
            }

            let plain_len = read as u64;
            tx.seal(&buffer[..read], true, &mut frame)?;
            remaining -= plain_len;
            sent += plain_len;

            // Taken rather than borrowed so the writer owns what it is handed. The
            // fresh allocation per record is the price of crossing a thread.
            let bytes = std::mem::take(&mut frame);
            if frames.send(SealedFrame { bytes, plain_len }).is_err() {
                bail!("the connection stopped accepting records");
            }
        }
    }
    Ok((tx, sent))
}

// ---------------------------------------------------------------------------
// Server (the receiving side)
// ---------------------------------------------------------------------------

/// Everything the receive loop needs to know about us.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub local: LocalDevice,
    pub download_dir: PathBuf,
}

/// What the handler is told before deciding whether to accept a transfer.
pub struct OfferContext<'a> {
    pub peer_name: String,
    pub peer_type: String,
    pub peer_public_key: String,
    pub verif_code: String,
    pub request: &'a FileSendRequest,
    pub total_size: u64,
}

/// Callbacks driving the receive loop.
pub struct ReceiveCallbacks<'a> {
    /// Return `true` to accept the offer.
    pub decide: &'a mut dyn FnMut(&OfferContext<'_>) -> Result<bool>,
    pub progress: &'a mut dyn FnMut(u64, u64),
    pub text: &'a mut dyn FnMut(&str),
    pub file_done: &'a mut dyn FnMut(&IncomingFile),
}

pub struct ServerSession {
    conn: TcpConnection,
    pub peer_device: Option<DeviceInfo>,
}

impl ServerSession {
    /// Complete the handshake as the server role on an accepted socket.
    pub fn accept(
        stream: TcpStream,
        identity: &Identity,
        handshake_timeout: Duration,
    ) -> Result<Self> {
        stream
            .set_read_timeout(Some(handshake_timeout))
            .context("setting handshake read timeout")?;
        let mut conn = wrap_tcp(stream)?;

        let hello: ClientHello = conn
            .recv_plain_json()
            .context("reading ClientHello")?
            .ok_or_else(|| anyhow!("peer closed before sending a ClientHello"))?;

        if !hello
            .supported_versions
            .iter()
            .any(|v| v.as_str() == crate::PROTOCOL_VERSION)
        {
            bail!(
                "peer does not support v2 (offered {:?})",
                hello.supported_versions
            );
        }
        let (peer_identity_pk, peer_ephemeral_pk) = verify_hello(&hello.public_key, &hello.ecdhe)?;

        let ephemeral = EphemeralKey::generate();
        let reply = ServerHello {
            version: crate::PROTOCOL_VERSION.to_string(),
            public_key: identity.pk_base64(),
            ecdhe: Ecdhe {
                public_key: b64_encode(&ephemeral.public()),
                sig: b64_encode(&identity.sign(&ephemeral.public())),
            },
        };
        conn.send_plain_json(&reply)
            .context("sending ServerHello")?;

        let keys = SessionKeys::derive(&ephemeral, &peer_ephemeral_pk, false);
        conn.enable_encryption(keys);
        conn.set_peer_public_key(peer_identity_pk);

        Ok(Self {
            conn,
            peer_device: None,
        })
    }

    pub fn peer_public_key_base64(&self) -> String {
        self.conn
            .peer_public_key()
            .map(|pk| b64_encode(&pk))
            .unwrap_or_default()
    }

    pub fn verif_code(&self) -> String {
        self.conn.verif_code().unwrap_or_default()
    }

    /// Run the receive loop until the peer disconnects.
    pub fn serve(
        &mut self,
        config: &ServerConfig,
        callbacks: &mut ReceiveCallbacks<'_>,
    ) -> Result<()> {
        loop {
            let Some(message) = self.conn.recv_message()? else {
                return Ok(());
            };
            match message.kind.as_str() {
                "supported_message_types" => {
                    self.conn
                        .send_message(&Message::supported_message_types())?;
                }
                "device_info" => {
                    let info = message.device_info_of().unwrap_or(DeviceInfo {
                        name: String::new(),
                        device_type: String::new(),
                    });
                    self.peer_device = Some(info);
                    self.conn.send_message(&Message::device_info(
                        &config.local.name,
                        &config.local.device_type,
                    ))?;
                }
                "file_send_request" => {
                    let request = message
                        .file_send_request_of()
                        .ok_or_else(|| anyhow!("malformed file_send_request"))?;
                    if !self.handle_file_send_request(request, config, callbacks)? {
                        // The offer was declined: close now rather than lingering
                        // until the peer gives up, which would log a spurious error.
                        return Ok(());
                    }
                }
                "text_send" => {
                    let text = message.text_of().unwrap_or_default();
                    self.conn.send_message(&Message::ack(true))?;
                    (callbacks.text)(&text);
                }
                "ack" => {
                    // Nothing to do; the sender acknowledges our messages.
                }
                other => {
                    // Anything unrecognised is negatively acknowledged, as the
                    // protocol does.
                    let _ = other;
                    self.conn.send_message(&Message::ack(false))?;
                }
            }
        }
    }

    /// Handle one offer. Returns `false` if the transfer was declined, in which
    /// case the caller should close the connection.
    fn handle_file_send_request(
        &mut self,
        request: FileSendRequest,
        config: &ServerConfig,
        callbacks: &mut ReceiveCallbacks<'_>,
    ) -> Result<bool> {
        let total: u64 = request.files.iter().map(|f| f.size).sum();
        let peer = self.peer_device.clone().unwrap_or(DeviceInfo {
            name: String::new(),
            device_type: String::new(),
        });

        let context = OfferContext {
            peer_name: peer.name.clone(),
            peer_type: peer.device_type.clone(),
            peer_public_key: self.peer_public_key_base64(),
            verif_code: self.verif_code(),
            request: &request,
            total_size: total,
        };
        let accept = (callbacks.decide)(&context)?;
        if !accept {
            self.conn
                .send_message(&Message::file_send_request_reply(false))?;
            return Ok(false);
        }

        // Validate names and resolve destinations BEFORE telling the sender to
        // proceed. Replying "accept" first and only then discovering that a
        // filename is hostile would abort the transfer mid-stream instead of
        // declining it cleanly.
        let incoming = match prepare_incoming(&config.download_dir, &request.files) {
            Ok(incoming) => incoming,
            Err(e) => {
                let _ = self
                    .conn
                    .send_message(&Message::file_send_request_reply(false));
                return Err(e.context("refused the offer: unusable file names"));
            }
        };

        self.conn
            .send_message(&Message::file_send_request_reply(true))?;

        // Writing runs on its own thread so that decrypting the next chunk overlaps
        // with the previous one reaching the disk. Over 512 MiB on this machine,
        // deframing and decrypting costs 0.39s and writing costs 0.23s, and run one
        // after the other they are the entire receive path.
        let writer = FileWriter::start()?;

        let mut transferred: u64 = 0;
        for entry in &incoming {
            writer.open(&entry.target)?;
            let mut remaining = entry.descriptor.size;

            while remaining > 0 {
                let Some(chunk) = self.conn.recv_bytes()? else {
                    bail!("the sender disconnected after {transferred} of {total} bytes");
                };
                if chunk.len() as u64 > remaining {
                    bail!(
                        "the sender sent more data than declared for {:?}",
                        entry.descriptor.filename
                    );
                }
                remaining -= chunk.len() as u64;
                transferred += chunk.len() as u64;
                writer.data(chunk)?;
                (callbacks.progress)(transferred, total);
            }

            writer.finish(&entry.target, &entry.descriptor)?;
            (callbacks.file_done)(entry);
        }

        // Ends the writer and waits for it, so a successful return means everything
        // it accepted has been written.
        drop(writer);
        Ok(true)
    }
}

/// How many chunks may sit between the receiving loop and the disk.
///
/// Small on purpose: this is lookahead to cover the write, not a buffer to absorb a
/// transfer. Without a bound, a receiver on a fast link and a slow disk would queue
/// the sender's whole file in memory.
const WRITE_QUEUE_DEPTH: usize = 4;

/// A thread that owns the output file, fed by the receiving loop.
///
/// `write_all` to a file is a copy into the page cache — CPU work, not waiting — so
/// handing it to another core genuinely overlaps rather than just queueing. The
/// bytes, their order and the resulting metadata are all unchanged; only the thread
/// that performs the copy differs.
struct FileWriter {
    /// `None` once shut down. Dropping it ends the worker's loop.
    jobs: Option<SyncSender<WriteJob>>,
    /// The first failure. The worker stops consuming when it records one, and its
    /// next `send` failure is how the receiving loop finds out.
    failure: Arc<Mutex<Option<anyhow::Error>>>,
    worker: Option<JoinHandle<()>>,
}

enum WriteJob {
    /// Create the file the following `Data` belongs to.
    Open {
        target: PathBuf,
    },
    Data(Vec<u8>),
    /// Flush, close, stamp the metadata, then report back.
    Finish {
        target: PathBuf,
        descriptor: FileDescriptor,
        done: SyncSender<()>,
    },
}

impl FileWriter {
    fn start() -> Result<Self> {
        let (jobs, queue) = sync_channel::<WriteJob>(WRITE_QUEUE_DEPTH);
        let failure: Arc<Mutex<Option<anyhow::Error>>> = Arc::new(Mutex::new(None));
        let recorded = Arc::clone(&failure);

        let worker = std::thread::Builder::new()
            .name("landrop-writer".to_string())
            .spawn(move || {
                let mut open: Option<(PathBuf, File)> = None;
                while let Ok(job) = queue.recv() {
                    if let Err(error) = write_job(&mut open, job) {
                        // Stop consuming. Dropping `queue` makes the next send fail,
                        // which is how the receiving loop learns to stop reading.
                        *recorded.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
                        return;
                    }
                }
            })
            .context("spawning the file writer")?;

        Ok(Self {
            jobs: Some(jobs),
            failure,
            worker: Some(worker),
        })
    }

    fn open(&self, target: &Path) -> Result<()> {
        self.send(WriteJob::Open {
            target: target.to_path_buf(),
        })
    }

    fn data(&self, chunk: Vec<u8>) -> Result<()> {
        self.send(WriteJob::Data(chunk))
    }

    /// Wait until the worker has closed and stamped this file.
    fn finish(&self, target: &Path, descriptor: &FileDescriptor) -> Result<()> {
        let (done, wait) = sync_channel::<()>(1);
        self.send(WriteJob::Finish {
            target: target.to_path_buf(),
            descriptor: descriptor.clone(),
            done,
        })?;
        // A dropped sender means the worker stopped; `check` reports why.
        let _ = wait.recv();
        self.check()
    }

    fn send(&self, job: WriteJob) -> Result<()> {
        let Some(jobs) = self.jobs.as_ref() else {
            return Err(self.take_failure());
        };
        jobs.send(job).map_err(|_| self.take_failure())
    }

    fn check(&self) -> Result<()> {
        if self
            .failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
        {
            return Err(self.take_failure());
        }
        Ok(())
    }

    fn take_failure(&self) -> anyhow::Error {
        self.failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .unwrap_or_else(|| anyhow!("the file writer stopped before it was finished with"))
    }
}

impl Drop for FileWriter {
    fn drop(&mut self) {
        // Ends the worker's loop; it drains what it already accepted.
        self.jobs.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn write_job(open: &mut Option<(PathBuf, File)>, job: WriteJob) -> Result<()> {
    match job {
        WriteJob::Open { target } => {
            let handle = File::create(&target)
                .with_context(|| format!("cannot create {}", target.display()))?;
            *open = Some((target, handle));
        }
        WriteJob::Data(chunk) => {
            let Some((target, handle)) = open.as_mut() else {
                bail!("the receiving loop wrote data before opening a file");
            };
            handle
                .write_all(&chunk)
                .with_context(|| format!("writing {}", target.display()))?;
        }
        WriteJob::Finish {
            target,
            descriptor,
            done,
        } => {
            if let Some((_, mut handle)) = open.take() {
                handle
                    .flush()
                    .with_context(|| format!("flushing {}", target.display()))?;
            }
            // After the handle is dropped, so the timestamp is not overwritten by
            // the close that follows it.
            apply_metadata(&target, &descriptor)?;
            let _ = done.send(());
        }
    }
    Ok(())
}

/// The capability list we advertise, for callers that want to log it.
pub fn supported_message_types() -> Vec<String> {
    SUPPORTED_MESSAGE_TYPES
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::Identity;
    use crate::discovery::DiscoveredDevice;
    use std::net::TcpListener;

    /// Drive a real client/server pair over loopback TCP.
    fn loopback_pair() -> (ClientSession, ServerSession) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server_identity = Identity::generate();
        let client_identity = Identity::generate();

        let server_thread = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            ServerSession::accept(stream, &server_identity, Duration::from_secs(5)).unwrap()
        });

        let client =
            ClientSession::connect(addr, &client_identity, None, Duration::from_secs(5)).unwrap();
        let server = server_thread.join().unwrap();
        (client, server)
    }

    #[test]
    fn handshake_agrees_on_keys_and_verification_code() {
        let (client, server) = loopback_pair();
        assert_eq!(client.verif_code(), server.verif_code());
        assert_eq!(client.verif_code().len(), 6);
        // The identities must be mutually visible and distinct.
        assert_ne!(
            client.peer_public_key_base64(),
            server.peer_public_key_base64()
        );
    }

    #[test]
    fn client_rejects_a_mismatched_expected_public_key() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server_identity = Identity::generate();
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let _ = ServerSession::accept(stream, &server_identity, Duration::from_secs(5));
            }
        });
        let client_identity = Identity::generate();
        let result = ClientSession::connect(
            addr,
            &client_identity,
            Some("AkxBTkRST1AtRVhBTVBMRS1LRVktT05FLi4uLi4uLi4u"),
            Duration::from_secs(5),
        );
        assert!(result.is_err(), "a wrong expected key must be rejected");
    }

    #[test]
    fn device_info_exchange_both_ways() {
        let (mut client, mut server) = loopback_pair();

        let server_thread = std::thread::spawn(move || {
            let mut callbacks = ReceiveCallbacks {
                decide: &mut |_| Ok(false),
                progress: &mut |_, _| {},
                text: &mut |_| {},
                file_done: &mut |_| {},
            };
            let config = ServerConfig {
                local: LocalDevice {
                    name: "server-box".to_string(),
                    device_type: "linux".to_string(),
                },
                download_dir: PathBuf::from("."),
            };
            // The client only sends the capability + info exchange before closing.
            let _ = server.serve(&config, &mut callbacks);
        });

        let types = client.exchange_supported_message_types().unwrap();
        assert!(types.contains(&"file_send_request".to_string()));

        let info = client
            .exchange_device_info(&LocalDevice {
                name: "cli".to_string(),
                device_type: "windows".to_string(),
            })
            .unwrap();
        assert_eq!(info.name, "server-box");
        assert_eq!(info.device_type, "linux");

        drop(client);
        server_thread.join().unwrap();
    }

    #[test]
    fn full_file_transfer_over_loopback() {
        let (mut client, mut server) = loopback_pair();

        // Prepare a payload in a temp directory.
        let source_dir = tempfile::tempdir().unwrap();
        let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 253) as u8).collect();
        let source = source_dir.path().join("payload.bin");
        std::fs::write(&source, &payload).unwrap();

        let dest_dir = tempfile::tempdir().unwrap();
        let dest_path = dest_dir.path().to_path_buf();

        let server_thread = std::thread::spawn(move || {
            let mut accepted = false;
            let mut last_progress = 0u64;
            let mut finished: Vec<PathBuf> = Vec::new();
            let mut callbacks = ReceiveCallbacks {
                decide: &mut |ctx| {
                    accepted = true;
                    assert!(ctx.request.files.len() == 1);
                    Ok(true)
                },
                progress: &mut |done, _total| last_progress = done,
                text: &mut |_| {},
                file_done: &mut |entry| finished.push(entry.target.clone()),
            };
            let config = ServerConfig {
                local: LocalDevice {
                    name: "server-box".to_string(),
                    device_type: "linux".to_string(),
                },
                download_dir: dest_path.clone(),
            };
            let _ = server.serve(&config, &mut callbacks);
            (accepted, last_progress, finished)
        });

        client.exchange_supported_message_types().unwrap();
        client
            .exchange_device_info(&LocalDevice {
                name: "cli".to_string(),
                device_type: "windows".to_string(),
            })
            .unwrap();

        let leaves = crate::transfer::parse_paths(&[source]).unwrap();
        let descriptors: Vec<_> = leaves
            .iter()
            .map(|leaf| crate::transfer::describe(leaf).unwrap())
            .collect();

        let mut progress_calls = 0;
        let sent = client
            .send_files(&leaves, &descriptors, &mut |_, _| progress_calls += 1)
            .unwrap();
        assert_eq!(sent, payload.len() as u64);
        assert!(progress_calls > 0);

        drop(client);
        let (accepted, last_progress, finished) = server_thread.join().unwrap();
        assert!(accepted);
        assert_eq!(last_progress, payload.len() as u64);
        assert_eq!(finished.len(), 1);

        let received = std::fs::read(&finished[0]).unwrap();
        assert_eq!(received.len(), payload.len());
        assert_eq!(received, payload, "received bytes must match exactly");
    }

    #[test]
    fn declining_a_transfer_aborts_it() {
        let (mut client, mut server) = loopback_pair();

        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("nope.bin");
        std::fs::write(&source, vec![7u8; 1024]).unwrap();

        let server_thread = std::thread::spawn(move || {
            let mut callbacks = ReceiveCallbacks {
                decide: &mut |_| Ok(false),
                progress: &mut |_, _| {},
                text: &mut |_| {},
                file_done: &mut |_| {},
            };
            let config = ServerConfig {
                local: LocalDevice {
                    name: "server-box".to_string(),
                    device_type: "linux".to_string(),
                },
                download_dir: PathBuf::from("."),
            };
            let _ = server.serve(&config, &mut callbacks);
        });

        client.exchange_supported_message_types().unwrap();
        client
            .exchange_device_info(&LocalDevice {
                name: "cli".to_string(),
                device_type: "windows".to_string(),
            })
            .unwrap();

        let leaves = crate::transfer::parse_paths(&[source]).unwrap();
        let descriptors: Vec<_> = leaves
            .iter()
            .map(|leaf| crate::transfer::describe(leaf).unwrap())
            .collect();

        let error = client
            .send_files(&leaves, &descriptors, &mut |_, _| {})
            .unwrap_err();
        assert!(
            error.to_string().contains("declined"),
            "unexpected error: {error}"
        );
        drop(client);
        server_thread.join().unwrap();
    }

    #[test]
    fn zero_length_file_is_created_empty() {
        let (mut client, mut server) = loopback_pair();

        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("empty.txt");
        std::fs::write(&source, b"").unwrap();

        let dest_dir = tempfile::tempdir().unwrap();
        let dest_path = dest_dir.path().to_path_buf();

        let server_thread = std::thread::spawn(move || {
            let mut finished: Vec<PathBuf> = Vec::new();
            let mut callbacks = ReceiveCallbacks {
                decide: &mut |_| Ok(true),
                progress: &mut |_, _| {},
                text: &mut |_| {},
                file_done: &mut |entry| finished.push(entry.target.clone()),
            };
            let config = ServerConfig {
                local: LocalDevice {
                    name: "server-box".to_string(),
                    device_type: "linux".to_string(),
                },
                download_dir: dest_path.clone(),
            };
            let _ = server.serve(&config, &mut callbacks);
            finished
        });

        client.exchange_supported_message_types().unwrap();
        client
            .exchange_device_info(&LocalDevice {
                name: "cli".to_string(),
                device_type: "windows".to_string(),
            })
            .unwrap();

        let leaves = crate::transfer::parse_paths(&[source]).unwrap();
        let descriptors: Vec<_> = leaves
            .iter()
            .map(|leaf| crate::transfer::describe(leaf).unwrap())
            .collect();

        // The sender skips empty files entirely, so no bytes flow.
        let sent = client
            .send_files(&leaves, &descriptors, &mut |_, _| {})
            .unwrap();
        assert_eq!(sent, 0);
        drop(client);

        let finished = server_thread.join().unwrap();
        assert_eq!(finished.len(), 1, "the empty file must still be created");
        assert_eq!(std::fs::read(&finished[0]).unwrap().len(), 0);
    }

    #[test]
    fn text_message_round_trip() {
        let (mut client, mut server) = loopback_pair();

        let server_thread = std::thread::spawn(move || {
            let mut received = String::new();
            let mut callbacks = ReceiveCallbacks {
                decide: &mut |_| Ok(false),
                progress: &mut |_, _| {},
                text: &mut |t| received = t.to_string(),
                file_done: &mut |_| {},
            };
            let config = ServerConfig {
                local: LocalDevice {
                    name: "server-box".to_string(),
                    device_type: "linux".to_string(),
                },
                download_dir: PathBuf::from("."),
            };
            let _ = server.serve(&config, &mut callbacks);
            received
        });

        client.exchange_supported_message_types().unwrap();
        client.send_text("hello from the cli").unwrap();
        drop(client);
        assert_eq!(server_thread.join().unwrap(), "hello from the cli");
    }

    #[test]
    fn discovered_device_helper_is_usable() {
        let device = DiscoveredDevice {
            name: "d".to_string(),
            device_type: "linux".to_string(),
            address: "1.2.3.4".to_string(),
            port: 1,
            public_key: "K".to_string(),
            discoverable: true,
        };
        assert!(device.label().contains("1.2.3.4:1"));
    }

    /// A receiver must refuse a hostile filename with `accept: false` *before*
    /// any data flows, not accept and then abort mid-stream.
    #[test]
    fn hostile_filename_is_declined_before_any_data() {
        let (mut client, mut server) = loopback_pair();
        let dest = tempfile::tempdir().unwrap();
        let dest_path = dest.path().to_path_buf();
        let outside = dest.path().parent().unwrap().join("escaped-by-test.txt");

        let server_thread = std::thread::spawn(move || {
            let mut callbacks = ReceiveCallbacks {
                decide: &mut |_| Ok(true), // the user says yes...
                progress: &mut |_, _| {},
                text: &mut |_| {},
                file_done: &mut |_| {},
            };
            let config = ServerConfig {
                local: LocalDevice {
                    name: "server-box".to_string(),
                    device_type: "linux".to_string(),
                },
                download_dir: dest_path,
            };
            server.serve(&config, &mut callbacks).is_err()
        });

        client.exchange_supported_message_types().unwrap();
        client
            .exchange_device_info(&LocalDevice {
                name: "cli".to_string(),
                device_type: "windows".to_string(),
            })
            .unwrap();

        // Hand-craft an offer whose filename tries to escape the download dir.
        let files = vec![FileDescriptor {
            filename: "../escaped-by-test.txt".to_string(),
            size: 4,
            last_modified: 0,
            permissions: "644".to_string(),
        }];
        client
            .conn
            .send_message(&Message::file_send_request(&files))
            .unwrap();

        let reply = client.conn.recv_message().unwrap().unwrap();
        assert_eq!(reply.kind, "file_send_request_reply");
        assert!(
            !reply.file_send_request_reply_of().unwrap().accept,
            "a traversal filename must be refused, not accepted"
        );

        drop(client);
        assert!(
            server_thread.join().unwrap(),
            "the server should report the refusal as an error"
        );
        assert!(
            !outside.exists(),
            "nothing may be written outside the download directory"
        );
    }

    /// The declared size is authoritative, so a file that shrinks mid-transfer must
    /// fail loudly rather than leave the receiver blocked forever.
    #[test]
    fn sender_detects_a_file_that_shrank() {
        let (mut client, mut server) = loopback_pair();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("shrinking.bin");
        std::fs::write(&source, vec![1u8; 200_000]).unwrap();

        // Describe while the file is still large.
        let leaves = crate::transfer::parse_paths(std::slice::from_ref(&source)).unwrap();
        let descriptors: Vec<_> = leaves
            .iter()
            .map(|leaf| crate::transfer::describe(leaf).unwrap())
            .collect();
        assert_eq!(descriptors[0].size, 200_000);

        // The receiver accepts and then waits for 200 000 bytes.
        let dest = tempfile::tempdir().unwrap();
        let dest_path = dest.path().to_path_buf();
        let server_thread = std::thread::spawn(move || {
            let mut callbacks = ReceiveCallbacks {
                decide: &mut |_| Ok(true),
                progress: &mut |_, _| {},
                text: &mut |_| {},
                file_done: &mut |_| {},
            };
            let config = ServerConfig {
                local: LocalDevice {
                    name: "server-box".to_string(),
                    device_type: "linux".to_string(),
                },
                download_dir: dest_path,
            };
            server.serve(&config, &mut callbacks).is_err()
        });

        client.exchange_supported_message_types().unwrap();
        client
            .exchange_device_info(&LocalDevice {
                name: "cli".to_string(),
                device_type: "windows".to_string(),
            })
            .unwrap();

        // Truncate after the file was described but before any byte is read.
        std::fs::write(&source, vec![1u8; 1000]).unwrap();

        let error = client
            .send_files(&leaves, &descriptors, &mut |_, _| {})
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("shrank"),
            "expected a shrink error, got: {error:#}"
        );

        drop(client);
        assert!(
            server_thread.join().unwrap(),
            "the short stream must surface as an error on the receiver too"
        );
    }

    /// A file that grows is harmless: we send exactly the declared prefix.
    #[test]
    fn sender_truncates_a_file_that_grew() {
        let (mut client, mut server) = loopback_pair();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("growing.bin");
        std::fs::write(&source, vec![1u8; 100_000]).unwrap();

        let leaves = crate::transfer::parse_paths(std::slice::from_ref(&source)).unwrap();
        let descriptors: Vec<_> = leaves
            .iter()
            .map(|leaf| crate::transfer::describe(leaf).unwrap())
            .collect();
        assert_eq!(descriptors[0].size, 100_000);

        // Grow the file after it was described: the declared 100 000 bytes are
        // still the first 100 000 bytes, which are now all 9s.
        std::fs::write(&source, vec![9u8; 300_000]).unwrap();

        let dest = tempfile::tempdir().unwrap();
        let dest_path = dest.path().to_path_buf();
        let server_thread = std::thread::spawn(move || {
            let mut written: Vec<PathBuf> = Vec::new();
            let mut callbacks = ReceiveCallbacks {
                decide: &mut |_| Ok(true),
                progress: &mut |_, _| {},
                text: &mut |_| {},
                file_done: &mut |entry| written.push(entry.target.clone()),
            };
            let config = ServerConfig {
                local: LocalDevice {
                    name: "server-box".to_string(),
                    device_type: "linux".to_string(),
                },
                download_dir: dest_path,
            };
            server.serve(&config, &mut callbacks).unwrap();
            written
        });

        client.exchange_supported_message_types().unwrap();
        client
            .exchange_device_info(&LocalDevice {
                name: "cli".to_string(),
                device_type: "windows".to_string(),
            })
            .unwrap();

        let sent = client
            .send_files(&leaves, &descriptors, &mut |_, _| {})
            .unwrap();
        assert_eq!(sent, 100_000, "only the declared size may be sent");
        drop(client);

        let written = server_thread.join().unwrap();
        assert_eq!(written.len(), 1);
        let received = std::fs::read(&written[0]).unwrap();
        assert_eq!(
            received.len(),
            100_000,
            "the receiver must get exactly the declared size, not the grown file"
        );
        assert!(
            received.iter().all(|b| *b == 9),
            "the declared prefix of the current contents must be what arrives"
        );
    }

    /// Feed `raw` to a server handshake and return what it makes of it. A
    /// malformed peer must produce a clean error, never a panic or a hang.
    fn handshake_against(raw: &[u8]) -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let identity = Identity::generate();
        let payload = raw.to_vec();

        let server_thread = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            ServerSession::accept(stream, &identity, Duration::from_secs(5)).map(|_| ())
        });

        let mut client = std::net::TcpStream::connect(addr).unwrap();
        client.write_all(&payload).unwrap();
        client.flush().unwrap();
        // Half-close so a deliberately short payload reaches the server as EOF
        // instead of blocking until the read timeout.
        let _ = client.shutdown(std::net::Shutdown::Write);

        let result = server_thread
            .join()
            .expect("the server thread must not panic");
        drop(client);
        result
    }

    fn framed(value: &serde_json::Value) -> Vec<u8> {
        let body = serde_json::to_vec(value).unwrap();
        let mut frame = Vec::new();
        frame.extend_from_slice(&(body.len() as u16).to_be_bytes());
        frame.extend_from_slice(&body);
        frame
    }

    #[test]
    fn garbage_hello_is_rejected() {
        let error = handshake_against(b"\x00\x05hello").unwrap_err();
        assert!(
            format!("{error:#}").contains("ClientHello"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn empty_json_hello_is_rejected() {
        let error = handshake_against(b"\x00\x02{}").unwrap_err();
        assert!(
            format!("{error:#}").contains("ClientHello"),
            "a hello without required fields must fail cleanly: {error:#}"
        );
    }

    #[test]
    fn truncated_frame_is_rejected() {
        // Claims 100 bytes, delivers 3, then closes.
        let error = handshake_against(b"\x00\x64\x01\x02\x03").unwrap_err();
        let text = format!("{error:#}").to_lowercase();
        assert!(
            text.contains("mid-frame") || text.contains("closed"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn oversized_frame_header_is_rejected() {
        // Maximum length field with no payload at all.
        let error = handshake_against(b"\xff\xff").unwrap_err();
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn hello_with_a_bogus_signature_is_rejected() {
        let identity = Identity::generate();
        let ephemeral = EphemeralKey::generate();
        let hello = serde_json::json!({
            "supported_versions": ["v2"],
            "public_key": identity.pk_base64(),
            "ecdhe": {
                "public_key": b64_encode(&ephemeral.public()),
                // Well-formed length, but not a signature over the ephemeral key.
                "sig": b64_encode(&[0u8; 64]),
            }
        });
        let error = handshake_against(&framed(&hello)).unwrap_err();
        assert!(
            format!("{error:#}").contains("signature"),
            "an unverifiable signature must be refused: {error:#}"
        );
    }

    #[test]
    fn hello_offering_only_v1_is_rejected() {
        let identity = Identity::generate();
        let ephemeral = EphemeralKey::generate();
        let hello = serde_json::json!({
            "supported_versions": ["v1"],
            "public_key": identity.pk_base64(),
            "ecdhe": {
                "public_key": b64_encode(&ephemeral.public()),
                "sig": b64_encode(&identity.sign(&ephemeral.public())),
            }
        });
        let error = handshake_against(&framed(&hello)).unwrap_err();
        assert!(
            format!("{error:#}").contains("v2"),
            "unexpected error: {error:#}"
        );
    }
}
