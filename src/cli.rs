// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! The command line interface: argument parsing, terminal output, target
//! resolution and every command implementation.
//!
//! This lives in the library rather than the binary so integration tests can
//! reach it; `src/main.rs` is only an entry point.

use crate::config::{Settings, device_type, load_or_create_identity};
use crate::crypto::Identity;
use crate::discovery::{self, DiscoverOptions, DiscoveredDevice};
use crate::messages::FileDescriptor;
use crate::session::{
    ClientSession, LocalDevice, OfferContext, ReceiveCallbacks, ServerConfig, ServerSession,
};
use crate::transfer::{self, FileLeaf};
use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand};
use dialoguer::{Confirm, Select};
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::json;
use std::fs;
use std::io::{BufRead, IsTerminal};
use std::net::{SocketAddr, TcpListener, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Argument definitions
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(
    name = "landrop-cli",
    version,
    about = "Discover LANDrop v2 devices and transfer files from the terminal",
    long_about = "A terminal client for the LANDrop v2 LAN protocol.\n\n\
                  It speaks the same wire protocol as the LANDrop v2 apps, so no \
                  device on your network needs to be changed or reinstalled."
)]
struct Cli {
    /// Emit machine-readable JSON on stdout (implies non-interactive).
    #[arg(long, global = true)]
    json: bool,

    /// Background/service mode: no banner and no progress bars, but keep logs.
    #[arg(long, short = 'q', global = true)]
    quiet: bool,

    /// Use an alternate configuration directory (identity, settings, cache).
    #[arg(long, global = true, value_name = "DIR")]
    config_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Find `LANDrop` devices on the local network.
    Discover(DiscoverArgs),
    /// Send files and/or directories to a device.
    Send(SendArgs),
    /// Receive files: run as a server other devices can send to.
    Receive(ReceiveArgs),
    /// Send a short text message to a device.
    Text(TextArgs),
    /// List, look up or forget devices remembered from earlier runs.
    Devices(DevicesArgs),
    /// Manage the senders this CLI accepts from without asking.
    Trusted(TrustedArgs),
    /// Show this CLI's identity, configuration and paths.
    Identity(IdentityArgs),
    /// Verify the implementation end to end over loopback, with no network peer.
    Selftest(SelftestArgs),
}

#[derive(Args, Debug)]
struct DiscoverArgs {
    /// Seconds to listen for replies.
    #[arg(long, default_value_t = 3)]
    timeout: u64,
    /// Include the loopback interface (useful for same-host testing).
    #[arg(long)]
    loopback: bool,
    /// Discovery UDP port (defaults to the protocol's 52637).
    #[arg(long, default_value_t = discovery::DISCOVERY_PORT)]
    discovery_port: u16,
}

#[derive(Args, Debug)]
struct SendArgs {
    /// Files and/or directories to send.
    files: Vec<PathBuf>,

    /// Target: a device name, a public key, or host:port.
    #[arg(long, short = 't')]
    to: Option<String>,

    /// Read newline-separated paths from stdin instead of the argument list.
    #[arg(long)]
    stdin: bool,

    /// Skip the interactive picker and use the first device found.
    #[arg(long)]
    first: bool,

    /// Seconds to wait for the recipient to accept the transfer.
    #[arg(long, default_value_t = 300)]
    reply_timeout: u64,

    /// Do not ask for confirmation before sending.
    #[arg(long, short = 'y')]
    yes: bool,

    /// After sending, wait for the recipient to close the connection.
    ///
    /// The protocol has no completion acknowledgement, so a send normally returns
    /// as soon as the bytes are handed to the OS — which can be well before the
    /// recipient has written them. This waits for the receiver to hang up, the
    /// closest thing to "the files are on their disk". Best effort: nothing
    /// obliges a receiver to close, so it also makes the command slower.
    #[arg(long)]
    wait_for_close: bool,
}

#[derive(Args, Debug)]
struct ReceiveArgs {
    /// TCP port to listen on. Without this the last port passed here is reused;
    /// `--port 0` picks a free port each run and forgets the remembered one.
    #[arg(long)]
    port: Option<u16>,

    /// Directory to save received files into.
    #[arg(long)]
    dir: Option<PathBuf>,

    /// Advertised device name.
    #[arg(long)]
    name: Option<String>,

    /// Accept every transfer without asking.
    #[arg(long, short = 'y')]
    yes: bool,

    /// Discovery UDP port (defaults to the protocol's 52637).
    #[arg(long, default_value_t = discovery::DISCOVERY_PORT)]
    discovery_port: u16,

    /// Exit after a single transfer.
    #[arg(long)]
    once: bool,

    /// Exit after this many transfers (0 means unlimited).
    #[arg(long, default_value_t = 0)]
    max: usize,
}

#[derive(Args, Debug)]
struct TextArgs {
    /// The text to send.
    text: String,

    /// Target: a device name, a public key, or host:port.
    #[arg(long, short = 't')]
    to: Option<String>,

    /// Skip the interactive picker and use the first device found.
    #[arg(long)]
    first: bool,
}

#[derive(Args, Debug)]
struct DevicesArgs {
    /// Remove a remembered device by name or public key.
    #[arg(long)]
    forget: Option<String>,
}

#[derive(Args, Debug)]
struct TrustedArgs {
    /// Trust this sender (a device name, or a public key).
    #[arg(long, value_name = "NAME|KEY")]
    add: Option<String>,

    /// Stop trusting this sender.
    #[arg(long, value_name = "NAME|KEY")]
    remove: Option<String>,

    /// Rename a trusted sender. The key, and so the trust itself, is unchanged.
    #[arg(long, value_name = "NAME|KEY", requires = "name")]
    edit: Option<String>,

    /// The new name for --edit.
    #[arg(long, value_name = "NEW")]
    name: Option<String>,
}

#[derive(Args, Debug)]
struct IdentityArgs {
    /// Also print the secret key (keep this private).
    #[arg(long)]
    show_secret: bool,
}

#[derive(Args, Debug)]
struct SelftestArgs {
    /// Payload size in bytes.
    #[arg(long, default_value_t = 5 * 1024 * 1024)]
    size: u64,
}

// ---------------------------------------------------------------------------
// Output helpers
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Ui {
    json: bool,
    /// Service mode: suppress the banner and progress bars, keep transfer logs.
    quiet: bool,
}

impl Ui {
    /// Interactive only when we are not in JSON mode and have a real terminal.
    fn interactive(&self) -> bool {
        !self.json && !self.quiet && std::io::stdin().is_terminal()
    }

    /// Progress and commentary. Never pollutes stdout in JSON mode.
    fn note(&self, message: impl AsRef<str>) {
        if self.json {
            eprintln!("{}", message.as_ref());
        } else {
            println!("{}", message.as_ref());
        }
    }

    /// Startup banner: only shown when not running as a background service.
    fn banner(&self, message: impl AsRef<str>) {
        if !self.quiet {
            self.note(message);
        }
    }

    fn warn(&self, message: impl AsRef<str>) {
        eprintln!("warning: {}", message.as_ref());
    }

    /// Progress bars would spam a service log with ANSI escapes.
    fn progress_bar(&self, total: u64, label: &str) -> ProgressBar {
        if self.quiet {
            ProgressBar::hidden()
        } else {
            progress_bar(total, label)
        }
    }

    /// The final machine-readable payload, emitted only in JSON mode.
    fn finish(&self, value: serde_json::Value) {
        if self.json {
            match serde_json::to_string_pretty(&value) {
                Ok(text) => println!("{text}"),
                Err(e) => eprintln!("error: could not serialise output: {e}"),
            }
        }
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

fn progress_bar(total: u64, label: &str) -> ProgressBar {
    if total == 0 {
        return ProgressBar::hidden();
    }
    let bar = ProgressBar::new(total);
    if let Ok(style) = ProgressStyle::with_template("{wide_bar} {msg}") {
        bar.set_style(style.progress_chars("=> "));
    }
    bar.set_message(format!("{label} {}", human_bytes(0)));
    bar
}

// ---------------------------------------------------------------------------
// Target resolution
// ---------------------------------------------------------------------------

/// Where a target's address came from, which decides whether it may be
/// re-resolved when it stops answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetSource {
    /// An explicit `host:port` from the user. Authoritative: never re-resolved.
    Direct,
    /// Remembered from an earlier discovery. Only a hint — see `connect_target`.
    Remembered,
    /// Answered discovery just now.
    Discovered,
}

#[derive(Debug, Clone)]
struct Target {
    name: String,
    device_type: String,
    address: String,
    port: u16,
    public_key: Option<String>,
    source: TargetSource,
}

impl Target {
    fn label(&self) -> String {
        format!(
            "{} [{}] {}:{}",
            if self.name.is_empty() {
                "(unnamed)"
            } else {
                &self.name
            },
            if self.device_type.is_empty() {
                "unknown"
            } else {
                &self.device_type
            },
            self.address,
            self.port
        )
    }

    fn to_json(&self) -> serde_json::Value {
        json!({
            "name": self.name,
            "type": self.device_type,
            "address": self.address,
            "port": self.port,
            "public_key": self.public_key,
        })
    }

    fn socket_addr(&self) -> Result<SocketAddr> {
        let mut addrs = (self.address.as_str(), self.port)
            .to_socket_addrs()
            .with_context(|| format!("cannot resolve {}:{}", self.address, self.port))?;
        addrs
            .next()
            .ok_or_else(|| anyhow!("no address resolved for {}:{}", self.address, self.port))
    }
}

fn from_discovered(device: &DiscoveredDevice) -> Target {
    Target {
        name: device.name.clone(),
        device_type: device.device_type.clone(),
        address: device.address.clone(),
        port: device.port,
        public_key: Some(device.public_key.clone()),
        source: TargetSource::Discovered,
    }
}

/// Parse `host:port` (or `[v6]:port`) into a direct target.
fn parse_direct(query: &str) -> Option<Target> {
    let (host, port) = if let Some(rest) = query.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        let port = tail.strip_prefix(':')?;
        (host.to_string(), port.parse::<u16>().ok()?)
    } else {
        let (host, port) = query.rsplit_once(':')?;
        if host.contains(':') {
            return None; // bare IPv6 without brackets
        }
        (host.to_string(), port.parse::<u16>().ok()?)
    };
    if host.is_empty() || port == 0 {
        return None;
    }
    Some(Target {
        name: String::new(),
        device_type: String::new(),
        address: host,
        port,
        public_key: None,
        source: TargetSource::Direct,
    })
}

fn run_discovery(
    identity: &Identity,
    timeout: Duration,
    loopback: bool,
    discovery_port: u16,
) -> Result<Vec<DiscoveredDevice>> {
    let mut options = DiscoverOptions::new(identity.pk_base64());
    options.timeout = timeout;
    options.include_loopback = loopback;
    options.discovery_port = discovery_port;
    discovery::discover(&options)
}

/// Resolve a user-supplied query to something we can connect to.
fn resolve_query(
    query: &str,
    identity: &Identity,
    settings: &mut Settings,
    ui: &Ui,
    timeout: Duration,
) -> Result<Target> {
    // 1. An explicit host:port bypasses discovery entirely and, like the desktop
    //    app's "add device by IP and port", does not pin the peer identity.
    if let Some(target) = parse_direct(query) {
        return Ok(target);
    }

    // 2. A device we have seen before. Its address is only a hint: the peer picks
    //    a fresh listening port every run, so a remembered one goes stale the
    //    moment the peer restarts. `connect_target` re-resolves it in that case.
    if let Some(known) = settings.lookup(query)
        && !known.last_address.is_empty()
        && known.last_port != 0
    {
        return Ok(Target {
            name: known.name,
            device_type: known.device_type,
            address: known.last_address,
            port: known.last_port,
            public_key: Some(known.public_key),
            source: TargetSource::Remembered,
        });
    }

    // 3. Ask the network.
    discover_matching(query, identity, settings, ui, timeout)
}

/// Ask the network for the device matching `query`, ignoring what we remember,
/// and refresh the remembered address of everything that answered.
fn discover_matching(
    query: &str,
    identity: &Identity,
    settings: &mut Settings,
    ui: &Ui,
    timeout: Duration,
) -> Result<Target> {
    let devices = run_discovery(identity, timeout, false, discovery::DISCOVERY_PORT)?;
    let query_lower = query.to_ascii_lowercase();
    let Some(device) = devices
        .iter()
        .find(|d| d.public_key == query || d.name.to_ascii_lowercase() == query_lower)
    else {
        if devices.is_empty() {
            bail!("no device matched {query:?} and discovery found nothing on the local network");
        }
        let available: Vec<String> = devices.iter().map(DiscoveredDevice::label).collect();
        bail!(
            "no device matched {query:?}. Devices found:\n  {}",
            available.join("\n  ")
        );
    };

    for device in &devices {
        settings.remember(device);
    }
    // Failing to write the cache costs a re-discovery next time, so it must not
    // fail the transfer — but it must not vanish silently either.
    if let Err(error) = settings.save() {
        ui.note(format!("  could not update the device cache: {error}"));
    }
    Ok(from_discovered(device))
}

/// Connect to a target, re-resolving a remembered address once when it no
/// longer answers.
///
/// The peer chooses a fresh listening port on every run, so an address we
/// remembered from an earlier discovery is a hint rather than a fact. Retrying
/// once against a live discovery is what makes `--to <public-key>` survive a
/// peer restart — which is the whole reason to target by key instead of by
/// address. Targeting an explicit `host:port` is never re-resolved: the caller
/// pinned that address, and there is no identity to search for.
fn connect_target(
    target: &Target,
    identity: &Identity,
    settings: &mut Settings,
    ui: &Ui,
    discovery_timeout: Duration,
) -> Result<(ClientSession, Target)> {
    let attempt = |target: &Target| {
        ClientSession::connect(
            target.socket_addr()?,
            identity,
            target.public_key.as_deref(),
            Duration::from_secs(10),
        )
    };

    let first_error = match attempt(target) {
        Ok(client) => return Ok((client, target.clone())),
        Err(error) => error,
    };

    if target.source != TargetSource::Remembered {
        return Err(first_error);
    }
    // Without a key there is nothing to search for, so the old address stands.
    let Some(key) = target.public_key.as_deref() else {
        return Err(first_error);
    };

    ui.note(format!(
        "  {} did not answer at {}:{}; asking the network again ...",
        target.label(),
        target.address,
        target.port
    ));

    let fresh = match discover_matching(key, identity, settings, ui, discovery_timeout) {
        Ok(fresh) => fresh,
        Err(discovery_error) => {
            // Chain the discovery failure rather than interpolating it into the
            // message, which would flatten its own cause chain into a string.
            return Err(first_error.context(discovery_error).context(
                "the remembered address went stale, and re-discovery did not find the device again",
            ));
        }
    };

    ui.note(format!(
        "  found {} at {}:{}",
        fresh.name, fresh.address, fresh.port
    ));

    attempt(&fresh)
        .map(|client| (client, fresh))
        .map_err(|error| error.context("the re-discovered address did not answer either"))
}

/// Pick a device interactively when `--to` was not given.
fn choose_device(identity: &Identity, ui: &Ui, timeout: Duration, first: bool) -> Result<Target> {
    let devices = run_discovery(identity, timeout, false, discovery::DISCOVERY_PORT)?;
    if devices.is_empty() {
        bail!("no LANDrop devices answered discovery. Are they running and on the same network?");
    }
    if first || devices.len() == 1 || !ui.interactive() {
        if !first && devices.len() > 1 && !ui.interactive() {
            bail!(
                "{} devices found; specify one with --to <name|key|host:port>",
                devices.len()
            );
        }
        return Ok(from_discovered(&devices[0]));
    }
    let labels: Vec<String> = devices.iter().map(DiscoveredDevice::label).collect();
    let index = Select::new()
        .with_prompt("Select a device")
        .items(&labels)
        .default(0)
        .interact()
        .map_err(|e| anyhow!("device selection failed: {e}"))?;
    Ok(from_discovered(&devices[index]))
}

fn resolve_target(
    explicit: Option<&str>,
    identity: &Identity,
    settings: &mut Settings,
    ui: &Ui,
    timeout: Duration,
    first: bool,
) -> Result<Target> {
    match explicit {
        Some(query) => resolve_query(query, identity, settings, ui, timeout),
        None => choose_device(identity, ui, timeout, first),
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn cmd_discover(
    ui: &Ui,
    settings: &mut Settings,
    identity: &Identity,
    args: DiscoverArgs,
) -> Result<()> {
    ui.note(format!(
        "broadcasting on {}:{} ...",
        discovery::MULTICAST_GROUP,
        args.discovery_port
    ));
    let devices = run_discovery(
        identity,
        Duration::from_secs(args.timeout.max(1)),
        args.loopback,
        args.discovery_port,
    )?;
    for device in &devices {
        settings.remember(device);
    }
    let _ = settings.save();

    if devices.is_empty() {
        ui.note("no devices found.");
    } else {
        ui.note(format!("found {} device(s):", devices.len()));
        for device in &devices {
            ui.note(format!(
                "  {}  key={}",
                device.label(),
                &device.public_key[..device.public_key.len().min(12)]
            ));
        }
    }
    ui.finish(json!({
        "count": devices.len(),
        "devices": devices,
    }));
    Ok(())
}

/// Collect input paths from the argument list, `--stdin`, or a pipe.
fn collect_inputs(args: &SendArgs) -> Result<Vec<PathBuf>> {
    let mut inputs: Vec<PathBuf> = args.files.clone();

    let read_stdin = args.stdin || (inputs.is_empty() && !std::io::stdin().is_terminal());
    if read_stdin {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let line = line.context("reading paths from stdin")?;
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                inputs.push(PathBuf::from(trimmed));
            }
        }
    }

    inputs.dedup();
    if inputs.is_empty() {
        bail!("no files to send: pass paths as arguments, or use --stdin to pipe them in");
    }
    Ok(inputs)
}

fn cmd_send(ui: &Ui, settings: &mut Settings, identity: &Identity, args: SendArgs) -> Result<()> {
    let inputs = collect_inputs(&args)?;

    let leaves: Vec<FileLeaf> = transfer::parse_paths(&inputs)?;
    let descriptors: Vec<FileDescriptor> = leaves
        .iter()
        .map(transfer::describe)
        .collect::<Result<_>>()?;
    let total = transfer::total_size(&descriptors);

    ui.note(format!(
        "staging {} file(s), {} total",
        descriptors.len(),
        human_bytes(total)
    ));

    // Resolve every target before sending anything: a typo in the third of five
    // should not let the first two land and then abort the run.
    let targets = resolve_targets(
        args.to.as_deref(),
        identity,
        settings,
        ui,
        args.first,
        Duration::from_secs(3),
    )?;

    if !args.yes && ui.interactive() {
        for target in &targets {
            ui.note(format!("\nSend to: {}", target.label()));
        }
        for descriptor in &descriptors {
            ui.note(format!(
                "  {:<50} {:>12}",
                descriptor.filename,
                human_bytes(descriptor.size)
            ));
        }
        ui.note(format!("  {:<50} {:>12}", "total", human_bytes(total)));
        if targets.len() > 1 {
            ui.note(format!(
                "  each of the {} devices will ask its own person to accept",
                targets.len()
            ));
        }
        let confirmed = Confirm::new()
            .with_prompt("Proceed?")
            .default(true)
            .interact()
            .map_err(|e| anyhow!("confirmation failed: {e}"))?;
        if !confirmed {
            ui.note("aborted.");
            return Ok(());
        }
    }

    // One device is the common case and keeps the single-object result shape that
    // scripts already parse. Several devices accumulate instead.
    let single = targets.len() == 1;
    let mut delivered: Vec<serde_json::Value> = Vec::new();
    let mut refused: Vec<serde_json::Value> = Vec::new();

    for target in &targets {
        match send_to_target(
            ui,
            settings,
            identity,
            target,
            &leaves,
            &descriptors,
            total,
            args.reply_timeout,
            args.wait_for_close,
        ) {
            Ok(summary) => {
                if single {
                    ui.finish(summary.clone());
                }
                delivered.push(summary);
            }
            Err(error) => {
                let message = format!("{error:#}");
                // Keep going: one unreachable device must not strand the rest.
                ui.warn(format!("{}: {message}", target.label()));
                refused.push(json!({ "target": target.to_json(), "error": message }));
            }
        }
    }

    let _ = settings.save();

    if single {
        if let Some(failure) = refused.first() {
            bail!(
                "{}",
                failure["error"].as_str().unwrap_or("the transfer failed")
            );
        }
        return Ok(());
    }

    ui.note(format!(
        "\n{} of {} device(s) received the files",
        delivered.len(),
        targets.len()
    ));
    for entry in &delivered {
        ui.note(format!(
            "  ok    {}",
            entry["target"]["name"].as_str().unwrap_or("(unnamed)")
        ));
    }
    for entry in &refused {
        ui.note(format!(
            "  fail  {}",
            entry["target"]["name"].as_str().unwrap_or("(unnamed)")
        ));
    }

    ui.finish(json!({
        "status": if refused.is_empty() { "ok" } else { "error" },
        "delivered": delivered.len(),
        "failed": refused.len(),
        "targets": delivered.iter().chain(refused.iter()).collect::<Vec<_>>(),
    }));

    if !refused.is_empty() {
        bail!(
            "{} of {} device(s) did not receive the files",
            refused.len(),
            targets.len()
        );
    }
    Ok(())
}

/// How long `--wait-for-close` gives the recipient to hang up before giving up.
///
/// There is no acknowledgement to wait for, only the connection ending, so this
/// bounds a wait that has no defined completion.
const WAIT_FOR_CLOSE: Duration = Duration::from_secs(30);

/// Send the staged files to one resolved target.
#[allow(clippy::too_many_arguments)]
fn send_to_target(
    ui: &Ui,
    settings: &mut Settings,
    identity: &Identity,
    target: &Target,
    leaves: &[FileLeaf],
    descriptors: &[FileDescriptor],
    total: u64,
    reply_timeout: u64,
    wait_for_close: bool,
) -> Result<serde_json::Value> {
    ui.note(format!("\nconnecting to {} ...", target.label()));

    let (mut client, target) =
        connect_target(target, identity, settings, ui, Duration::from_secs(3))?;

    let verif_code = client.verif_code();
    client.set_reply_timeout(Duration::from_secs(reply_timeout.max(1)))?;
    client.exchange_supported_message_types()?;
    let local = LocalDevice {
        name: settings.device_name(),
        device_type: device_type().to_string(),
    };
    let peer = client.exchange_device_info(&local)?;

    ui.note(format!(
        "  connected to {} [{}]",
        peer.name, peer.device_type
    ));
    ui.note(format!("  verification code : {verif_code}"));
    ui.note(format!(
        "  if {} does not already trust this CLI, accept the prompt on that device",
        if peer.name.is_empty() {
            "the recipient"
        } else {
            &peer.name
        }
    ));

    let bar = ui.progress_bar(total, "sending");
    let started = Instant::now();
    let mut last_render = Instant::now();
    let mut sent_so_far = 0u64;

    let result = client.send_files(leaves, descriptors, &mut |sent, expected| {
        sent_so_far = sent;
        bar.set_position(sent);
        if last_render.elapsed() > Duration::from_millis(120) || sent == expected {
            last_render = Instant::now();
            let elapsed = started.elapsed().as_secs_f64();
            let rate = if elapsed > 0.0 {
                sent as f64 / elapsed
            } else {
                0.0
            };
            bar.set_message(format!(
                "{} / {}  ({} /s)",
                human_bytes(sent),
                human_bytes(expected),
                human_bytes(rate as u64)
            ));
        }
    });
    bar.finish_and_clear();

    let elapsed = started.elapsed().as_secs_f64();
    let sent = result.with_context(|| {
        if sent_so_far == 0 {
            format!(
                "no data reached {}. If the device is still showing an Accept prompt, \
                 someone has to confirm it there (waited up to {}s)",
                target.label(),
                reply_timeout
            )
        } else {
            format!("the transfer to {} was interrupted", target.label())
        }
    })?;
    let rate = if elapsed > 0.0 {
        sent as f64 / elapsed
    } else {
        0.0
    };

    ui.note(format!(
        "\nsent {} in {:.2}s ({}/s)",
        human_bytes(sent),
        elapsed,
        human_bytes(rate as u64)
    ));

    // Off by default: waiting costs time on every send, and a receiver is not
    // obliged to close at all, so this can only ever be a best-effort answer.
    let closed = if wait_for_close {
        let closed = client.wait_for_close(WAIT_FOR_CLOSE)?;
        if closed {
            ui.note("  the recipient closed the connection");
        } else {
            ui.note(format!(
                "  the recipient did not close within {WAIT_FOR_CLOSE:?}; the bytes were \
                 sent, but nothing confirms they were written"
            ));
        }
        Some(closed)
    } else {
        None
    };

    client.close();

    Ok(json!({
        "status": "ok",
        "target": target.to_json(),
        "verification_code": verif_code,
        "files": descriptors,
        "total_bytes": total,
        "sent_bytes": sent,
        "duration_seconds": elapsed,
        "throughput_bytes_per_second": rate as u64,
        // `null` when not asked for, `false` when the wait expired. Only `true`
        // means the recipient hung up, which is the only completion signal the
        // protocol offers.
        "peer_closed": closed,
    }))
}

/// Resolve `--to` into every device to send to.
///
/// Several comma-separated targets are accepted so one invocation can reach a
/// group; each is still a separate connection, because a transfer to one device
/// cannot carry another's acceptance.
fn resolve_targets(
    explicit: Option<&str>,
    identity: &Identity,
    settings: &mut Settings,
    ui: &Ui,
    first: bool,
    timeout: Duration,
) -> Result<Vec<Target>> {
    let Some(text) = explicit else {
        return Ok(vec![choose_device(identity, ui, timeout, first)?]);
    };

    let queries: Vec<&str> = text
        .split(',')
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .collect();
    if queries.is_empty() {
        bail!("--to was given but names no device");
    }

    let mut targets: Vec<Target> = Vec::with_capacity(queries.len());
    for query in queries {
        let target = resolve_query(query, identity, settings, ui, timeout)?;
        // Naming the same device twice would transfer to it twice.
        if targets
            .iter()
            .any(|seen| seen.public_key == target.public_key && seen.address == target.address)
        {
            ui.note(format!(
                "  {} is listed twice; sending once",
                target.label()
            ));
            continue;
        }
        targets.push(target);
    }

    // A device advertising port 0 is switched off, so say so before connecting to
    // anything rather than after the first transfer has already gone out.
    for target in &targets {
        if target.port == 0 {
            bail!(
                "{} is not accepting transfers (its discovery reply advertises port 0)",
                target.label()
            );
        }
    }
    Ok(targets)
}

fn cmd_text(ui: &Ui, settings: &mut Settings, identity: &Identity, args: TextArgs) -> Result<()> {
    let target = resolve_target(
        args.to.as_deref(),
        identity,
        settings,
        ui,
        Duration::from_secs(3),
        args.first,
    )?;
    if target.port == 0 {
        bail!("{} is not accepting connections (port 0)", target.label());
    }
    let (mut client, target) =
        connect_target(&target, identity, settings, ui, Duration::from_secs(3))?;
    client.set_reply_timeout(Duration::from_secs(60))?;
    client.exchange_supported_message_types()?;
    let local = LocalDevice {
        name: settings.device_name(),
        device_type: device_type().to_string(),
    };
    client.exchange_device_info(&local)?;
    client.send_text(&args.text)?;
    ui.note(format!("text delivered to {}", target.label()));
    ui.finish(json!({
        "status": "ok",
        "target": target.to_json(),
        "length": args.text.chars().count(),
    }));
    client.close();
    Ok(())
}

/// State shared by every in-flight connection.
///
/// The receiver serves connections concurrently, so anything mutable is behind a
/// lock. `settings` is only ever locked for the brief trust-list read or write,
/// never across a transfer — a lock held for the length of a transfer would
/// serialise exactly what this is meant to stop serialising.
struct Shared {
    settings: Mutex<Settings>,
    /// Interactive prompts read one stdin, so two at once would interleave their
    /// questions on the same terminal.
    prompt: Mutex<()>,
    /// Connections actually dealt with, for `--once` and `--max`. Counted after a
    /// connection is finished with, not when it arrives: something that connects
    /// and hangs up before the handshake is not a transfer and must not consume a
    /// `--max` slot.
    handled: AtomicUsize,
}

impl Shared {
    fn with_settings<T>(&self, f: impl FnOnce(&mut Settings) -> T) -> T {
        let mut guard = self
            .settings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut guard)
    }

    /// Serialise a prompt. The guard is released as soon as the question is asked.
    fn ask<T>(&self, f: impl FnOnce() -> T) -> T {
        let _guard = self
            .prompt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f()
    }
}

fn cmd_receive(
    ui: &Ui,
    settings: &mut Settings,
    identity: &Identity,
    args: ReceiveArgs,
) -> Result<()> {
    let download_dir = args.dir.clone().unwrap_or_else(|| settings.download_dir());
    fs::create_dir_all(&download_dir)
        .with_context(|| format!("cannot create {}", download_dir.display()))?;

    // `None` means the user said nothing, so reuse whatever was pinned last time.
    // An explicit `0` means "a free port every run", which also clears that.
    let requested_port = args.port.unwrap_or(settings.listening_port);
    let listener = TcpListener::bind(("0.0.0.0", requested_port))
        .with_context(|| format!("cannot listen on port {requested_port}"))?;
    let port = listener.local_addr()?.port();

    // Checked before the banner is printed, so the delay — when there is one —
    // does not land in the middle of it. Advisory: it never changes what we do.
    let firewall = crate::firewall::inspect(port);

    // Remember a pinned port, but only once it is proven bindable: persisting one
    // that is already taken would make the next run fail for a reason the user set
    // up themselves.
    let remember = match args.port {
        Some(0) => Some(0),
        Some(pinned) if settings.listening_port != pinned => Some(pinned),
        _ => None,
    };
    if let Some(pinned) = remember {
        settings.listening_port = pinned;
        match settings.save() {
            Ok(()) => ui.banner(format!(
                "  port setting: {}",
                if pinned == 0 {
                    "forgotten; a free port is picked each run".to_string()
                } else {
                    format!("remembered {pinned} for future runs")
                }
            )),
            Err(error) => ui.warn(format!("could not remember the port: {error}")),
        }
    }

    let local = LocalDevice {
        name: args
            .name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| settings.device_name()),
        device_type: device_type().to_string(),
    };

    ui.banner("LANDrop CLI receiver");
    ui.banner(format!("  name        : {}", local.name));
    ui.banner(format!("  port        : {port}"));
    ui.banner(format!("  saving to   : {}", download_dir.display()));
    ui.banner(format!("  identity    : {}", identity.pk_base64()));
    ui.banner(format!(
        "  accepting   : {}",
        if args.yes {
            "everything (--yes)"
        } else {
            "trusted devices; others are declined without a terminal"
        }
    ));
    ui.banner(format!(
        "  trusted     : {} sender(s)",
        settings.trusted_devices.len()
    ));
    if let Some(report) = &firewall {
        ui.banner(format!("  firewall    : {}", describe_firewall(report)));
        if let Some(warning) = report.warning() {
            ui.warn(warning);
        }
    }

    // Stop cleanly on Ctrl-C so a service manager sees a clean shutdown.
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        let _ = ctrlc::set_handler(move || {
            stop.store(true, Ordering::Relaxed);
        });
    }

    // Advertise ourselves on the LAN. If the desktop app already owns the
    // discovery port this fails; the TCP listener still works for direct sends.
    let mut responder = Some({
        let stop = Arc::clone(&stop);
        let public_key = identity.pk_base64();
        let name = local.name.clone();
        let dtype = local.device_type.clone();
        let discovery_port = args.discovery_port;
        std::thread::spawn(move || {
            discovery::run_responder(public_key, name, dtype, port, discovery_port, stop)
        })
    });
    std::thread::sleep(Duration::from_millis(250));
    if responder
        .as_ref()
        .is_some_and(|handle| handle.is_finished())
    {
        if let Some(handle) = responder.take()
            && let Ok(Err(e)) = handle.join()
        {
            let message = format!(
                "could not advertise on UDP {}: {e}. Other devices will not discover \
                 this receiver automatically; add it manually as <this-ip>:{port}.",
                args.discovery_port
            );
            if ui.quiet {
                ui.warn(message);
            } else {
                ui.warn(format!(
                    "{message}\n  (the desktop LANDrop app holds that port while it runs)"
                ));
            }
        }
    } else {
        ui.banner(format!(
            "  advertising : multicast {}:{}",
            discovery::MULTICAST_GROUP,
            args.discovery_port
        ));
    }

    ui.note(format!(
        "listening for incoming transfers on port {port} (Ctrl-C to stop)"
    ));

    // Non-blocking accept lets the stop flag interrupt the loop promptly.
    listener.set_nonblocking(true)?;

    // Connections are served concurrently: one device that is slow, or sitting on
    // an accept prompt, must not stop the next device being served at all. Nothing
    // mutable is shared directly; `settings` goes behind Shared's lock.
    let shared = Arc::new(Shared {
        settings: Mutex::new(std::mem::take(settings)),
        prompt: Mutex::new(()),
        handled: AtomicUsize::new(0),
    });
    let mut workers: Vec<std::thread::JoinHandle<()>> = Vec::new();

    // `--once` is a limit of one, not a reason to stop immediately: the check below
    // runs before every accept, so it has to be a count of what has been served.
    let limit = if args.once { 1 } else { args.max };

    while !stop.load(Ordering::Relaxed) {
        // Checked here rather than at the bottom of the loop: once the limit is
        // reached there may be no further connection to trigger a check down
        // there, and the receiver would sit polling instead of exiting.
        if limit > 0 && shared.handled.load(Ordering::Relaxed) >= limit {
            break;
        }

        let (stream, _) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(150));
                continue;
            }
            Err(e) => {
                ui.warn(format!("accept failed: {e}"));
                std::thread::sleep(Duration::from_millis(300));
                continue;
            }
        };
        // Accepted sockets can inherit the listener's non-blocking mode.
        if let Err(e) = stream.set_nonblocking(false) {
            ui.warn(format!("could not make the connection blocking: {e}"));
            continue;
        }
        let peer_addr = stream
            .peer_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        // The handshake happens inside the worker too: a peer that connects and
        // then stalls must not hold up the accept loop either.
        let worker_ui = ui.clone();
        let worker_shared = Arc::clone(&shared);
        let worker_local = local.clone();
        let worker_dir = download_dir.clone();
        let worker_identity = identity.clone();
        let auto_accept = args.yes;

        workers.push(std::thread::spawn(move || {
            let mut server =
                match ServerSession::accept(stream, &worker_identity, Duration::from_secs(15)) {
                    Ok(server) => server,
                    Err(e) => {
                        worker_ui.warn(format!("handshake with {peer_addr} failed: {e:#}"));
                        return;
                    }
                };
            if let Err(e) = serve_one(
                &worker_ui,
                &mut server,
                &worker_local,
                &worker_dir,
                auto_accept,
                peer_addr,
                &worker_shared,
            ) {
                worker_ui.warn(format!("transfer failed: {e:#}"));
            }
            worker_shared.handled.fetch_add(1, Ordering::Relaxed);
        }));
    }

    // Transfers already running are allowed to finish before we report.
    for worker in workers {
        let _ = worker.join();
    }
    let served = shared.handled.load(Ordering::Relaxed);

    stop.store(true, Ordering::Relaxed);
    if let Some(handle) = responder.take() {
        let _ = handle.join();
    }
    // Hand the trust list, which a worker may have added to, back to the caller.
    *settings = shared
        .settings
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();

    ui.note(format!("served {served} connection(s); stopping."));
    ui.finish(json!({
        "status": "ok",
        "served": served,
        // `null` when this platform has nothing to say, which is not the same as
        // "no firewall": it means the question is not asked here.
        "firewall": firewall.as_ref().map(crate::firewall::InboundReport::to_json),
    }));
    Ok(())
}

/// One banner line. Never claims more than the report actually established.
fn describe_firewall(report: &crate::firewall::InboundReport) -> String {
    match (report.active, report.allowed) {
        (false, _) => "not filtering (nothing to allow)".to_string(),
        (true, Some(true)) => "active, and an inbound rule covers this listener".to_string(),
        (true, Some(false)) => "ACTIVE, with no inbound rule for this listener".to_string(),
        (true, None) => "active; whether it covers this listener was not determined".to_string(),
    }
}

fn serve_one(
    ui: &Ui,
    server: &mut ServerSession,
    local: &LocalDevice,
    download_dir: &Path,
    auto_accept: bool,
    peer_addr: String,
    shared: &Shared,
) -> Result<()> {
    let config = ServerConfig {
        local: local.clone(),
        download_dir: download_dir.to_path_buf(),
    };

    let interactive = ui.interactive();
    let mut saved: Vec<serde_json::Value> = Vec::new();
    let mut bar: Option<ProgressBar> = None;
    // Recorded by `decide` so we can offer to trust the sender afterwards.
    let mut sender: Option<(String, String)> = None;
    let mut accepted = false;

    let mut decide = |ctx: &OfferContext<'_>| -> Result<bool> {
        let short_key: String = ctx.peer_public_key.chars().take(12).collect();
        let who = if ctx.peer_name.is_empty() {
            short_key.as_str()
        } else {
            ctx.peer_name.as_str()
        };
        ui.note(format!(
            "incoming transfer from {} [{}] ({})",
            who, ctx.peer_type, peer_addr
        ));
        ui.note(format!("  verification code : {}", ctx.verif_code));
        ui.note(format!(
            "  {} file(s), {} total",
            ctx.request.files.len(),
            human_bytes(ctx.total_size)
        ));
        for file in &ctx.request.files {
            ui.note(format!(
                "    {:<50} {:>12}",
                file.filename,
                human_bytes(file.size)
            ));
        }

        let remember = || Some((ctx.peer_name.clone(), ctx.peer_public_key.clone()));

        // A trusted sender is accepted silently, exactly like the desktop app.
        if shared.with_settings(|s| s.is_trusted(&ctx.peer_public_key)) {
            let label = if ctx.peer_name.is_empty() {
                short_key.clone()
            } else {
                ctx.peer_name.clone()
            };
            ui.note(format!(
                "  trusted device ({label}) — accepting automatically"
            ));
            accepted = true;
            sender = remember();
            return Ok(true);
        }
        if auto_accept {
            ui.note("  auto-accepting (--yes)");
            accepted = true;
            sender = remember();
            return Ok(true);
        }
        if !interactive {
            // A background service must not accept unknown senders. Log the key so
            // an administrator can authorise it with `trusted --add`.
            ui.warn(format!(
                "declining {} ({}): not in the trust list and no terminal to confirm.\n  \
                 to allow it:  landrop-cli trusted --add {}",
                who, ctx.peer_public_key, ctx.peer_public_key
            ));
            return Ok(false);
        }
        let ok = shared.ask(|| {
            Confirm::new()
                .with_prompt("Accept these files?")
                .default(true)
                .interact()
                .map_err(|e| anyhow!("confirmation failed: {e}"))
        })?;
        if ok {
            accepted = true;
            sender = remember();
        }
        Ok(ok)
    };

    let mut progress = |done: u64, total: u64| {
        if bar.is_none() && total > 0 {
            bar = Some(ui.progress_bar(total, "receiving"));
        }
        if let Some(bar) = &bar {
            bar.set_position(done);
            bar.set_message(format!("{} / {}", human_bytes(done), human_bytes(total)));
        }
    };
    let mut on_text = |text: &str| {
        ui.note(format!(
            "\n--- text received ---\n{text}\n---------------------"
        ));
    };
    let mut on_file = |entry: &transfer::IncomingFile| {
        saved.push(json!({
            "filename": entry.descriptor.filename,
            "size": entry.descriptor.size,
            "path": entry.target.to_string_lossy(),
        }));
    };

    let mut callbacks = ReceiveCallbacks {
        decide: &mut decide,
        progress: &mut progress,
        text: &mut on_text,
        file_done: &mut on_file,
    };

    let result = server.serve(&config, &mut callbacks);
    if let Some(bar) = bar {
        bar.finish_and_clear();
    }
    result?;

    if !saved.is_empty() {
        ui.note(format!("\nsaved {} file(s):", saved.len()));
        for entry in &saved {
            ui.note(format!("  {}", entry["path"].as_str().unwrap_or("?")));
        }
        ui.finish(json!({ "status": "ok", "received": saved }));
    }

    // Mirror the app: after a completed receive from an unknown sender, offer to
    // remember it. Never modify the trust list without an explicit yes.
    if accepted
        && !saved.is_empty()
        && let Some((name, public_key)) = sender
        && !shared.with_settings(|s| s.is_trusted(&public_key))
        && interactive
        && !auto_accept
    {
        let label = if name.is_empty() {
            public_key.chars().take(12).collect::<String>()
        } else {
            name.clone()
        };
        let want = shared.ask(|| {
            Confirm::new()
                .with_prompt(format!("Always accept files from {label}?"))
                .default(true)
                .interact()
                .map_err(|e| anyhow!("confirmation failed: {e}"))
        })?;
        if want {
            shared.with_settings(|s| {
                s.add_trusted(&name, &public_key);
                s.save()
            })?;
            ui.note(format!(
                "trusted {label} — future transfers will not prompt"
            ));
        }
    }
    Ok(())
}

fn cmd_devices(ui: &Ui, settings: &mut Settings, args: DevicesArgs) -> Result<()> {
    if let Some(query) = &args.forget {
        let before = settings.known_devices.len();
        let query_lower = query.to_ascii_lowercase();
        settings
            .known_devices
            .retain(|d| d.public_key != *query && d.name.to_ascii_lowercase() != query_lower);
        let removed = before - settings.known_devices.len();
        if removed == 0 {
            bail!("no remembered device matched {query:?}");
        }
        settings.save()?;
        ui.note(format!("forgot {removed} device(s)"));
        ui.finish(json!({ "status": "ok", "forgotten": removed }));
        return Ok(());
    }

    if settings.known_devices.is_empty() {
        ui.note("no remembered devices yet. Run `landrop-cli discover` first.");
    } else {
        ui.note(format!(
            "{} remembered device(s):",
            settings.known_devices.len()
        ));
        for device in &settings.known_devices {
            ui.note(format!(
                "  {} [{}] {}:{}",
                device.name, device.device_type, device.last_address, device.last_port
            ));
        }
    }
    ui.finish(json!({ "devices": settings.known_devices }));
    Ok(())
}

fn cmd_trusted(ui: &Ui, settings: &mut Settings, args: TrustedArgs) -> Result<()> {
    let actions = [&args.add, &args.remove, &args.edit]
        .iter()
        .filter(|a| a.is_some())
        .count();
    if actions > 1 {
        bail!("use only one of --add, --remove or --edit");
    }
    if args.name.is_some() && args.edit.is_none() {
        bail!("--name only applies to --edit");
    }

    if let Some(query) = &args.add {
        // Accept either a raw public key or a device name we have seen before.
        let (name, public_key) = if query.len() == 44 {
            let known = settings.lookup(query);
            (known.map(|d| d.name).unwrap_or_default(), query.clone())
        } else {
            match settings.lookup(query) {
                Some(device) => (device.name, device.public_key),
                None => bail!(
                    "no known device matches {query:?}. Run `landrop-cli discover` first, \
                     or pass the 44-character public key directly."
                ),
            }
        };
        let is_new = settings.add_trusted(&name, &public_key);
        settings.save()?;
        let label = if name.is_empty() { &public_key } else { &name };
        ui.note(if is_new {
            format!("trusted {label}")
        } else {
            format!("{label} was already trusted")
        });
        ui.finish(json!({
            "status": "ok",
            "added": is_new,
            "name": name,
            "public_key": public_key,
        }));
        return Ok(());
    }

    if let Some(query) = &args.edit {
        let new_name = args.name.as_deref().unwrap_or_default().trim();
        if new_name.is_empty() {
            bail!("--name must not be empty");
        }
        // Resolve to a key first: a query can match by name, and after the rename
        // that name no longer identifies the entry.
        let Some(key) = settings.find_trusted(query).map(|d| d.public_key.clone()) else {
            bail!("no trusted device matched {query:?}");
        };
        // Names are selectors for --remove and --edit, so two entries sharing one
        // would make those commands ambiguous.
        if let Some(other) = settings.find_trusted(new_name)
            && other.public_key != key
        {
            bail!(
                "{new_name:?} already names another trusted device; \
                 pick a name that still tells them apart"
            );
        }
        let renamed = settings.rename_trusted(&key, new_name);
        settings.save()?;
        ui.note(format!("renamed {renamed} entry/entries to {new_name:?}"));
        ui.finish(json!({
            "status": "ok",
            "renamed": renamed,
            "name": new_name,
            "public_key": key,
        }));
        return Ok(());
    }

    if let Some(query) = &args.remove {
        let removed = settings.remove_trusted(query);
        if removed == 0 {
            bail!("no trusted device matched {query:?}");
        }
        settings.save()?;
        ui.note(format!(
            "removed {removed} entry/entries from the trust list"
        ));
        ui.finish(json!({ "status": "ok", "removed": removed }));
        return Ok(());
    }

    if settings.trusted_devices.is_empty() {
        ui.note("no trusted devices. The trust list is empty, so every incoming");
        ui.note("transfer in `receive` mode will ask for confirmation.");
    } else {
        ui.note(format!(
            "{} trusted device(s) — transfers from these are accepted without asking:",
            settings.trusted_devices.len()
        ));
        for device in &settings.trusted_devices {
            ui.note(format!(
                "  {}  {}",
                if device.name.is_empty() {
                    "(unnamed)"
                } else {
                    &device.name
                },
                &device.public_key[..device.public_key.len().min(16)]
            ));
        }
    }
    ui.finish(json!({ "trusted_devices": settings.trusted_devices }));
    Ok(())
}

fn cmd_identity(
    ui: &Ui,
    settings: &Settings,
    identity: &Identity,
    args: IdentityArgs,
) -> Result<()> {
    let config_dir = crate::config::config_dir()?;
    ui.note(format!("device name  : {}", settings.device_name()));
    ui.note(format!("device type  : {}", device_type()));
    ui.note(format!("public key   : {}", identity.pk_base64()));
    ui.note(format!("config dir   : {}", config_dir.display()));
    ui.note(format!(
        "identity file: {}",
        crate::config::identity_path()?.display()
    ));
    ui.note(format!(
        "download dir : {}",
        settings.download_dir().display()
    ));
    if args.show_secret {
        ui.note(format!("secret key   : {}", identity.sk_base64()));
    }
    ui.finish(json!({
        "device_name": settings.device_name(),
        "device_type": device_type(),
        "public_key": identity.pk_base64(),
        "secret_key": if args.show_secret { Some(identity.sk_base64()) } else { None },
        "config_dir": config_dir.to_string_lossy(),
        "download_dir": settings.download_dir().to_string_lossy(),
        "version": crate::VERSION,
    }));
    Ok(())
}

/// End-to-end check over loopback: no network peer and no user interaction.
fn cmd_selftest(ui: &Ui, args: SelftestArgs) -> Result<()> {
    let size = args.size.max(1);
    let source_dir = tempfile_dir()?;
    let dest_dir = tempfile_dir()?;

    let source = source_dir.join("selftest.bin");
    let payload: Vec<u8> = (0..size).map(|i| ((i * 31 + 7) % 251) as u8).collect();
    fs::write(&source, &payload)?;

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    let server_identity = Identity::generate();
    let client_identity = Identity::generate();

    let dest_clone = dest_dir.clone();
    let server_thread = std::thread::spawn(move || -> Result<Vec<PathBuf>> {
        let (stream, _) = listener.accept()?;
        let mut server = ServerSession::accept(stream, &server_identity, Duration::from_secs(30))?;
        let mut written: Vec<PathBuf> = Vec::new();
        let config = ServerConfig {
            local: LocalDevice {
                name: "selftest-server".to_string(),
                device_type: device_type().to_string(),
            },
            download_dir: dest_clone,
        };
        {
            let mut decide = |_: &OfferContext<'_>| Ok(true);
            let mut progress = |_: u64, _: u64| {};
            let mut text = |_: &str| {};
            let mut file_done = |entry: &transfer::IncomingFile| written.push(entry.target.clone());
            let mut callbacks = ReceiveCallbacks {
                decide: &mut decide,
                progress: &mut progress,
                text: &mut text,
                file_done: &mut file_done,
            };
            server.serve(&config, &mut callbacks)?;
        }
        Ok(written)
    });

    let leaves = transfer::parse_paths(&[source])?;
    let descriptors: Vec<FileDescriptor> = leaves
        .iter()
        .map(transfer::describe)
        .collect::<Result<_>>()?;

    let mut client = ClientSession::connect(addr, &client_identity, None, Duration::from_secs(30))?;
    client.exchange_supported_message_types()?;
    client.exchange_device_info(&LocalDevice {
        name: "selftest-client".to_string(),
        device_type: device_type().to_string(),
    })?;

    let started = Instant::now();
    let sent = client.send_files(&leaves, &descriptors, &mut |_, _| {})?;
    let elapsed = started.elapsed().as_secs_f64();
    drop(client);

    let written = server_thread
        .join()
        .map_err(|_| anyhow!("the selftest server thread panicked"))??;
    if written.len() != 1 {
        bail!("expected exactly one received file, got {}", written.len());
    }
    let received = fs::read(&written[0])?;
    if received != payload {
        bail!(
            "payload mismatch: sent {} bytes, received {} bytes",
            payload.len(),
            received.len()
        );
    }

    let rate = if elapsed > 0.0 {
        sent as f64 / elapsed
    } else {
        0.0
    };
    ui.note(format!(
        "selftest OK: {} transferred intact in {:.2}s ({}/s)",
        human_bytes(sent),
        elapsed,
        human_bytes(rate as u64)
    ));
    ui.finish(json!({
        "status": "ok",
        "bytes": sent,
        "duration_seconds": elapsed,
        "throughput_bytes_per_second": rate as u64,
    }));

    let _ = fs::remove_dir_all(&source_dir);
    let _ = fs::remove_dir_all(&dest_dir);
    Ok(())
}

fn tempfile_dir() -> Result<PathBuf> {
    let base = std::env::temp_dir().join(format!(
        "landrop-cli-selftest-{}-{:x}",
        std::process::id(),
        rand::random::<u32>()
    ));
    fs::create_dir_all(&base)?;
    Ok(base)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn run(cli: Cli) -> Result<()> {
    let ui = Ui {
        json: cli.json,
        quiet: cli.quiet,
    };
    if let Some(dir) = &cli.config_dir {
        crate::config::set_config_dir_override(dir.clone());
    }
    let identity = load_or_create_identity()?;
    let mut settings = Settings::load()?;

    match cli.command {
        Command::Discover(args) => cmd_discover(&ui, &mut settings, &identity, args),
        Command::Send(args) => cmd_send(&ui, &mut settings, &identity, args),
        Command::Receive(args) => cmd_receive(&ui, &mut settings, &identity, args),
        Command::Text(args) => cmd_text(&ui, &mut settings, &identity, args),
        Command::Devices(args) => cmd_devices(&ui, &mut settings, args),
        Command::Trusted(args) => cmd_trusted(&ui, &mut settings, args),
        Command::Identity(args) => cmd_identity(&ui, &settings, &identity, args),
        Command::Selftest(args) => cmd_selftest(&ui, args),
    }
}

/// Parse the command line, run the requested command, and report any failure.
///
/// Returns the process exit code rather than calling `exit`, so the thin binary
/// keeps ownership of how the process ends.
pub fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    if let Err(error) = run(cli) {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "status": "error",
                    "error": format!("{error:#}"),
                }))
                .unwrap_or_else(|_| "{\"status\":\"error\"}".to_string())
            );
        }
        eprintln!("error: {error:#}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}
