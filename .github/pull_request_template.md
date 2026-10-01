## Summary

<!-- What changed and why. Link any related issue. -->

## Type of change

- [ ] `feat` — new feature
- [ ] `fix` — bug fix
- [ ] `docs` — documentation
- [ ] `refactor` / `perf` / `style` — no behaviour change
- [ ] `test` / `ci` / `build` / `chore`

## Checklist

- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --all-targets --locked -- -D warnings` passes
- [ ] `cargo test` passes
- [ ] `cargo run -- selftest` passes
- [ ] Both language versions updated, if documentation changed

## Interoperability

<!-- Did this touch src/crypto.rs, src/wire.rs or src/session.rs? If so, a self-consistent implementation would still pass its own tests while breaking against real devices. Describe what was verified against a real peer, and what was not. -->

- [ ] Not applicable (no protocol-layer change)
- [ ] Verified against a real LANDrop device — describe below
- [ ] Not verified against a real device — describe the risk below

## Notes for reviewers

<!-- Anything surprising, any tradeoff, anything you are unsure about. -->
