<!-- For changes to the Rust port (thor-rs/). For the C# tool, describe accordingly. -->

## What this changes

<!-- A short description of the change and why. -->

## Checklist (Rust port)

- [ ] Added/updated tests first (TDD); the suite runs without a device
- [ ] `cargo fmt --all --check` is clean
- [ ] `cargo clippy --all-targets -- -D warnings` is clean
- [ ] `cargo test --all` passes
- [ ] Any new destructive operation is gated (`--execute` + typed confirmation, non-interactive
      stdin refused without `--yes`)
- [ ] Anything not verified on real hardware is marked as such in code and docs
- [ ] Docs updated (README / `docs/port/` roadmap) if behavior changed

## Tested on

<!-- Real hardware (model + mode), or mock/synthetic only. Be specific. -->
