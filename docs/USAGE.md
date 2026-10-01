# Usage

Every command, every flag, and what to do when something does not work.
For installing and building, see the [README](../README.md).

## Synopsis

```
landrop-cli [OPTIONS] <COMMAND>
```

### Global options

| Option | Meaning |
|---|---|
| `--json` | Machine-readable JSON on stdout. Implies non-interactive: nothing will prompt |
| `-q`, `--quiet` | Background/service mode. No banner, no progress bars; warnings and errors still go to stderr |
| `--config-dir <DIR>` | Use an alternate configuration directory. Useful for testing and for running more than one identity |
| `-h`, `--help` | Help for the command |
| `-V`, `--version` | Version |

Every command accepts these, so they can go before or after the subcommand.

## Commands

| Command | What it does |
|---|---|
| [`discover`](#discover) | Find devices on the local network |
| [`send`](#send) | Send files and/or directories |
| [`receive`](#receive) | Receive files: run as a server others can send to |
| [`text`](#text) | Send a short text message |
| [`devices`](#devices) | List, look up or forget devices seen earlier |
| [`trusted`](#trusted) | Manage the senders `receive` accepts without asking |
| [`identity`](#identity) | Show this CLI's identity, configuration and paths |
| [`selftest`](#selftest) | Verify the installation end to end, with no network peer |

---

### `discover`

Find devices on the local network and remember them for later name lookups.

| Option | Default | Meaning |
|---|---|---|
| `--timeout <SECS>` | `3` | How long to listen for replies |
| `--loopback` | off | Include the loopback interface (same-host testing) |
| `--discovery-port <PORT>` | `52637` | Only change this for isolated testing |

```console
$ landrop-cli discover
$ landrop-cli discover --timeout 5 --json
```

Each run reports only the devices that answered it. Remembered devices persist
separately and are listed by [`devices`](#devices).

---

### `send`

```console
landrop-cli send [OPTIONS] [FILES]...
```

| Option | Default | Meaning |
|---|---|---|
| `-t`, `--to <TARGET>` | — | Where to send. Comma-separate to reach several devices. Without it an interactive picker opens |
| `--stdin` | off | Read newline-separated paths from stdin instead of the argument list |
| `--first` | off | Take the first device found instead of prompting |
| `--reply-timeout <SECS>` | `300` | How long to wait for the recipient to accept |
| `-y`, `--yes` | off | Skip the local "Proceed?" confirmation |
| `--wait-for-close` | off | Wait for the recipient to hang up after the last byte. See below |

`--to` accepts three forms:

| Form | Example | Notes |
|---|---|---|
| Public key | `--to AkxBTkRS...` | Most reliable. 44 base64 characters, stable across runs |
| Device name | `--to build-server` | Needs an earlier `discover`, or a live reply |
| `host:port` | `--to 192.168.1.42:44769` | Skips discovery entirely |

Directories are sent recursively and the receiver recreates the structure. Empty
files are transferred as zero-byte files.

**Several targets.** `--to a,b,c` reaches them all. Every target is resolved
before anything is sent, so a mistyped name stops the run rather than delivering
a partial batch. The transfers still run one after another, and each waits for
its own acceptance — so a device that fails does not strand the rest. A device
named twice is sent to once.

```console
$ landrop-cli send report.pdf --to build-server
$ landrop-cli send ./photos --to AkxBTkRST1AtRVhBTVBMRS1LRVktT05FLi4uLi4uLi4u
$ landrop-cli send a.zip b.zip --to laptop,phone --json
$ find . -name '*.log' | landrop-cli send --stdin --to build-server
```

**About `--wait-for-close`.** A send normally returns as soon as the bytes are
handed to the operating system, which can be well before the recipient has
written them to disk. That is why a sender's progress bar can finish first. This
flag waits for the recipient to close the connection instead, which is the
closest thing to "the files are on their disk". It is off by default because
nothing obliges a receiver to close, so it can only ever be best effort, and it
makes the command slower. The result gains `peer_closed`: `true` when the peer
hung up, `false` when the wait expired, `null` when the flag was not given.

---

### `receive`

Runs in the foreground until stopped. Connections are served concurrently, so a
device waiting on its accept prompt does not hold up the next device.

| Option | Default | Meaning |
|---|---|---|
| `--port <PORT>` | remembered | TCP port. A value is remembered for later runs; `--port 0` picks a free port each run and forgets it |
| `--dir <DIR>` | configured | Where received files are written |
| `--name <NAME>` | hostname | Advertised device name |
| `-y`, `--yes` | off | Accept every transfer without asking |
| `--discovery-port <PORT>` | `52637` | Only change this for isolated testing |
| `--once` | off | Exit after a single transfer |
| `--max <N>` | `0` (unlimited) | Exit after this many transfers |

```console
$ landrop-cli receive
$ landrop-cli receive --dir ~/inbox --yes
$ landrop-cli receive --port 41234 --max 3
```

Without `--yes`, transfers from senders that are not in the trust list ask for
confirmation. Started with no terminal — as a service — those are declined
instead, so add the senders you expect to the trust list first.

---

### `text`

```console
landrop-cli text [OPTIONS] <TEXT>
```

| Option | Meaning |
|---|---|
| `-t`, `--to <TARGET>` | Same forms as `send`. Without it an interactive picker opens |
| `--first` | Take the first device found instead of prompting |

```console
$ landrop-cli text 'build finished' --to build-server
```

---

### `devices`

Devices remembered from earlier runs, for name lookups by `--to`.

| Option | Meaning |
|---|---|
| `--forget <NAME\|KEY>` | Remove a remembered device by name or public key |

```console
$ landrop-cli devices
$ landrop-cli devices --forget old-laptop
```

---

### `trusted`

The senders `receive` accepts without asking.

| Option | Meaning |
|---|---|
| `--add <NAME\|KEY>` | Trust a sender. A name must resolve to a device seen before |
| `--remove <NAME\|KEY>` | Stop trusting a sender |
| `--edit <NAME\|KEY> --name <NEW>` | Rename a trusted sender. The key, and so the trust itself, is unchanged. Refused if `NEW` already names a different entry |

```console
$ landrop-cli trusted
$ landrop-cli trusted --add build-server
$ landrop-cli trusted --edit build-server --name workshop-pc
$ landrop-cli trusted --remove workshop-pc
```

---

### `identity`

| Option | Meaning |
|---|---|
| `--show-secret` | Also print the secret key. Keep it private |

```console
$ landrop-cli identity
```

---

### `selftest`

Transfers a payload over loopback with no peer involved. If this fails, the
problem is local.

| Option | Default | Meaning |
|---|---|---|
| `--size <BYTES>` | `5242880` | Payload size |

```console
$ landrop-cli selftest
```

---

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Success. With `--json`, `status` is `ok` |
| `1` | Failure. With `--json`, `status` is `error` and `error` describes it |
| `2` | The command line was wrong — an unknown flag or a missing argument |

With several `--to` targets, the exit code is non-zero if any device failed, so
read the per-device entries rather than the exit code alone.

## Where things are stored

| Platform | Directory |
|---|---|
| Linux | `~/.config/landrop-cli` |
| Windows | `%APPDATA%\landrop-cli` |
| macOS | `~/Library/Application Support/landrop-cli` |

`landrop-cli identity` prints the exact path in use. Override it with
`--config-dir`. The directory holds:

| File | Contents |
|---|---|
| `identity.json` | This CLI's identity key. Keep it private — it is what other devices recognise |
| `settings.json` | Device name, download directory, listening port, trust list |
| `devices.json` | Devices remembered from earlier runs |

## Running in the background

`--quiet` removes the banner and progress bars but keeps warnings and errors, so
it is the mode to use under a service manager.

```ini
# ~/.config/systemd/user/landrop-cli.service
[Unit]
Description=LANDrop CLI receiver

[Service]
ExecStart=%h/.cargo/bin/landrop-cli receive --yes --quiet --port 41234
Restart=on-failure

[Install]
WantedBy=default.target
```

```console
$ systemctl --user enable --now landrop-cli
```

On Windows, `nssm` or a scheduled task both work; the important part is passing
`--quiet` and an explicit `--port`.

## Troubleshooting

### `discover` finds nothing

- Make sure the other device has its app open and is accepting.
- Some networks block multicast between clients (guest Wi-Fi, and networks with
  client isolation). If a direct address works, multicast is what is blocked.
- The other device must be on the same subnet. Multicast does not cross routers
  by default.
- If you know the address, skip discovery: `--to 192.168.1.42:44769`.

### A device was found but nothing is received

- The transfer list only shows devices that are accepting. A device advertising
  port `0` has receiving switched off.
- A sender that waits and then reports a timeout, rather than an immediate
  refusal, is usually being blocked by a firewall on the *receiving* device.
  `receive` prints the rule to add on startup if it finds one missing.

### The first transfer to a new device stops before any bytes move

Someone has to accept the prompt on the receiving device. If nobody does, the
transfer gives up after `--reply-timeout` seconds.

### `receive` cannot use port 52637

The desktop app holds that port while it runs, so discovery from this CLI will
not work on the same machine at the same time. Transfers are unaffected: they
use a separate port, and `--to host:port` skips discovery completely. The
receiver warns when this happens and tells you the address to use.

### Sending a file while the desktop app is also running

They are separate installations with separate identities. Each keeps its own
configuration directory and trust list, so a device that trusts one does not
automatically trust the other.
