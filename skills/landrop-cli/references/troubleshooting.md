# Troubleshooting

Errors are free text today, so match on the phrases below. The command's `stderr` carries the same text; `stdout` carries the JSON `error` field.

## Nothing found by discovery

| Error text | Cause | What to do |
|---|---|---|
| `no LANDrop devices answered discovery` | No peer announced itself | Check the peer runs LANDrop v2 and is on the same L2 network. Multicast does not cross subnets or VPNs. |
| `no device matched "<x>" and discovery found nothing` | As above, plus the name/key matched nothing | Re-run `discover`; if the peer is not on this segment, use `--to host:port`. |
| `no device matched "<x>". Devices found:` | Discovery worked, the target did not match | Use one of the listed keys. Names can differ from what the user expects. |
| `no known device matches "<x>"` (from `trusted --add`) | The name is not in the device cache | Run `discover` first, or pass the 44-character public key. |

## The device was found but cannot receive

| Error text | Cause | What to do |
|---|---|---|
| `is not accepting transfers (its discovery reply advertises port 0)` | The peer has receiving switched off | Ask the user to enable receiving in LANDrop on that device. |
| `could not connect to <addr>` | Nothing listening there | If the target was addressed by name or key, the CLI has already re-run discovery once and the error says so. If it was an explicit `host:port`, re-run `discover`: the peer picks a new port on every run. |
| `the recipient does not support receiving files` | The peer replied with an unexpected message type | Usually a version mismatch, or the port belongs to something that is not LANDrop. |

## The send hung or timed out

| Error text | Cause | What to do |
|---|---|---|
| `no data reached <device>. If the device is still showing an Accept prompt, someone has to confirm it there` | **A human has not accepted yet.** This is not a network fault | Tell the user to accept on that device, then retry once. Do not loop. |
| `reading record: ... timed out` | The peer stopped responding mid-transfer | Check the peer is still running; retry once. |
| `the transfer to <device> was interrupted` | The connection dropped after data started flowing | Retry once. If it repeats, the link is the suspect. |

## The transfer was refused

| Error text | Cause | What to do |
|---|---|---|
| `the recipient declined the transfer` | A human pressed No on the peer | Ask the user whether they meant to. Do not retry automatically. |
| `the recipient closed the connection` | The peer quit or the prompt was dismissed | Retry once after confirming the peer is running. |

## Identity problems

| Error text | Cause | What to do |
|---|---|---|
| `peer identity mismatch: expected <a>, got <b>` | The device answering is not the one that was discovered | Re-run `discover`. Something else may be on that address now. |
| `handshake failed: the peer's ephemeral key signature did not verify` | The peer is not speaking LANDrop v2 | Confirm the target is really a LANDrop device. |

## Local problems, before any network traffic

| Error text | Cause | What to do |
|---|---|---|
| `cannot stat <path>` | The path does not exist | Check the path. Paths are relative to the working directory of the CLI process. |
| `nothing to send: no regular files found in the given paths` | The path held no regular files | The directory may be empty. |
| `no files to send: pass paths as arguments, or use --stdin to pipe them in` | No paths were given and stdin was a terminal | Pass paths, or pipe them in. |
| `N devices found; specify one with --to` | Several devices and no terminal to choose with | Pass `--to`. |

## Receiving

| Symptom | Cause | What to do |
|---|---|---|
| A sender is declined with `not in the trust list and no terminal to confirm` | `receive` has no terminal to ask at | Add the sender with `trusted --add <key>`, or run `receive --yes` if that is acceptable. |
| `could not advertise on UDP 52637` | The LANDrop desktop app on the same host already holds that port | The TCP listener still works. Add this machine to the app by IP and port, or run `receive` on another host. See the "Usage" reference shipped with the release. |
| A transfer is refused for an unusual filename | Received names containing `..`, an absolute path or `:` are rejected | This is deliberate. The sender has to rename the file. |

## Two things that are not errors

1. **A 6-digit `verification_code` in the output.** It is a man-in-the-middle check for the human to compare, not a failure. Quote it when asking the user to confirm.
2. **The first transfer to a new device stopping before any bytes move.** That is the peer waiting for a person. See "The send hung or timed out" above.

## Before blaming the network

`receive` prints a `firewall` line on startup and reports the same thing as a `firewall` object in its final JSON. Read it before theorising:

- **`active: false`** — nothing is filtering, so a connection failure is elsewhere.
- **`active: true, allowed: true`** — an inbound rule already covers this listener.
- **`active: true, allowed: false`** — the one that matters. Other devices cannot reach this receiver, and `command` is the exact rule to add. **Do not run it yourself**: it needs elevation, and changing a machine's security configuration is the user's decision, not a side effect of transferring a file. Surface it and let them decide.
- **`allowed: null`** — *not determined*, which is not the same as "no rule". Say you could not tell, and offer the command.

The whole object is absent on platforms with nothing to say, which is also not the same as "no firewall".

A sender that reports **a timeout rather than a refusal** is seeing dropped packets, not a closed port. A firewall on the receiving device is the usual cause, and the receiver running there prints the rule to add.

```console
$ landrop-cli selftest --json
```

A passing `selftest` transfers a payload over loopback with no peer involved, which separates "this binary is broken" from "the network or the peer is the problem".
