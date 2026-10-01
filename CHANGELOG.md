# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.2] - 2026-10-01

Documentation only: clearer wording in comments, test names and both READMEs. No binary changed, so the release assets are identical to 1.0.1.

## [1.0.1] - 2026-10-01

### Fixed

- A listening device was announced on only some of its networks. The responder joined the multicast group on every interface but never chose the interface an announcement should leave by, so the multicast went out by whatever the routing table preferred — on a machine with a VPN that is the VPN — while the subnet broadcast was already sent per interface. Each interface now sends from a socket whose only way out is that interface, and both the multicast and the broadcast leave by it.
- The interface list was read once at start-up and never again, so an adapter that reconnected was not announced on after that — silently, for as long as the process ran. It is re-read every round now, and multicast membership follows it: new interfaces are joined, interfaces that have gone away are left.

### Changed

- `discovery::DiscoverOptions` gains `advertise`, naming the device name, type and port to include in the seeker's own query packets. It is `None` by default, which is what a one-shot `discover` sends and what the wire format has a place for. A process that is listening for transfers should set it: a seeker that describes nothing is telling every listener that it is not there, so a device that is both asking and listening otherwise drops off their lists between announcements and returns with the next one.

## [1.0.0] - 2026-10-01

## 0.1.0-beta3 - 2026-10-01

### Added

- `send --to` accepts several comma-separated targets, so one invocation can reach a group. Targets are resolved before anything is sent, so a mistyped name stops the run instead of delivering a partial batch; each transfer still waits for its own acceptance, a device that fails does not strand the rest, and a device named twice is sent to once. The exit code is non-zero if any device failed, so the per-device entries are what to read.
- `trusted --edit <NAME|KEY> --name <NEW>` renames a trusted sender without touching the key, and so without touching the trust. Refused when the new name already belongs to a different entry, since names are how `--edit` and `--remove` select.
- `receive --port` is remembered for later runs. `--port 0` asks for a free port each run and forgets the pinned one.
- `send --wait-for-close` waits for the recipient to hang up after the last byte. The protocol has no completion acknowledgement, so a send normally returns as soon as the bytes are handed to the OS — which can be well before the recipient has written them, and is why a sender's progress bar can finish first. Waiting is the closest thing to "the files are on their disk", but nothing obliges a receiver to close, so it is off by default and best effort by construction. The result gains `peer_closed`: `true` when the peer hung up, `false` when the wait expired, `null` when not asked for.
- `receive` reports what it can tell about the local firewall, on the banner and as a `firewall` object in its result. On Windows it reads the profile state and looks for an enabled inbound allow rule covering this executable; on Linux it reports which manager is active. It never changes anything — the command that would open the port is handed to a person or an agent, with a note that it needs elevation. `allowed: null` means *not determined* and never means "no rule": reporting a rule missing when nothing looked for one would be inventing a fact. Each platform has its own module and neither is compiled into the other's binary.
- A connection that times out rather than being refused now says so. Silence means the packets are being dropped rather than rejected, and a firewall on that device is the usual cause — indistinguishable, from the sender, from a peer that is merely slow to start.

### Changed

- The record seal path encrypts in place instead of building a plaintext buffer and copying it twice more, which is 67% faster in isolation (876 to 1463 MiB/s). The wire bytes are unchanged, and that is checked rather than assumed: `the_wire_format_is_frozen` pins a single record by hex and a multi-record stream by hash, against vectors captured before the change. End-to-end throughput rises far less (553 to 591 MiB/s) because the receiver, not the sender, is what limits a transfer — so this is groundwork rather than a user-visible win on its own.
- `receive` serves connections concurrently, one thread each. Previously a single device — slow, or sitting on an accept prompt — blocked every other device from being served at all. The connection setup runs inside the worker too, so a peer that connects and stalls no longer holds up the accept loop.
- Concurrent transfers can no longer resolve the same destination filename. The name is now claimed on disk as it is chosen, which is what `unique_top_level` alone could not do: two transfers arriving together each saw the same name as free and overwrote one another.
- `--once` and `--max` count connections that were actually served rather than connections accepted, so a probe that connects and hangs up no longer consumes a slot, and the limit is checked before each accept rather than after, so the receiver no longer sits idle past its limit waiting for a connection to trigger the check.

### Fixed

- `receive --once` exited without ever listening. Checking the limit before every accept is what stops the receiver polling past it, but the condition was written as a flag rather than a count — and a flag is true on the first pass. The change that introduced it was covered by a `--max` test, which did not reach this path; there is now a `--once` test that fails on the old behaviour.

## 0.1.0-beta2 - 2026-09-30

### Fixed

- Sending to a device by public key or name no longer fails permanently when the address remembered from an earlier discovery has gone stale. A peer picks a fresh listening port every run, so a cached port stops working the moment the peer restarts — targeting by public key used to fail with `could not connect ... (os error 10061)` even though the device was online and discoverable. The address is now re-resolved by discovery and the connection retried once. An explicit `host:port` is still never re-resolved: the caller pinned that address, and re-resolving it would mean sending somewhere they did not name.

### Added

- `skills/landrop-cli`: an Agent Skill that teaches an agent to move files between LAN devices, with a full command reference and an error-to-action troubleshooting guide. It states the two things agents otherwise get wrong — that the first transfer to a device blocks on a human accepting a prompt there, and that this is a file transfer tool rather than a message bus.

### Changed

- The release profile now sets `panic = "abort"`, which a CLI has nothing to unwind for: the Windows binary drops from 1,821,696 to 1,308,672 bytes.

### Verified

- The stale-address recovery was reproduced against a real device: a deliberately corrupted cached port produced `did not answer at 192.168.1.42:1; asking the network again`, followed by the device's real address and a completed connection.

## 0.1.0-beta1 - 2026-09-30

First published version: the planned feature set is complete and is now awaiting testing.

### Added

- UDP multicast device discovery on port `52637`, including subnet broadcast fallback and per-interface probing.
- `send` command: files and directories, recursive with relative structure preserved, interactive or non-interactive device selection, progress output.
- `receive` command: long-running TCP listener with a per-sender trust list, running as a foreground process or a background service.
- `text` command for short text messages.
- `discover`, `devices`, `trusted`, `identity` and `selftest` commands.
- `--json` output mode for scripting, with structured results on stdout, human output on stderr, non-interactive behaviour and non-zero exit codes on failure.
- Paths accepted from the command line or piped via `--stdin`.
- Identity persistence (key pair) with `--config-dir` and `LANDROP_CLI_CONFIG_DIR` overrides.
- Filename sanitisation on receive: path traversal, absolute paths, control characters and `:` are rejected before the offer is accepted.
- The two interoperability traps that produce silent failures rather than errors are documented at the code that has to avoid them.

### Verified

- Interoperability with the LANDrop v2.7.2 desktop application: files transferred in both directions arrive byte-identical (`SHA-256`), including directories with non-ASCII and emoji names, zero-byte files and nested subdirectories.
- Session key derivation matches a known-answer vector byte for byte.
- Discovery interoperates with a second implementation in both directions.

[1.0.2]: https://github.com/M0rt1s0114/OpenLandrop-Cli/releases/tag/v1.0.2
