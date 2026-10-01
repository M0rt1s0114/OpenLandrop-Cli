// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! Everywhere else.
//!
//! No check rather than a guessed one: each platform's firewall is its own story,
//! and inventing a shared guess would be worse than saying nothing. Adding one
//! means adding a module and a `cfg` arm in the parent, exactly like the two that
//! exist.

use super::InboundReport;

pub fn inspect(_port: u16) -> Option<InboundReport> {
    None
}
