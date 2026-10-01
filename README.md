# OpenLandrop-Cli

[![CI](https://github.com/M0rt1s0114/OpenLandrop-Cli/actions/workflows/ci.yml/badge.svg)](https://github.com/M0rt1s0114/OpenLandrop-Cli/actions/workflows/ci.yml)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://www.rust-lang.org)

**English** | [简体中文](README.zh-CN.md)

An open-source client application for the **LANDrop v2** LAN transfer protocol, written in Rust.

It implements the same wire protocol as LANDrop v2, so it interoperates with existing LANDrop v2 devices without modifying, reinstalling or re-pairing them.

> **Note on the name.** This is an independent project. It is not affiliated with or endorsed by the owners of LANDrop. See [Trademarks](#trademarks).

---

## Status

| Area | State |
|---|---|
| Windows | Release binary built; exercised against the LANDrop v2.7.2 desktop app |
| Linux | Release binary built and smoke-tested in CI |
| Discovery | Verified in both directions with a second implementation |
| Key derivation | Known-answer vector in the test suite, checked byte-for-byte |
| File transfer | Verified byte-exact (`SHA-256`) in both directions — see [Verification](#verification) |
| Tests | 61 (`cargo test`), `cargo clippy -- -D warnings` clean, `cargo fmt` clean |

## Requirements

- Rust 1.88 or later (edition 2024, uses let-chains)
- No C toolchain and no system crypto libraries. Every dependency is pure Rust, so `cargo build` is the only build step on both Windows and Linux.

## Build

```console
cargo build --release        # target/release/landrop-cli[.exe]
cargo test                   # 61 tests
cargo run -- selftest        # end-to-end self check, needs no peer
```

Building natively works on both platforms; nothing outside the Rust toolchain is required.

## Usage

```console
# List devices on the local network
landrop-cli discover

# Send files or directories (without --to, an interactive picker is shown)
landrop-cli send ./report.pdf ~/Pictures/holiday/

# Send to a specific device by name, public key, or host:port
landrop-cli send f.zip --to build-server
landrop-cli send f.zip --to AkxBTkRST1AtRVhBTVBMRS1LRVktT05FLi4uLi4uLi4u
landrop-cli send f.zip --to 192.168.1.42:44769

# Read the list of paths from stdin
find . -name '*.log' | landrop-cli send --stdin

# Receive files
landrop-cli receive --dir ~/Downloads

# Send a short text message
landrop-cli text "build finished" --to build-server
```

### Commands

| Command | Purpose |
|---|---|
| `discover` | Find LANDrop v2 devices via UDP multicast |
| `send <paths...>` | Send files and/or directories (recursive, structure preserved) |
| `receive` | Run as a receiver other devices can send to |
| `text <text>` | Send a short text message |
| `devices` | List or forget devices remembered from earlier runs |
| `trusted` | Manage the senders accepted without prompting |
| `identity` | Show this client's identity, public key and configuration paths |
| `selftest` | End-to-end loopback check that needs no peer |

Run `landrop-cli <command> --help` for the full option list.

## Non-interactive use

`--json` writes a structured result to stdout, sends all human-facing output to stderr, and implies non-interactive mode (no pickers or confirmation prompts). On failure the exit code is non-zero and stdout still contains valid JSON.

```console
$ landrop-cli send ./a.bin --to 192.168.1.42:44769 --json
{
  "status": "ok",
  "target": {
    "name": "build-server", "type": "linux",
    "address": "192.168.1.42", "port": 44769,
    "public_key": "AkxBTkRST1AtRVhBTVBMRS1LRVktT05FLi4uLi4uLi4u"
  },
  "verification_code": "844443",
  "files": [
    { "filename": "a.bin", "size": 8388608,
      "last_modified": 1759234000, "permissions": "644" }
  ],
  "total_bytes": 8388608,
  "sent_bytes": 8388608,
  "duration_seconds": 1.22,
  "throughput_bytes_per_second": 6876616
}
```

## Identity and trust

The client's identity is a long-lived **key pair**, generated on first run and stored at:

| Platform | Location |
|---|---|
| Windows | `%APPDATA%\landrop-cli\identity.json` |
| Linux | `~/.config/landrop-cli/identity.json` |
| Any | override with `--config-dir <DIR>` or `LANDROP_CLI_CONFIG_DIR` |

The public key is how peers identify this client in their trust lists, so it must stay stable; a new key on every run would make every peer treat the client as a new device each time.

The client has its **own** identity and its **own** trust list, and shares neither with anything else on this machine. The consequence is that the first transfer to each receiving device shows a confirmation prompt there:

1. The device displays a prompt containing a 6-digit verification code. It must match the code printed by the CLI, otherwise the connection is being intercepted.
2. Accepting once completes the transfer.
3. The device then offers a one-time **Trust** action. Accepting it adds this client's public key to that device's trust list, after which transfers are silent.

### Receiver-side trust list

`receive` keeps its own trust list in `settings.json` inside the configuration directory:

| Sender | Behaviour of `receive` |
|---|---|
| In the trust list | Accepted silently |
| Not listed, terminal present | Prints the file list and verification code, asks for confirmation, then offers to remember the sender |
| Not listed, no terminal (pipe or service) | **Declined** |
| Not listed, `--yes` given | Accepted, but the trust list is not modified |

`--yes` and the trust list are separate concerns: the former accepts *this* transfer, the latter authorises *this sender* from now on. Nothing is added to the trust list without an explicit action.

When a sender is declined, its public key and the exact command to authorise it are written to stderr:

```
warning: declining dev-laptop (A0xBTkRST1AtRVhBTVBMRS1LRVktVFdPLi4uLi4uLi4u):
  not in the trust list and no terminal to confirm.
  to allow it:  landrop-cli trusted --add A0xBTkRST1AtRVhBTVBMRS1LRVktVFdPLi4uLi4uLi4u
```

### Configuration file

`settings.json` in the configuration directory. All fields are optional and unknown fields are ignored, so additions in a newer version do not break older ones.

```json
{
  "device_name": null,
  "download_dir": null,
  "listening_port": 0,
  "known_devices": [],
  "trusted_devices": [
    {
      "name": "build-server",
      "public_key": "AkxBTkRST1AtRVhBTVBMRS1LRVktT05FLi4uLi4uLi4u",
      "added_at": 1790769028
    }
  ]
}
```

## Running as a background service

`receive` is a long-running listener. Combined with the trust list it accepts transfers from authorised devices with no interaction:

```console
landrop-cli receive --quiet --dir /srv/landrop/inbox
```

`--quiet` suppresses the banner and progress bars (which would otherwise emit ANSI escapes into a service log) while keeping transfer logs. `Ctrl-C` performs a clean shutdown. See [`docs/USAGE.md`](docs/USAGE.md) for systemd and Windows Scheduled Task configurations.

> **Known constraint.** Device discovery uses UDP port **52637**, which the LANDrop desktop application also binds. While that application runs on the same host, this client cannot bind the port and so cannot advertise itself: it will not appear in other devices' discovery results. The TCP listener is unaffected. Workarounds are documented in [`docs/USAGE.md`](docs/USAGE.md#receive-cannot-use-port-52637).

## Verification

Measurements below were taken against **LANDrop v2.7.2**, on loopback TCP unless stated otherwise.

| Check | Method | Result |
|---|---|---|
| CLI → desktop app, single file | 8 MiB, 24 MiB and 64 MiB transfers | `SHA-256` identical at the destination |
| CLI → desktop app, directory | Tree containing a non-ASCII name, an emoji name, a name with spaces, a zero-byte file and a nested subdirectory | All 6 files `SHA-256` identical, structure preserved |
| CLI → desktop app, many files | 500 × 32 KiB | Complete |
| CLI → desktop app, text | Text message round trip | Acknowledged by the app |
| Interactive prompt | Receiving device's Accept + Trust prompt | Accepted manually; verification codes matched on both sides |
| Second sender → CLI receiver | 3 MiB from a second implementation | `SHA-256` identical |
| Trust list, untrusted sender | Non-interactive receiver | Declined, nothing written |
| Trust list, trusted sender | After `trusted --add` | Accepted silently, `SHA-256` identical |
| Discovery, both directions | Two implementations | Devices found in both directions |
| Key derivation | Fixed inputs, known-answer vector | Byte-identical |

**Not verified.** The following have not been exercised and should not be assumed:

- The desktop app sending to this client's receiver *on the same host* (blocked by the UDP 52637 constraint above).
- Throughput over a real LAN or Wi-Fi. All figures below are loopback, where the network is not the bottleneck.
- Any platform other than Windows, or any LANDrop version other than v2.
- Files larger than 4 GiB.

## Performance

Loopback TCP, same host, CLI sending to the LANDrop v2.7.2 desktop app:

| Case | Throughput |
|---|---|
| 8 MiB single file | ~100 MiB/s |
| 64 MiB single file | ~86 MiB/s |
| 500 × 32 KiB files | ~10 MiB/s |
| CLI → CLI (both Rust) | ~300 MiB/s |
| `selftest` (in-memory) | ~410 MiB/s |

Two observations from these numbers:

- Large-file throughput is limited by the receiver rather than by this client. On a real network the link is expected to be the bottleneck first.
- Many small files are substantially slower: the receiving side performs roughly six extra syscalls per file (`open`, `write`, `close`, `stat`, `utimes`, `chmod`), about 3 ms per file.

## Limitations

1. **LAN only.** The internet/WebRTC relay transport is not implemented.
2. **UDP 52637 is exclusive**, as described above.
3. **A CLI and the desktop app on one host cannot discover each other.** This client filters discovery by public key rather than by address, so two instances of it on one host can find each other; it cannot see the desktop app, and the desktop app cannot see it.
4. **Filename sanitisation is this client's own.** Received names are rejected before an offer is accepted if they contain path traversal (`..`), an absolute path or a `:`. Files that arrive through this client cannot land outside the download directory.
5. **The declared file size is authoritative.** A file that shrinks while it is being sent ends the transfer with an error rather than leaving the receiver waiting for bytes that will never come.

## Documentation

| Document | English | 简体中文 |
|---|---|---|
| README | [README.md](README.md) | [README.zh-CN.md](README.zh-CN.md) |
| Usage reference | [docs/USAGE.md](docs/USAGE.md) | [docs/USAGE.zh-CN.md](docs/USAGE.zh-CN.md) |

**[`skills/landrop-cli`](skills/landrop-cli/SKILL.md)** is an Agent Skill: it teaches a coding agent to drive this CLI — picking a target with the user rather than guessing, confirming the receiving device is ready, and reading a transfer's result for what it does and does not prove. It follows the cross-tool `.agents/skills` convention, so any harness that reads that layout can use it.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). Commits follow [Conventional Commits](https://www.conventionalcommits.org/).

## Acknowledgements

- **[LANDrop](https://landrop.app)** — thanks to the original authors for an excellent application. This project started from wanting a LANDrop program that also works on Linux machines with no desktop environment. If there is any concern about infringement, please open an issue and I will take the repository down.
- **[DeepSeek](https://www.deepseek.com)** — thanks to DeepSeek-V4.1 Flash for its substantial help and contribution to this work, which greatly increased my efficiency.

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE).

## Trademarks

"LANDrop" and any related marks belong to their respective owners. This project is an independent implementation that is compatible with LANDrop v2; it is not affiliated with or endorsed by them.
