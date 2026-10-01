// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! Device discovery over UDP.
//!
//! `LANDrop` v2 announces itself by multicasting a JSON blob to
//! `239.192.52.63:52637`, and also to each interface's subnet broadcast address.
//! A peer that sees `request: true` answers **unicast to the source address and
//! port** of the datagram — which is why a seeker must send from the same socket
//! it intends to listen on, and can use an ephemeral port safely.

use crate::messages::DeviceInfo;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

pub const MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(239, 192, 52, 63);
pub const DISCOVERY_PORT: u16 = 52637;
pub const MULTICAST_TTL: u32 = 32;

/// How often the responder announces itself. Fast enough that a device switched
/// on appears while somebody is still looking at the list.
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(2);

/// The discovery datagram. Fields are defaulted on parse so we tolerate peers that
/// omit optional keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryPacket {
    #[serde(default)]
    pub request: bool,
    #[serde(default)]
    pub supported_versions: Vec<String>,
    #[serde(default)]
    pub public_key: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub device_info: DeviceInfo,
    #[serde(default)]
    pub discoverable: bool,
}

impl DiscoveryPacket {
    /// "Who is out there?"
    ///
    /// Without an advertisement this carries `discoverable: false`, an empty
    /// `device_info` and `port: 0` — the honest answer for a one-shot seek that
    /// is not listening for anything.
    ///
    /// With one, it carries the same self-description an announcement does. A
    /// seeker that says nothing about itself is telling every listener that it
    /// is not there, and they are entitled to act on that; a device that is both
    /// asking and listening has to say so while it asks, or it disappears from
    /// their lists between announcements and comes back when the next one lands.
    /// A client that is listening has to say so while it asks.
    pub fn request(public_key: &str, advertise: Option<&Advertising>) -> Self {
        Self {
            request: true,
            supported_versions: vec![crate::PROTOCOL_VERSION.to_string()],
            public_key: public_key.to_string(),
            port: advertise.map_or(0, |advertisement| advertisement.port),
            device_info: advertise.map_or_else(DeviceInfo::default, |advertisement| DeviceInfo {
                name: advertisement.name.clone(),
                device_type: advertisement.device_type.clone(),
            }),
            discoverable: advertise.is_some(),
        }
    }

    /// "Here I am" — the reply, and the periodic announcement in receive mode.
    pub fn announce(public_key: &str, name: &str, device_type: &str, port: u16) -> Self {
        Self {
            request: false,
            supported_versions: vec![crate::PROTOCOL_VERSION.to_string()],
            public_key: public_key.to_string(),
            port,
            device_info: DeviceInfo {
                name: name.to_string(),
                device_type: device_type.to_string(),
            },
            discoverable: true,
        }
    }
}

/// An IPv4 interface with its derived broadcast address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interface {
    pub name: String,
    pub address: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub broadcast: Ipv4Addr,
}

/// Enumerate usable IPv4 interfaces. Loopback is excluded unless asked for.
pub fn interfaces(include_loopback: bool) -> Result<Vec<Interface>> {
    let mut out = Vec::new();
    for iface in if_addrs::get_if_addrs().context("enumerating network interfaces")? {
        let v4 = match iface.addr {
            if_addrs::IfAddr::V4(v4) => v4,
            if_addrs::IfAddr::V6(_) => continue,
        };
        if v4.ip.is_loopback() && !include_loopback {
            continue;
        }
        if v4.ip.is_unspecified() {
            continue;
        }
        out.push(Interface {
            name: iface.name,
            address: v4.ip,
            netmask: v4.netmask,
            broadcast: broadcast_of(v4.ip, v4.netmask),
        });
    }
    Ok(out)
}

/// `ip | ~netmask`, as one `u32` operation rather than four octets.
fn broadcast_of(ip: Ipv4Addr, netmask: Ipv4Addr) -> Ipv4Addr {
    let ip = u32::from(ip);
    let mask = u32::from(netmask);
    Ipv4Addr::from(ip | !mask)
}

/// A device seen on the LAN.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredDevice {
    pub name: String,
    #[serde(rename = "type")]
    pub device_type: String,
    pub address: String,
    pub port: u16,
    pub public_key: String,
    pub discoverable: bool,
}

impl DiscoveredDevice {
    fn from_packet(packet: &DiscoveryPacket, from: SocketAddr) -> Self {
        Self {
            name: packet.device_info.name.clone(),
            device_type: packet.device_info.device_type.clone(),
            address: from.ip().to_string(),
            port: packet.port,
            public_key: packet.public_key.clone(),
            discoverable: packet.discoverable,
        }
    }

    /// A short label for menus, always unique enough to pick from.
    pub fn label(&self) -> String {
        let type_tag = if self.device_type.is_empty() {
            "unknown".to_string()
        } else {
            self.device_type.clone()
        };
        format!(
            "{} [{}] {}:{}",
            if self.name.is_empty() {
                "(unnamed)"
            } else {
                &self.name
            },
            type_tag,
            self.address,
            self.port
        )
    }
}

/// What a seeker says about itself while it asks.
///
/// Only meaningful for a process that is also listening for transfers: a peer
/// that receives this will offer the device to its user. Leaving
/// [`DiscoverOptions::advertise`] as `None` means "not listening", which is the
/// truth for a one-shot command-line seek, and is what the wire format has a
/// place for.
#[derive(Debug, Clone)]
pub struct Advertising {
    pub name: String,
    pub device_type: String,
    pub port: u16,
}

#[derive(Debug, Clone)]
pub struct DiscoverOptions {
    pub public_key: String,
    pub timeout: Duration,
    /// Send a fresh round this often (the app re-announces every 2 s).
    pub resend_interval: Duration,
    pub include_loopback: bool,
    /// Almost always `DISCOVERY_PORT`; overridable so several instances can be
    /// tested on one host without fighting over the well-known port.
    pub discovery_port: u16,
    /// Sent in our own query packets. See [`Advertising`], and
    /// [`DiscoveryPacket::request`] for why silence is not neutral.
    pub advertise: Option<Advertising>,
}

impl DiscoverOptions {
    pub fn new(public_key: impl Into<String>) -> Self {
        Self {
            public_key: public_key.into(),
            timeout: Duration::from_secs(3),
            resend_interval: Duration::from_millis(1500),
            include_loopback: false,
            discovery_port: DISCOVERY_PORT,
            advertise: None,
        }
    }
}

/// Broadcast a discovery request on every interface and gather the replies.
///
/// Each interface gets its own socket bound to that interface's address, so replies
/// come back with the correct source and no cross-interface ambiguity.
pub fn discover(opts: &DiscoverOptions) -> Result<Vec<DiscoveredDevice>> {
    let ifaces = interfaces(opts.include_loopback)?;
    if ifaces.is_empty() {
        bail!("no usable IPv4 interfaces found");
    }

    let mut handles = Vec::new();
    for iface in ifaces {
        let options = opts.clone();
        handles.push(std::thread::spawn(move || {
            probe_interface(&iface, &options)
        }));
    }

    let mut found: BTreeMap<String, DiscoveredDevice> = BTreeMap::new();
    for handle in handles {
        match handle.join() {
            Ok(Ok(devices)) => {
                for device in devices {
                    if device.public_key != opts.public_key {
                        found.insert(device.public_key.clone(), device);
                    }
                }
            }
            Ok(Err(e)) => eprintln!("warning: discovery on an interface failed: {e:#}"),
            Err(_) => eprintln!("warning: a discovery thread panicked"),
        }
    }
    Ok(found.into_values().collect())
}

fn probe_interface(iface: &Interface, opts: &DiscoverOptions) -> Result<Vec<DiscoveredDevice>> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.bind(&SocketAddr::from((iface.address, 0)).into())?;
    socket.set_broadcast(true)?;
    socket.set_multicast_ttl_v4(MULTICAST_TTL)?;
    socket.set_multicast_loop_v4(false)?;
    // Joining is not strictly required to receive unicast replies, but it lets us
    // observe unsolicited announcements too.
    if let Err(e) = socket.join_multicast_v4(&MULTICAST_GROUP, &iface.address) {
        eprintln!(
            "warning: could not join {} on {}: {e}",
            MULTICAST_GROUP, iface.address
        );
    }
    let _ = socket.set_multicast_if_v4(&iface.address);
    let udp: UdpSocket = socket.into();
    udp.set_read_timeout(Some(Duration::from_millis(200)))?;

    let request = serde_json::to_vec(&DiscoveryPacket::request(
        &opts.public_key,
        opts.advertise.as_ref(),
    ))?;
    let multicast_target = SocketAddr::from((MULTICAST_GROUP, opts.discovery_port));
    let broadcast_target = SocketAddr::from((iface.broadcast, opts.discovery_port));

    let deadline = Instant::now() + opts.timeout;
    let mut next_send = Instant::now();
    let mut buf = vec![0u8; 65_536];
    let mut devices: BTreeMap<String, DiscoveredDevice> = BTreeMap::new();

    while Instant::now() < deadline {
        if Instant::now() >= next_send {
            // A single dropped datagram should not hide a device, so both targets
            // are probed on every round.
            let _ = udp.send_to(&request, multicast_target);
            let _ = udp.send_to(&request, broadcast_target);
            next_send = Instant::now() + opts.resend_interval;
        }
        match udp.recv_from(&mut buf) {
            Ok((n, from)) => {
                let packet: DiscoveryPacket = match serde_json::from_slice(&buf[..n]) {
                    Ok(packet) => packet,
                    Err(_) => continue,
                };
                if packet.request {
                    continue; // somebody else's query, not an answer
                }
                if packet.public_key == opts.public_key {
                    continue; // our own announcement echoed back
                }
                let device = DiscoveredDevice::from_packet(&packet, from);
                devices.insert(device.public_key.clone(), device);
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(anyhow!(e).context("receiving discovery reply")),
        }
    }
    Ok(devices.into_values().collect())
}

/// A socket whose only way out is one interface.
///
/// `set_multicast_if_v4` lives on `socket2::Socket` and has no equivalent on
/// `std::net::UdpSocket`, so it has to be called before the conversion. Sending
/// is the only thing this socket is for, which is why nothing is lost by
/// converting it immediately.
fn egress_socket(address: &Ipv4Addr) -> Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.bind(&SocketAddr::from((*address, 0)).into())?;
    socket.set_broadcast(true)?;
    socket.set_multicast_ttl_v4(MULTICAST_TTL)?;
    socket.set_multicast_loop_v4(false)?;
    socket.set_multicast_if_v4(address)?;
    Ok(socket.into())
}

/// Long-running discovery responder for receive mode.
///
/// Answers `request: true` datagrams unicast back to the asker, and periodically
/// announces our presence. Returns an error if the discovery port cannot be bound
/// (commonly because the `LANDrop` desktop app already owns it on this host).
///
/// `stop` is polled between rounds so the caller can shut it down cleanly.
///
/// Announcing is not as simple as writing to the multicast group. A group
/// address says nothing about *which* interface the packet should leave by, so
/// that is a separate decision, and leaving it unmade means the routing table
/// makes it instead. On a machine with a VPN that is the VPN: the announcement
/// reaches a network with nothing on it while the devices on the real one never
/// hear this machine at all. The multicast and the broadcast are both sent once
/// per interface, from a socket whose only way out is that interface.
pub fn run_responder(
    public_key: String,
    name: String,
    device_type: String,
    tcp_port: u16,
    discovery_port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<()> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // Unix only. On Windows `SO_REUSEADDR` is an explicitly different thing: it
    // cannot claim a port another process already holds, so it buys no
    // coexistence, and it rewrites the honest `EADDRINUSE` into `WSAEACCES`
    // ("access denied") — which sends people looking for a firewall problem when
    // the real cause is that the desktop app is running. Measured both ways with
    // `examples/port_probe.rs`.
    #[cfg(unix)]
    socket.set_reuse_address(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, discovery_port)).into())?;
    socket.set_broadcast(true)?;
    socket.set_multicast_ttl_v4(MULTICAST_TTL)?;
    socket.set_multicast_loop_v4(false)?;

    let udp: UdpSocket = socket.into();
    udp.set_read_timeout(Some(Duration::from_millis(250)))?;

    // Which interfaces we have already joined on, so that joining happens once
    // per interface rather than once per round. Joining a group that is already
    // joined is not defined to be harmless on every platform, so it is done once
    // per interface and the answer is read rather than discarded.
    let mut joined: BTreeSet<Ipv4Addr> = BTreeSet::new();

    let announcement = serde_json::to_vec(&DiscoveryPacket::announce(
        &public_key,
        &name,
        &device_type,
        tcp_port,
    ))?;
    let multicast_target = SocketAddr::from((MULTICAST_GROUP, discovery_port));

    let mut next_announce = Instant::now();
    let mut buf = vec![0u8; 65_536];

    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        if Instant::now() >= next_announce {
            // Re-enumerated every round. A wireless adapter
            // that reconnects comes back with a different address, and a
            // responder that read the interface list once keeps announcing into
            // an address that no longer exists — silently, and for as long as it
            // runs.
            let ifaces = interfaces(false)?;
            let present: BTreeSet<Ipv4Addr> = ifaces.iter().map(|iface| iface.address).collect();

            for address in joined.difference(&present).copied().collect::<Vec<_>>() {
                let _ = udp.leave_multicast_v4(&MULTICAST_GROUP, &address);
                joined.remove(&address);
            }
            for address in &present {
                if joined.contains(address) {
                    continue;
                }
                match udp.join_multicast_v4(&MULTICAST_GROUP, address) {
                    Ok(()) => {
                        joined.insert(*address);
                    }
                    Err(e) => eprintln!(
                        "warning: could not join {} on {address}: {e}",
                        MULTICAST_GROUP
                    ),
                }
            }

            for iface in &ifaces {
                // The egress interface has to be chosen before the socket can
                // send: `set_multicast_if_v4` exists only on `socket2::Socket`,
                // and holding the receiving socket in that type would mean
                // `recv_from(&mut [MaybeUninit<u8>])` and therefore `unsafe`,
                // which this crate forbids. One socket per interface answers the
                // same question with none: this socket has exactly one way out,
                // and it is this interface. It is rebuilt every round so that an
                // address which has gone away cannot be sent from again.
                let Ok(egress) = egress_socket(&iface.address) else {
                    continue;
                };
                let _ = egress.send_to(
                    &announcement,
                    SocketAddr::from((iface.broadcast, discovery_port)),
                );
                let _ = egress.send_to(&announcement, multicast_target);
            }
            next_announce = Instant::now() + ANNOUNCE_INTERVAL;
        }

        match udp.recv_from(&mut buf) {
            Ok((n, from)) => {
                let packet: DiscoveryPacket = match serde_json::from_slice(&buf[..n]) {
                    Ok(packet) => packet,
                    Err(_) => continue,
                };
                if !packet.request {
                    continue;
                }
                // Self-discovery is filtered by public key rather than by address.
                // That keeps two instances on one host visible to each other, while
                // still never answering our own query.
                if packet.public_key == public_key {
                    continue;
                }
                let _ = udp.send_to(&announcement, from);
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(anyhow!(e).context("discovery responder receive")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcast_derivation() {
        assert_eq!(
            broadcast_of(
                Ipv4Addr::new(192, 168, 128, 244),
                Ipv4Addr::new(255, 255, 255, 0)
            ),
            Ipv4Addr::new(192, 168, 128, 255)
        );
        assert_eq!(
            broadcast_of(Ipv4Addr::new(10, 1, 2, 3), Ipv4Addr::new(255, 255, 0, 0)),
            Ipv4Addr::new(10, 1, 255, 255)
        );
    }

    #[test]
    fn request_packet_shape() {
        let packet = DiscoveryPacket::request("AAAA", None);
        let json = serde_json::to_value(&packet).unwrap();
        assert_eq!(json["request"], true);
        assert_eq!(json["supported_versions"][0], "v2");
        assert_eq!(json["port"], 0);
        assert_eq!(json["discoverable"], false);
        assert_eq!(json["device_info"]["name"], "");
        assert_eq!(json["device_info"]["type"], "");
    }

    #[test]
    fn a_seeker_that_is_listening_says_so() {
        // Silence is not neutral. A peer drops this public key from its device
        // list on any packet that says `discoverable: false`, a query included,
        // so a seeker that announces nothing about itself deletes itself once per
        // round. See the note on `DiscoveryPacket::request`.
        let advertising = Advertising {
            name: "my-box".to_string(),
            device_type: "linux".to_string(),
            port: 44769,
        };
        let packet = DiscoveryPacket::request("AAAA", Some(&advertising));
        let json = serde_json::to_value(&packet).unwrap();
        assert_eq!(json["request"], true);
        assert_eq!(json["supported_versions"][0], "v2");
        assert_eq!(json["discoverable"], true);
        assert_eq!(json["port"], 44769);
        assert_eq!(json["device_info"]["name"], "my-box");
        assert_eq!(json["device_info"]["type"], "linux");
    }

    #[test]
    fn announce_packet_shape() {
        let packet = DiscoveryPacket::announce("AAAA", "my-box", "linux", 44769);
        let json = serde_json::to_value(&packet).unwrap();
        assert_eq!(json["request"], false);
        assert_eq!(json["port"], 44769);
        assert_eq!(json["discoverable"], true);
        assert_eq!(json["device_info"]["name"], "my-box");
        assert_eq!(json["device_info"]["type"], "linux");
    }

    #[test]
    fn parses_a_captured_reply() {
        // The shape of a discovery reply, with placeholder identifiers.
        let raw = r#"{"request":false,"supported_versions":["v2"],
            "public_key":"AkxBTkRST1AtRVhBTVBMRS1LRVktT05FLi4uLi4uLi4u",
            "port":44769,"device_info":{"name":"build-server","type":"linux"},
            "discoverable":true}"#;
        let packet: DiscoveryPacket = serde_json::from_str(raw).unwrap();
        assert_eq!(packet.port, 44769);
        assert_eq!(packet.device_info.name, "build-server");
        assert_eq!(packet.device_info.device_type, "linux");
        assert!(!packet.request);
    }

    #[test]
    fn tolerates_missing_optional_fields() {
        let packet: DiscoveryPacket =
            serde_json::from_str(r#"{"request":true,"public_key":"AAAA"}"#).unwrap();
        assert!(packet.request);
        assert_eq!(packet.port, 0);
        assert!(!packet.discoverable);
    }
}
