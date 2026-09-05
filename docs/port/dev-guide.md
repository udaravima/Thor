# Thor (Rust) — Developer Guide

How to build, test, and extend the Rust port under [`thor-rs/`](../../thor-rs/). If you've
read the [engine docs](../) you already know the protocol; this is about the code.

## Layout

```
thor-rs/
  Cargo.toml            workspace
  thor-core/            the engine (library) — no UI
    src/pit.rs          PIT parser + field mappers
    src/proto.rs        1024-byte command packet builder + LE reads
    src/odin.rs         failure decode + Odin<T> session (handshake/begin/dump)
    src/flash.rs        flash-sequence planner (plan_flash)
    src/transport.rs    the Transport trait (the cross-platform seam) + UsbError
    src/backend.rs      nusb implementation of Transport (list/open/claim/bulk)
    tests/              golden tests against dev_files/ vectors
  thor-cli/             the binary (`thor`) — read-only subcommands for now
```

The one rule that keeps this clean: **everything above the USB metal talks to the
`Transport` trait, never to nusb directly.** `odin.rs` is generic over `Transport`, so it's
driven by a scripted mock in tests and by `NusbTransport` in production.

## Build & test

```sh
cd thor-rs
cargo build              # build everything
cargo test               # run all tests (no device needed — uses a mock + fixtures)
cargo clippy --all-targets   # lint; the tree is kept warning-clean
cargo run -p thor-cli -- list          # list connected Samsung devices
```

Tests need **no hardware**: the PIT parser/mappers run against `dev_files/sample-pit.pit`,
and the Odin session runs against a scripted `MockTransport` that records writes and replays
canned replies. Keep it that way — pure logic must stay device-independent and test-first.

## Running against a real device

1. Put the phone in **download mode** (power off; Vol-Down + Power + connect USB, or the
   model-specific combo; newer models need *Maintenance Mode* enabled first).
2. USB access to `/dev/bus/usb` needs permission. Either run as root, or add a udev rule:
   ```
   # /etc/udev/rules.d/51-android.rules
   SUBSYSTEM=="usb", ATTR{idVendor}=="04e8", MODE="0666", GROUP="plugdev"
   ```
   then `sudo udevadm control --reload && sudo udevadm trigger`.
3. If `cdc_acm` grabs the device, the backend detaches it automatically
   (`detach_and_claim_interface`). You can also blacklist it (see the engine docs).
4. Read-only commands:
   ```sh
   cargo run -p thor-cli -- dump-pit out.pit    # dump the partition table
   cargo run -p thor-cli -- print-pit           # dump + pretty-print live
   cargo run -p thor-cli -- print-pit file.pit  # pretty-print an existing file
   ```
   Set `THOR_DEBUG=1` to print the chosen interface/endpoints.

> **A connection can't be reused.** After any attempt (even a failed one), the bootloader
> stays past the handshake, so a second connect will time out on `ODIN`/`LOKE`. **Reboot the
> device back into download mode between attempts** — this is a device constraint, noted in
> the original Thor README too.

## Gotchas learned the hard way

- **nusb IN transfers must request a multiple of the endpoint's max packet size** (512 for
  high-speed bulk), or you get `TransferError::InvalidArgument`. `NusbTransport::bulk_read`
  rounds the request up to `max_packet_size` and trims the result to `completion.actual_len`
  (the device ends the transfer early with a short packet). This is the biggest behavioral
  difference from the libusb/C# model — don't "simplify" it away.
- **OUT transfers** have no such rule; the buffer length is exactly what's sent.
- **Read length ≠ requested length.** Always use `actual_len`, never the buffer's capacity.

## Adding a platform backend (Windows/macOS)

nusb is cross-platform, so in principle `NusbTransport` already compiles elsewhere; the work
is verifying `detach_and_claim_interface` semantics and endpoint discovery per-OS. If a
platform needs a different USB stack entirely, implement the `Transport` trait for it and
select it at runtime — nothing else in the engine changes. That's the whole point of the
seam.

## Contribution workflow

- **TDD.** No production logic without a failing test first. Pure modules (`pit`, `proto`,
  `flash`, `odin` session) are all tested against real vectors or a mock — match that bar.
- **Keep clippy clean** (`cargo clippy --all-targets`).
- **Commits** are made only on explicit request in this project; do the work, then ask.
- Behavior is pinned to the [engine docs](../) and, where possible, diffed against the
  reference C# (`dotnet` is available to run it as an oracle).
