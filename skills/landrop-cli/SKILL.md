---
name: landrop-cli
description: Send files and directories to other devices on the local network with LANDrop v2, and list which devices are reachable. Use when the user wants to transfer a file to another computer, phone, or named LAN device.
whenToUse: The user asks to send, transfer, push or copy a file or directory to another device on the same local network; asks which devices are reachable; or wants to receive files that another device sends to this machine.
---

# LANDrop CLI

Moves files between devices on the same local network. The other end runs the stock LANDrop v2 app, so nothing has to be installed there.

Prerequisite: the `landrop-cli` binary must be on `PATH`. If the command is not found, say so instead of guessing at a path.

## Scope

This is a **file transfer** tool, not a message bus and not a remote shell.

- `text` sends one short string, acknowledged by the peer — see *Sending text*.
- There is no queue. If the peer is not running, the send fails immediately.
- Discovery is link-local multicast: it does not cross subnets or VPNs. Use `host:port` as the target when the peer is not on the same segment.
- Every command is a separate process. There is no session to keep open.

## Always pass --json

`--json` writes a structured result to stdout, sends everything human-facing to stderr, and disables all interactive prompts.

**On failure the exit code is non-zero and stdout still contains valid JSON.** Parse stdout regardless of the exit code; do not treat a non-zero exit as "no output".

## Two human steps, not one

Say this before starting, because otherwise the user thinks it has hung:

1. **The receiving device must be running LANDrop with receiving switched on.**
2. **The first transfer to a device stops and waits** for a person at that device to accept a prompt showing a 6-digit code.

**Do not assume the first one.** If the device has not just answered a scan, ask the user to confirm it is open before sending. A remembered device is not a running one, and "last seen four hours ago" says nothing about now. A device with its app closed and a device on a broken network fail in exactly the same way, so an unanswered scan is not evidence either way — ask.

## Choosing a target

A device is chosen by a name its user set, so resolve it deliberately rather than taking the first one that appears.

### 1. Offer what is already known

```console
$ landrop-cli devices --json
{ "devices": [ { "name": "pixel-phone", "type": "android",
                 "public_key": "AkxBTkRST1AtRVhBTVBMRS1LRVktT05FLi4uLi4uLi4u", "last_seen": 1790769028 } ] }
```

These are devices an earlier discovery found, so offering them costs no network round trip and no wait: they are the user's own devices, already named the way LANDrop shows them. Present each as **name plus type**, with how long ago it was last seen — a remembered device may be long gone.

Always give the user a way to say "not in this list". If they name several devices, that is fine; see *Sending to more than one device*.

If the list is empty, go straight to discovery.

### 2. Discovery, when the device is not in the list

A scan only finds devices that are running LANDrop and accepting. Scanning while the app is closed finds nothing and looks like a network fault, so **ask first**:

> Please make sure LANDrop is open on the device and that receiving is switched on, then tell me.

Ask even when it looks obvious. People who use LANDrop daily still leave it closed, and a scan against a closed app is indistinguishable from a broken network.

Once they confirm:

```console
$ landrop-cli discover --json --timeout 5
{
  "count": 1,
  "devices": [
    { "name": "pixel-phone", "type": "android", "address": "192.168.1.77",
      "port": 44769, "public_key": "AkxBTkRST1AtRVhBTVBMRS1LRVktT05FLi4uLi4uLi4u",
      "discoverable": true }
  ]
}
```

Then ask again, listing what was found, and again allowing several. Drop any device whose `port` is `0`: that one is not accepting transfers right now.

**If nothing answers, do not simply retry.** Report the three likely causes and ask which to check:

- the two devices are not on the same network (multicast does not cross subnets or VPNs);
- LANDrop is not running on the other device;
- LANDrop is running but receiving is switched off.

Then offer to scan again. Never loop on a scan silently.

### 3. Address the target by public key

Prefer `public_key` over the name. Names are user-chosen: two devices can share one, and a name changes when the user renames the device.

A name works too, and `host:port` skips discovery entirely, but prefer the key when the device came from discovery.

A peer picks a new listening port on every run, so an address remembered from an earlier discovery can be stale. Addressing by key or name survives that: if the remembered address does not answer, the CLI re-runs discovery and retries once. An explicit `host:port` is never re-resolved, because that would mean sending somewhere you did not name.

If the device the user asked for is not in the list, say so. Do not silently send to a different device.

## Sending

```console
$ landrop-cli send ./report.pdf ./logs/ --to <public_key> --json --reply-timeout 30
```

- Several paths are accepted, and directories are sent recursively with their structure preserved.
- A path list can come from stdin: `find . -name '*.log' | landrop-cli send --stdin --to <key> --json`
- **Always pass `--reply-timeout`.** Without it the default is 300 seconds, and a pending human confirmation can block the command for that long.

### Sending to more than one device

Comma-separate the targets, and one command reaches them all:

```console
$ landrop-cli send ./report.pdf --to <key-a>,<key-b> --json --reply-timeout 30
```

They are still **separate transfers, run one after another** — a transfer to one device cannot carry another's acceptance — so:

- every device asks its own person to accept, and each can block on its own;
- keep `--reply-timeout` short, so one unattended device does not hold up the rest;
- a device that fails does not stop the others. The command reports every result and exits non-zero if any of them failed, so read the per-device entries rather than the exit code alone.

Naming the same device twice sends to it once. Every target is resolved before anything is sent, so a typo in one name stops the whole run instead of letting the earlier devices receive a partial batch.

With a single target the result is one object, as below. With several it is `{ "status", "delivered", "failed", "targets": [ ... ] }`.

### Check the result

```json
{
  "status": "ok",
  "target": { "name": "build-server", "address": "192.168.1.42", "port": 44769 },
  "verification_code": "844443",
  "files": [ { "filename": "report.pdf", "size": 8388608 } ],
  "total_bytes": 8388608,
  "sent_bytes": 8388608,
  "duration_seconds": 1.22,
  "throughput_bytes_per_second": 6876616
}
```

`sent_bytes` must equal `total_bytes`. Report both, per device.

**`sent_bytes` proves the sending side was complete, never that the recipient wrote anything.** The protocol has no completion acknowledgement, so a send returns when the bytes reach the OS — which is why the sender's progress bar can finish while the recipient is still going. Do not tell the user the file "arrived" on this evidence; say it was sent. Add `--wait-for-close` when the difference matters: it waits for the recipient to hang up and then reports `peer_closed` (`true` hung up, `false` the wait expired, `null` not asked for). Even `true` is best effort — nothing obliges a receiver to close.

## Sending text

```console
$ landrop-cli text 'build finished' --to <public_key> --json
```

One short string, with the target chosen the same way as for a send. No file list, no
`--stdin`, and no `--reply-timeout` — that flag belongs to `send` and does not exist
here, so the wait is whatever the client defaults to.

```json
{ "status": "ok", "length": 14,
  "target": { "name": "build-server", "type": "linux",
              "address": "192.168.1.42", "port": 44769 } }
```

`length` is the character count of what was sent.

**Unlike a file transfer, this one is acknowledged.** The peer replies, and the command
waits for that reply before returning. So `status: "ok"` here means the peer received
it — not merely that this side finished sending. That is the opposite of `send`, where
nothing confirms the far end wrote anything. Read the acknowledgement rule for
whichever command you are running; they do not carry across.

## When a send times out

A timeout with `sent_bytes: 0` usually means *the peer is waiting for a human*, not that the network is broken.

- Say that, and ask the user to accept on that device.
- The code on the device must equal `verification_code`. That is what to compare if the user asks.
- After a device accepts once and is trusted, later transfers are silent.

**Never retry a timed-out send in a loop** — each attempt queues another prompt on the peer's screen. One retry, after the user confirms they have accepted, is enough.

## Receiving

`receive` runs in the foreground as a listener and does not exit:

```console
$ landrop-cli receive --json --dir ~/Downloads --once
```

It accepts from trusted senders silently, declines unknown senders when there is no terminal to confirm at, and `--yes` overrides that. Prefer `--once` or `--max N` so the command terminates.

## Asking the user

This skill needs an answer at four points. Use whatever question mechanism your harness provides — a picker, a prompt, or simply listing the options and stopping for a reply.

| When | Ask |
|---|---|
| Before scanning, **or before sending to any device whose liveness is unknown** | "Please make sure LANDrop is open and receiving is switched on, then tell me." |
| Target not in the remembered list | Present the discovered devices; let them pick one or several |
| Before sending | Confirm the file list and the target — a send is not undoable |
| After a timeout | "Is the device showing an accept prompt? If so, accept it and tell me." |

## Reference

- [`references/commands.md`](references/commands.md) — every command, flag and JSON field
- [`references/troubleshooting.md`](references/troubleshooting.md) — error text to cause to next action
