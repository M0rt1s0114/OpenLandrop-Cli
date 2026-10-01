// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! Windows firewall inspection.
//!
//! This is the platform where the problem is silent. When a program first listens,
//! Windows offers to allow it; dismissing that prompt — or never being shown one,
//! as happens for a program started without an interactive session — leaves a rule
//! that blocks it, and it is never asked again. The receiver then accepts nothing,
//! forever, with no error to explain why.
//!
//! Both queries below run without elevation, which is what makes checking from a
//! CLI worthwhile at all.

use super::{InboundReport, field, program_path, run};

/// The rule name used if we ever hand someone a command to add one; the desktop
/// app uses its own application name for the same purpose.
const RULE_NAME: &str = "landrop-cli";

pub fn inspect(_port: u16) -> Option<InboundReport> {
    let state = run("netsh", &["advfirewall", "show", "allprofiles", "state"])?;
    let active = any_profile_active(&state);

    // Only scan the rule table when a firewall is actually on: the dump runs to a
    // few thousand lines, and with the firewall off there is nothing it can tell us.
    let allowed = if active {
        find_allow_rule(&rule_dump()?, &program_path())
    } else {
        None
    };

    let program = program_path();
    Some(InboundReport {
        platform: "windows",
        active,
        allowed,
        command: format!(
            "netsh advfirewall firewall add rule name=\"{RULE_NAME}\" dir=in \
             action=allow program=\"{}\" enable=yes",
            program.display()
        ),
        how: "needs an elevated prompt; this program never runs it for you",
    })
}

/// `netsh advfirewall show allprofiles state` prints one `State  ON|OFF` line per
/// profile. The label stays English on a localized Windows — only rule names and
/// descriptions are translated — which is what makes this parseable.
fn any_profile_active(output: &str) -> bool {
    output.lines().any(|line| {
        let mut words = line.split_whitespace();
        matches!(words.next(), Some("State")) && words.next() == Some("ON")
    })
}

fn rule_dump() -> Option<String> {
    run(
        "netsh",
        &[
            "advfirewall",
            "firewall",
            "show",
            "rule",
            "name=all",
            "dir=in",
        ],
    )
}

/// Is there an enabled inbound rule that allows this exact program?
///
/// Returns `None` when the dump could not be read, so that "we could not tell" is
/// never reported as "there is no rule".
fn find_allow_rule(dump: &str, program: &std::path::Path) -> Option<bool> {
    let wanted = program.to_string_lossy().to_lowercase();
    let mut found_program = false;
    let mut enabled = false;
    let mut allow = false;

    for line in dump.lines() {
        let line = line.trim();
        if line.starts_with("Rule Name:") {
            // A rule's fields are all above the next `Rule Name:`, so this is where
            // the previous one has to be judged.
            if found_program && enabled && allow {
                return Some(true);
            }
            found_program = false;
            enabled = false;
            allow = false;
        } else if let Some(value) = field(line, "Program:") {
            // A rule may list several programs; any of them matching counts.
            found_program |= value.to_lowercase() == wanted;
        } else if let Some(value) = field(line, "Enabled:") {
            enabled = value.eq_ignore_ascii_case("yes");
        } else if let Some(value) = field(line, "Action:") {
            allow = value.eq_ignore_ascii_case("allow");
        }
    }
    Some(found_program && enabled && allow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn profile_state_is_read_from_any_profile() {
        let all_off =
            "Domain Profile Settings:\n  State    OFF\nPrivate Profile Settings:\n  State    OFF\n";
        assert!(!any_profile_active(all_off));

        let one_on =
            "Domain Profile Settings:\n  State    OFF\nPrivate Profile Settings:\n  State    ON\n";
        assert!(any_profile_active(one_on));

        assert!(
            !any_profile_active(""),
            "empty output means no answer, not ON"
        );
    }

    #[test]
    fn a_rule_counts_only_when_it_is_enabled_allowing_and_ours() {
        let ours = r"C:\tools\landrop-cli.exe";
        let base = |action: &str, enabled: &str, program: &str| {
            format!(
                "Rule Name:                            landrop-cli\n\
                 ----------------------------------------------------------------------\n\
                 Enabled:                              {enabled}\n\
                 Direction:                            In\n\
                 Program:                              {program}\n\
                 Action:                               {action}\n"
            )
        };

        assert_eq!(
            find_allow_rule(&base("Allow", "Yes", ours), Path::new(ours)),
            Some(true)
        );
        // A block rule for us is exactly the silent trap this check exists for.
        assert_eq!(
            find_allow_rule(&base("Block", "Yes", ours), Path::new(ours)),
            Some(false)
        );
        assert_eq!(
            find_allow_rule(&base("Allow", "No", ours), Path::new(ours)),
            Some(false)
        );
        // Somebody else's rule says nothing about us.
        assert_eq!(
            find_allow_rule(
                &base("Allow", "Yes", r"C:\other\thing.exe"),
                Path::new(ours)
            ),
            Some(false)
        );
    }

    #[test]
    fn the_last_rule_in_a_dump_is_still_judged() {
        // There is no trailing `Rule Name:` to trigger the in-loop check.
        let ours = r"C:\tools\landrop-cli.exe";
        let dump = format!(
            "Rule Name:                            first\n\
             Enabled:                              Yes\n\
             Program:                              {ours}\n\
             Action:                               Allow\n"
        );
        assert_eq!(find_allow_rule(&dump, Path::new(ours)), Some(true));
    }

    #[test]
    fn program_paths_are_compared_case_insensitively() {
        let dump = "Rule Name: x\nEnabled: Yes\nProgram:                              C:\\Tools\\Landrop-CLI.EXE\nAction: Allow\n";
        assert_eq!(
            find_allow_rule(dump, Path::new(r"c:\tools\landrop-cli.exe")),
            Some(true)
        );
    }
}
