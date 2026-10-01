// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Mortis0114

//! `landrop-cli` binary entry point.
//!
//! Nothing but the entry point lives here: every command, and the argument
//! parsing that selects one, is in the library so integration tests can drive
//! it directly instead of only through a spawned process.

fn main() -> std::process::ExitCode {
    landrop_cli::cli::main()
}
