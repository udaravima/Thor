# Thor (Rust)

A from-scratch **Rust** reimplementation of [Thor](../README.md) — an open-source clone of
Samsung's **Odin** firmware flasher, which talks to Galaxy devices in **download mode** over
USB. This port keeps what makes Thor special (raw USB, no libusb) while adding cross-platform
support, a testable engine, and a scriptable CLI.

> **Status: read-only + dry-run validated on real hardware; live flash built and gated.**
> Every non-destructive operation is proven byte-for-byte against the reference C# tool.
> Writing firmware — the one destructive operation, whose failure mode is a bricked device —
> is now implemented for both a single partition image and a whole Odin `.tar`/`.tar.md5`
> (`thor flash --execute`), behind a dry-run default and a typed confirmation, but has **not**
> yet been fired at real hardware pending a safe target (a spare device or exact stock signed
> firmware).

## What it can do today

| Command | Description |
|---------|-------------|
| `thor list` | List connected Samsung devices |
| `thor dump-pit <out.pit> [--reboot\|--reboot-download\|--shutdown]` | Dump the partition table (PIT), then optionally reboot |
| `thor print-pit [file]` | Pretty-print a PIT — from a file, or dumped live |
| `thor tar-list <archive>` | List the images in an Odin `.tar` / `.tar.md5` |
| `thor flash-plan [--pit <pit>] <file> [partition]` | **Dry run** — show exactly what flashing would do (sequences, parts, sizes), writing nothing. Handles single images, whole archives, and `.lz4` |
| `thor flash [--execute] [--yes] [--reboot\|…] <file> [partition]` | Flash a single image **or a whole `.tar`/`.tar.md5`** to its partition(s). **Without `--execute`** it's a dry run (identical to `flash-plan`); **with `--execute`** it writes, after showing the plan and requiring a typed confirmation (the partition name for one image, `FLASH` for an archive). `.lz4` is decompressed on the fly |
| `thor reboot [normal\|download]` · `thor end` | Reboot / shut down the device |
| `thor shell` | **Interactive session** — connect once, run many commands |

**Why `shell` exists:** a Samsung download-mode connection can't be reused across program
runs (after the first handshake the bootloader won't handshake again), so a one-shot command
model needs a fresh reboot each time. `thor shell` connects once and runs everything —
`pit`, `dump-pit`, `flash-plan`, `reboot` — in a single session.

## Build & run

Requires a recent Rust toolchain. Currently **Linux only** (the nusb backend compiles for
Windows/macOS but hasn't been verified there yet).

```sh
cd thor-rs
cargo build --release          # binary at target/release/thor  (~788 KB)
cargo test                     # 57 tests, no device required
./target/release/thor list
```

**Device access:** talking to `/dev/bus/usb` is privileged. Either run as root, or add a
udev rule (recommended):

```
# /etc/udev/rules.d/51-android.rules
SUBSYSTEM=="usb", ATTR{idVendor}=="04e8", MODE="0666", GROUP="plugdev"
```

then `sudo udevadm control --reload && sudo udevadm trigger`. If the kernel's `cdc_acm`
driver grabs the device, the backend detaches it automatically.

Put the phone in **download mode** first (power off, then Vol-Down + Power while plugging in
USB, or the model-specific combo; some 2024+ models need *Maintenance Mode* enabled first).

## Example: dry-run a firmware flash (no device needed)

```sh
thor flash-plan --pit dev_files/sample-pit.pit AP_XXX.tar.md5
```

Matches every image in the archive to a PIT partition, resolves `.lz4` sizes from the frame
header, and prints the sequence/part plan for each — a safe way to see what a real flash
would do.

## How it's built

Two crates, mirroring the C# `Library` / `Shell` split:

- **`thor-core`** — the engine, UI-free and fully unit-tested:
  `pit` (partition table), `proto` (1024-byte command packets), `odin` (the ODIN⇄LOKE
  protocol + session), `flash` (the sequence/part planner), `archive` (tar + LZ4),
  `transport` (the trait that abstracts USB), `backend` (the nusb implementation).
- **`thor-cli`** — the `thor` binary (subcommands + the `shell` REPL).

The **`Transport` trait** is the only platform-specific seam: the entire protocol is generic
over it, driven by a scripted mock in tests and by real USB in production. A Windows/macOS
port is one trait implementation — nothing else changes.

See [`../docs/`](../docs/) for the protocol and module documentation this port is built
against, and [`../docs/port/`](../docs/port/) for the [dev guide](../docs/port/dev-guide.md),
[roadmap](../docs/port/roadmap.md), and [kernel experiments](../docs/port/experiments-kernel.md).

## Safety notes

- **Writing is off by default and hard to trigger by accident.** `thor flash` is a dry run
  unless you pass `--execute`; even then it prints the plan and makes you *type the partition
  name* to proceed (`--yes` skips that only for automation), refuses a non-interactive stdin,
  and warns loudly on the bootloader/critical partitions that hard-brick. Nothing writes on
  the read-only commands.
- **Signatures are enforced by the bootloader.** You can only flash officially-signed images
  for the exact model; a mismatch is rejected (`Auth`), not written as garbage.
- **No partition read exists** in the Odin protocol, so "dump a partition and flash it back"
  is not available as a safety net — a safe flash target must be real signed firmware.
- **Newest devices are locked.** Samsung disabled download mode on the Galaxy S26 / Z Fold 7
  (2026); this targets the large base of existing devices.

## Tested against

Development was validated on a live Qualcomm-based Samsung (bootloader v3). `thor dump-pit`
produced a PIT byte-identical to the reference C# Thor's dump, and `thor print-pit` matched
its output field-for-field.

## License

[Mozilla Public License 2.0](../LICENSE), like the original project.

## Credits

[TheAirBlow](https://github.com/theairblow) for Thor · [Benjamin-Dobell](https://github.com/Benjamin-Dobell)
for documenting the Odin protocol · this Rust port and its documentation.
