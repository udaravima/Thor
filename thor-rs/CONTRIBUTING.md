# Contributing to the Thor Rust port (`thor-rs`)

Thanks for helping improve the Rust reimplementation of Thor. This guide covers the Rust port
under `thor-rs/`; the original C# tool lives at the repository root and is a separate codebase.

## Ground rules

- **Test-driven.** The engine is built test-first, and the whole suite runs without a device.
  Add a failing test before the code that makes it pass. Protocol logic must be exercised with
  the scripted `MockTransport` (see `thor-core/src/odin.rs` tests), never against real hardware
  in the test suite.
- **Nothing destructive by default.** Every write path (`flash`, `factory-reset`, `erase`,
  `set-region`, `flash-pit`) is gated behind `--execute` and a typed confirmation, refuses a
  non-interactive stdin without `--yes`, and warns on brick-prone partitions. Keep it that way;
  new destructive operations must follow the same pattern.
- **Be honest about what's verified.** Mark anything not confirmed on real hardware as such, in
  both the code and the docs — see the `set-region` F8 note and the upload-mode address-framing
  note for the house style.

## Before you push

Run the same three checks CI runs (from `thor-rs/`):

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```

All three must be clean. CI runs the test suite on Linux, Windows, and macOS, so keep the code
cross-platform — everything platform-specific belongs behind the `Transport` trait
(`thor-core/src/transport.rs`), with the nusb implementation in `backend.rs`.

## Architecture at a glance

- `thor-core` — the UI-free engine: `pit`, `proto`, `odin`, `flash`, `archive`, `upload`,
  `dmesg`, `trace`, `transport`, `backend`. Fully unit-tested.
- `thor-cli` — the `thor` binary: subcommands plus the interactive `shell`.

See [`../docs/`](../docs/) for the protocol documentation the port is built against, and
[`../docs/port/`](../docs/port/) for the roadmap, dev guide, and kernel-experiment notes.

## Reporting bugs

Use the issue templates. For anything protocol-related, include a `--debug` wire trace
(`thor --debug <command>`, or `debug on` in the shell) and the exact device model and mode
(download vs upload).

## Licensing

Contributions are under the project's [MPL-2.0](../LICENSE) license, the same as the original
Thor. Preserve existing copyright and license notices.
