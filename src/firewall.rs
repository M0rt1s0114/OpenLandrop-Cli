// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! Whether inbound connections are likely to reach this program.
//!
//! Advisory only. The desktop app offers a "fix firewall" button that shells out
//! to `netsh` with `-Verb RunAs`; this does not. A CLI that transfers files is not
//! the right place to change a machine's security configuration, it often runs as
//! a service where an elevation prompt is not even available, and an agent that
//! could silently open a port would be a worse thing to have. So this diagnoses
//! and hands the command to whoever can decide.
//!
//! The platforms do not share an implementation, only the report shape. Their
//! situations are genuinely different: Windows creates silent block rules for new
//! listeners, Linux has whatever firewall an administrator configured on purpose.

#[cfg(windows)]
use std::path::PathBuf;

#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(not(any(windows, target_os = "linux")))]
mod unsupported;

#[cfg(windows)]
pub use windows::inspect;

#[cfg(target_os = "linux")]
pub use linux::inspect;

#[cfg(not(any(windows, target_os = "linux")))]
pub use unsupported::inspect;

/// What we can say about inbound reachability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundReport {
    pub platform: &'static str,
    /// Is a host firewall active at all?
    pub active: bool,
    /// Is there an allow rule covering this listener?
    ///
    /// `None` means *not determined* — we either did not look or could not. It must
    /// never stand in for `false`: reporting a rule as missing when nothing checked
    /// it would be inventing a fact.
    pub allowed: Option<bool>,
    /// A command that would open it, runnable as written. This program never runs it.
    pub command: String,
    /// How that command has to be run, in the platform's own terms.
    pub how: &'static str,
}

impl InboundReport {
    /// The startup warning, or `None` when there is nothing worth saying: no
    /// firewall, or one that already covers this listener.
    pub fn warning(&self) -> Option<String> {
        if !self.active {
            return None;
        }
        let lead = match self.allowed {
            Some(true) => return None,
            Some(false) => {
                "a firewall is active and no inbound rule covers this listener, so other \
                 devices may not be able to reach it"
            }
            None => {
                "a firewall is active, and whether an inbound rule covers this listener \
                 could not be determined; if other devices cannot reach it"
            }
        };
        Some(format!(
            "{lead}. To allow it:\n  {}\n  ({})",
            self.command, self.how
        ))
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "platform": self.platform,
            "active": self.active,
            "allowed": self.allowed,
            "command": self.command,
            "how": self.how,
        })
    }
}

/// Run a program and return its combined output, or `None` if it could not be run.
///
/// A missing tool is not an error worth surfacing: it only means this check has
/// nothing to say, which `Option` already expresses.
pub(crate) fn run(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Some(text)
}

/// The leading value of a `Label:   value` line, if the line carries that label.
pub(crate) fn field<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    line.strip_prefix(label).map(str::trim)
}

/// This executable, as a firewall rule would spell it.
///
/// Windows only, like its single caller: Windows rules are scoped to a program and
/// Linux rules to a port. It is gated rather than left unused, so that it is not
/// compiled into a binary that cannot use it — which is also what a `-D warnings`
/// build on Linux rejects.
#[cfg(windows)]
pub(crate) fn program_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("landrop-cli"))
}
