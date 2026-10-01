// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! Linux firewall inspection.
//!
//! Deliberately a different question from the Windows one. Linux has no silent
//! block rule created for a new listener: if a firewall is filtering, an
//! administrator put that rule there and knows about it. So the useful thing to
//! report is *which* manager is active and how to open the port in its terms —
//! not a verdict on whether this particular binary is allowed, which is a
//! Windows-shaped question that does not apply here.
//!
//! `allowed` is therefore always `None` on this platform. That is not a gap; it is
//! the honest answer to a question this platform does not ask.
//!
//! Both managers need root to answer, so a non-root check reports nothing rather
//! than guessing.

use super::{InboundReport, field, run};

pub fn inspect(port: u16) -> Option<InboundReport> {
    if let Some(text) = run("ufw", &["status"])
        && let Some(active) = ufw_state(&text)
    {
        return Some(InboundReport {
            platform: "linux",
            active,
            allowed: None,
            command: format!("ufw allow {port}/tcp"),
            how: "needs root, for example through sudo; this program never runs it",
        });
    }

    if let Some(text) = run("firewall-cmd", &["--state"])
        && let Some(active) = firewalld_state(&text)
    {
        return Some(InboundReport {
            platform: "linux",
            active,
            allowed: None,
            command: format!(
                "firewall-cmd --permanent --add-port={port}/tcp && firewall-cmd --reload"
            ),
            how: "needs root, for example through sudo; this program never runs it",
        });
    }

    None
}

/// `ufw status` prints `Status: active` or `Status: inactive`, and an error line
/// instead when it is not run as root.
fn ufw_state(output: &str) -> Option<bool> {
    output.lines().find_map(|line| {
        match field(line.trim(), "Status:")? {
            "active" => Some(true),
            "inactive" => Some(false),
            // Anything else — including the permission error — is not an answer.
            _ => None,
        }
    })
}

/// `firewall-cmd --state` prints exactly `running` or `not running`.
fn firewalld_state(output: &str) -> Option<bool> {
    match output.trim() {
        "running" => Some(true),
        "not running" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ufw_reports_active_and_inactive() {
        assert_eq!(
            ufw_state("Status: active\n\nTo  Action  From\n"),
            Some(true)
        );
        assert_eq!(ufw_state("Status: inactive\n"), Some(false));
    }

    #[test]
    fn ufw_refuses_to_guess_when_it_cannot_answer() {
        // What ufw prints when run without root.
        let denied = "ERROR: You need to be root to run this script\n";
        assert_eq!(ufw_state(denied), None);
        assert_eq!(ufw_state(""), None, "no output is not the same as inactive");
    }

    #[test]
    fn firewalld_reads_its_two_answers_only() {
        assert_eq!(firewalld_state("running\n"), Some(true));
        assert_eq!(firewalld_state("not running\n"), Some(false));
        assert_eq!(firewalld_state("Authorization failed.\n"), None);
    }
}
