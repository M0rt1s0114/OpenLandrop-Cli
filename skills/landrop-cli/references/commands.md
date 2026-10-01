# Command reference

Every command accepts `--json`. JSON keys are emitted in alphabetical order.

## Global flags

| Flag | Meaning |
|---|---|
| `--json` | Structured result on stdout, human output on stderr, no prompts |
| `-q`, `--quiet` | No banner, no progress bars; transfer logs are kept |
| `--config-dir <DIR>` | Alternate configuration directory (identity, settings, cache) |

`--json` is the only flag a non-interactive caller needs. Without it, `send` may open an interactive device picker and a confirmation prompt.

## Choosing a target

`--to` accepts three forms:

| Form | Example | Notes |
|---|---|---|
| Public key | `--to AkxBTkRST1AtRVhB...` | Preferred. 44 base64 characters, stable |
| Device name | `--to build-server` | Needs a previous `discover` (cached) or a live reply |
| `host:port` | `--to 192.168.1.42:44769` | Skips discovery entirely; the peer identity is not pinned |

Several targets may be comma-separated (`--to a,b,c`). They are resolved up front — one bad name stops the run before anything is sent — and then transferred one after another, each waiting for its own acceptance. A target listed twice is sent to once.

---

## `discover`

Finds devices by UDP multicast, and remembers them for later name lookups.

| Flag | Default | Meaning |
|---|---|---|
| `--timeout <SECS>` | `3` | How long to listen for replies |
| `--loopback` | off | Also probe the loopback interface (same-host testing) |
| `--discovery-port <PORT>` | `52637` | Protocol port; only change it for isolated testing |

```json
{
  "count": 1,
  "devices": [
    {
      "name": "build-server",
      "type": "linux",
      "address": "192.168.1.42",
      "port": 44769,
      "public_key": "AkxBTkRST1AtRVhBTVBMRS1LRVktT05FLi4uLi4uLi4u",
      "discoverable": true
    }
  ]
}
```

- `type` is `windows`, `linux`, `macos` or `android`.
- `port: 0` means the device is not accepting transfers right now.
- Each run reports only the devices that answered it. Remembered devices persist in `devices` until `--forget`, so a device that has gone away can still be listed there while failing to answer a new `discover`.

## `send`

```console
landrop-cli send [OPTIONS] [FILES]...
```

| Flag | Default | Meaning |
|---|---|---|
| `-t`, `--to <TARGET>` | — | Target; comma-separate to reach several. Without it an interactive picker opens |
| `--stdin` | off | Read newline-separated paths from stdin instead of arguments |
| `--first` | off | Take the first device found instead of prompting |
| `--reply-timeout <SECS>` | `300` | How long to wait for the recipient to accept |
| `-y`, `--yes` | off | Skip the local "Proceed?" confirmation |
| `--wait-for-close` | off | Wait for the recipient to hang up after the last byte. The protocol has no completion acknowledgement, so this is the only signal that the receiver finished writing — best effort, and slower |

Directories are sent recursively; the receiver recreates the structure. Empty files are transferred as zero-byte files.

```json
{
  "status": "ok",
  "target": { "name": "build-server", "type": "linux", "address": "192.168.1.42",
              "port": 44769, "public_key": "AkxBTkRS..." },
  "verification_code": "844443",
  "files": [ { "filename": "report.pdf", "size": 8388608,
               "last_modified": 1759234000, "permissions": "644" } ],
  "total_bytes": 8388608,
  "sent_bytes": 8388608,
  "duration_seconds": 1.22,
  "throughput_bytes_per_second": 6876616
}
```

Sending to several targets uses the same per-target object, collected as `{ "status", "delivered", "failed", "targets": [ ... ] }`. The exit code is non-zero if any device failed, so read the per-device entries rather than the exit code alone.

## `receive`

Runs a listener in the foreground; it does not exit on its own. Connections are served concurrently, so a device waiting on its accept prompt does not hold up the next device.

| Flag | Default | Meaning |
|---|---|---|
| `--port <PORT>` | remembered | TCP port. A value is remembered for later runs; `--port 0` picks a free port each run and forgets it |
| `--dir <DIR>` | configured | Where received files are written |
| `--name <NAME>` | hostname | Advertised device name |
| `-y`, `--yes` | off | Accept every transfer, trusted or not |
| `--once` | off | Exit after one transfer |
| `--max <N>` | `0` | Exit after N transfers; `0` is unlimited |
| `--discovery-port <PORT>` | `52637` | Protocol port |

Acceptance rules without `--yes`:

| Sender | Behaviour |
|---|---|
| In the trust list | Accepted silently |
| Unknown, terminal present | Asks; then offers to remember the sender |
| Unknown, no terminal | **Declined** |

Per-transfer output:

```json
{ "status": "ok", "received": [ { "filename": "a.bin", "size": 8388608,
                                  "path": "D:\\LANDrop\\inbox\\a.bin" } ] }
```

## `text`

```console
landrop-cli text [OPTIONS] <TEXT>
```

Sends one short string. The peer acknowledges it; the reply carries no content.

```json
{ "status": "ok", "target": { ... }, "length": 27 }
```

## `devices`

Lists devices remembered from previous discoveries, or forgets one.

| Flag | Meaning |
|---|---|
| `--forget <NAME\|KEY>` | Remove a remembered device |

```json
{ "devices": [ { "name": "build-server", "type": "linux",
                 "public_key": "AkxBTkRS...", "last_address": "192.168.1.42",
                 "last_port": 44769, "last_seen": 1790769028 } ] }
```

## `trusted`

Manages the senders `receive` accepts without asking.

| Flag | Meaning |
|---|---|
| `--add <NAME\|KEY>` | Trust a sender; a name must be a remembered device |
| `--remove <NAME\|KEY>` | Stop trusting a sender |
| `--edit <NAME\|KEY> --name <NEW>` | Rename a trusted sender. The key, and so the trust itself, is unchanged. Refused when `NEW` already names a different entry, because names are how `--remove` and `--edit` select |

```json
{ "trusted_devices": [ { "name": "build-server", "public_key": "AkxBTkRS...",
                         "added_at": 1790769028 } ] }
```

## `identity`

```json
{
  "config_dir": "C:\\Users\\you\\AppData\\Roaming\\landrop-cli",
  "device_name": "desktop-pc",
  "device_type": "windows",
  "download_dir": "D:\\LANDrop",
  "public_key": "AkxBTkRST1AtRVhBTVBMRS1LRVktT05FLi4uLi4uLi4u",
  "secret_key": null,
  "version": "0.1.0-beta2"
}
```

`secret_key` is `null` unless `--show-secret` is passed. Use `version` to check which build is installed.

## `selftest`

End-to-end loopback transfer. Needs no peer, and is the cheapest way to confirm the binary works before blaming the network.

| Flag | Default | Meaning |
|---|---|---|
| `--size <BYTES>` | `5242880` | Payload size |

```json
{ "bytes": 262144, "duration_seconds": 0.0014943,
  "status": "ok", "throughput_bytes_per_second": 175429297 }
```

---

## Exit codes and errors

`0` on success, `1` on failure. On failure stdout still carries JSON:

```json
{ "error": "cannot stat /tmp/nope: No such file or directory (os error 2)",
  "status": "error" }
```

Free-text `error` is all that is available today; see [troubleshooting.md](troubleshooting.md) for matching it to a cause.

## Configuration

| Platform | Default directory |
|---|---|
| Windows | `%APPDATA%\landrop-cli` |
| Linux | `~/.config/landrop-cli` |
| Any | `--config-dir`, or `LANDROP_CLI_CONFIG_DIR` |

It holds `identity.json` (the identity key pair — keep it private) and `settings.json` (device name, download directory, device cache, trust list).
