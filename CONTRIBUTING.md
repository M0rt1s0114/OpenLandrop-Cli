# Contributing

Thanks for considering a contribution. This document covers the conventions the project follows.

## Commit messages

Commits follow [Conventional Commits](https://www.conventionalcommits.org/), in English.

```
<type>[optional scope]: <subject>
```

* The subject is at most 50 characters and written in the imperative mood ("add retry", not "added retry").
* The body, if any, is separated by a blank line and wrapped at 72 characters.
* Footer may contain `BREAKING CHANGE: <description>` or issue references.

| Type | Meaning |
|---|---|
| `feat` | A new feature |
| `fix` | A bug fix |
| `docs` | Documentation only |
| `style` | Formatting, no behaviour change |
| `refactor` | Neither a fix nor a feature |
| `perf` | Performance |
| `test` | Tests |
| `build` | Build system or dependencies |
| `ci` | CI configuration |
| `chore` | Anything else |
| `revert` | Revert, with `This reverts commit <hash>` in the footer |

Examples:

```
feat(discovery): retry broadcasts on packet loss
fix(wire): reject frames larger than the uint16 length field
docs(internals): document the connection setup trap
```

The first commit of a repository uses the `init` type, with the same `<type>: <subject>` shape:

```
init: LANDrop v2 terminal client in Rust
```

## Versioning and releases

Versions follow [Semantic Versioning](https://semver.org/). The version lives in `Cargo.toml`, and a release is triggered by pushing a tag of `v` followed by that exact version. Three channels are distinguished by the prerelease part of the version:

| Version | Meaning | Published as |
|---|---|---|
| `0.2.0-alpha1` | Work in progress: a feature is partially implemented, may be incomplete or carry serious bugs. **Local testing only.** | Nothing. CI is skipped and the release workflow refuses to run. |
| `0.2.0-beta1` | Feature complete and basically debugged, but not yet proven. Unexpected minor problems are likely. | GitHub **pre-release** |
| `0.2.0` | Considered stable. | Normal release, marked *latest* |

Pushing an alpha commit is allowed, but it does not consume CI: the `preflight` job reads the version and skips the remaining jobs. Pull requests are always checked regardless of the version, so contributions are never merged unverified.

### Cutting a release

```console
# 1. Set the version in Cargo.toml, for example 0.1.0-beta1
# 2. Add a CHANGELOG.md entry
# 3. Commit, then tag with a "v" prefix
git commit -am "chore(release): 0.1.0-beta1"
git tag v0.1.0-beta1
git push origin main --tags
```

The release workflow then:

1. verifies that the tag matches the `Cargo.toml` version exactly, and refuses to continue if it does not;
2. refuses to publish any `-alpha` version;
3. runs the test suite on Linux and Windows;
4. builds a release binary for each platform and runs `selftest` on it;
5. attaches both binaries to a release, marked as a pre-release when the version carries a prerelease part, and as *latest* otherwise.

## Development

```console
cargo build
cargo test
cargo run -- selftest
```

All three of these must pass before a change is proposed, along with the two lint gates that CI enforces:

```console
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
```

CI runs both on Linux and Windows, plus a release build on each.

## Code layout

| File | Responsibility |
|---|---|
| `src/crypto.rs` | Cryptographic primitives |
| `src/wire.rs` | Framing and the connection abstraction |
| `src/messages.rs` | The messages that cross the connection |
| `src/session.rs` | Client and server state machines |
| `src/discovery.rs` | UDP multicast discovery and interface enumeration |
| `src/transfer.rs` | Path expansion, filename sanitisation, file metadata |
| `src/config.rs` | Identity and settings persistence |
| `src/main.rs` | CLI definition and command implementations |

## Tests

Protocol-level behaviour is tested over real loopback TCP rather than with mocks, because the interesting failures in this codebase are wire-level: a wrong key derivation, a nonce desynchronisation, or a frame boundary error all produce self-consistent but non-interoperable behaviour that a mock would not catch.

When adding a test for a cryptographic primitive, prefer a known-answer vector over a round-trip assertion: a round trip passes even when both halves are wrong in the same way. `src/crypto.rs` has an example.

## Changing the protocol layer

Any change to `crypto.rs`, `wire.rs` or `session.rs` risks breaking interoperability with real LANDrop devices in ways the test suite cannot detect, because a self-consistent implementation passes its own tests. Before proposing such a change:

1. Re-read the part you are about to change, and the tests around it.
2. If possible, verify against a real device and note in the pull request what was tested and what was not.
3. Update the comments in those files if behaviour changed.

## Documentation

Documentation is maintained in both English and Chinese:

* `README.md` / `README.zh-CN.md`
* `docs/USAGE.md` / `docs/USAGE.zh-CN.md`

When changing one language, update the other in the same pull request. Each document links to its counterpart at the top.

## License

By contributing you agree that your contributions are licensed under GPL-3.0-or-later, the license of this project. Every source file carries an SPDX header; keep it in place when adding files.
