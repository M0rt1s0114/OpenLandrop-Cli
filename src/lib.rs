// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! A terminal client for the `LANDrop` v2 LAN protocol.
//!
//! It implements the same wire protocol as `LANDrop` v2, so it interoperates with
//! `LANDrop` v2 devices without any change on their side. The protocol is
//! implemented by `wire`, `messages` and `crypto`, which are its specification.

pub mod cli;
pub mod config;
pub mod crypto;
pub mod discovery;
pub mod firewall;
pub mod messages;
pub mod session;
pub mod transfer;
pub mod wire;

/// The protocol version string this implementation speaks.
pub const PROTOCOL_VERSION: &str = "v2";

/// Human-readable crate version, reported by `--version` and `identity`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
