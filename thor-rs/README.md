# Thor (Rust)

A from-scratch **Rust** reimplementation of [Thor](../README.md) — an open-source clone of
Samsung's **Odin** firmware flasher, which talks to Galaxy devices in **download mode** over
USB. This port keeps what makes Thor special (raw USB, no libusb) while adding cross-platform
support, a testable engine, and a scriptable CLI.

> **Status: validated on real hardware, including live flashing.** Every read-only operation is
> proven byte-for-byte against the reference C# tool, and the **write path is now proven on a real
> device** — thor flashed TWRP to a Galaxy J2 (SM-J250Y) in download mode, sequence-for-sequence,
> and the phone booted it. The remaining destructive operations (whole-archive flash, factory
> reset, erase, T-Flash, region set) drive that same, now-exercised flash engine and stay behind a
> dry-run default plus a typed confirmation.

## What it can do today

| Command | Description |
|---------|-------------|
| `thor list` | List connected Samsung devices |
| `thor dump-pit <out.pit> [--reboot\|--reboot-download\|--shutdown]` | Dump the partition table (PIT), then optionally reboot |
| `thor print-pit [file]` | Pretty-print a PIT — from a file, or dumped live |
| `thor tar-list <archive>` | List the images in an Odin `.tar` / `.tar.md5` |
| `thor flash-plan [--pit <pit>] <file> [partition]` | **Dry run** — show exactly what flashing would do (sequences, parts, sizes), writing nothing. Handles single images, whole archives, and `.lz4` |
| `thor flash [--execute] [--yes] [--tflash] [--reboot\|…] <file> [partition]` | Flash a single image **or a whole `.tar`/`.tar.md5`** to its partition(s). **Without `--execute`** it's a dry run (identical to `flash-plan`); **with `--execute`** it writes, after showing the plan and requiring a typed confirmation (the partition name for one image, `FLASH` for an archive). `.lz4` is decompressed on the fly; `--tflash` targets an inserted microSD |
| `thor factory-reset --execute [--yes]` | Wipe `/data` (factory reset). Type `ERASE` to confirm |
| `thor erase <partition> --size <bytes> --execute [--yes]` | Zero-fill a partition. Type the partition name to confirm |
| `thor set-region <XAA> --execute [--yes]` | Set the CSC region code — **unverified** (shares an opcode with T-Flash in the original; may enable T-Flash instead). Type `YES` to confirm |
| `thor flash-pit <file.pit> --execute [--yes]` | **Repartition** the device from a PIT (validated first). The most brick-prone command — type `FLASHPIT` to confirm |
| `thor upload-probe` · `thor upload-dump <start> <end> <out>` · `thor upload-reboot` | **Upload mode (SUC):** list RAM regions and **dump memory** from a device in upload/ramdump mode — read-only. A RAM dump is a superset of "get the kernel log over USB" |
| `thor dmesg-carve <dumpfile>` · `thor upload-dmesg <start> <end>` | **Carve the kernel `printk` log** out of a RAM dump — offline from a file, or dump-and-carve live. The "USB → printk" payoff |
| `thor reboot [normal\|download]` · `thor end` | Reboot / shut down the device |
| `thor shell` | **Interactive session** — connect once, run many commands. Real line editing: Tab-completion, ↑/↓ history (saved to `~/.thor_history`), Ctrl-A/E/U/K/W, Ctrl-R search, Ctrl-C/Ctrl-D |

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
cargo test                     # 102 tests, no device required
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

## Seeing the protocol (debug trace)

Run any command with `--debug` (or `THOR_DEBUG=1`, or `debug on` in the shell) to trace every
USB bulk transfer to stderr — each outgoing command decoded to its Odin region/sub-command
name, with a hex preview:

```
→ BeginSession (region=0x64 sub=0x00) [1024B]   64 00 00 00 00 00 00 00 ff ff ff 7f …
← 8B   00 00 00 00 03 00 00 00
```

Handy for understanding the wire protocol or debugging a device that misbehaves.

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
[roadmap](../docs/port/roadmap.md), [kernel experiments](../docs/port/experiments-kernel.md)
(including the upload-mode SUC protocol), and the [Galaxy J2 (2018) unlock guide](../docs/port/device-j250y.md).

## Safety notes

- **Every destructive command is off by default and hard to trigger by accident.** `flash`,
  `factory-reset`, `erase`, and `set-region` all do nothing without `--execute`; even then they
  print what will happen and make you *type a confirmation word* (the partition name, `FLASH`,
  `ERASE`, or `YES`), refuse a non-interactive stdin unless `--yes`, and warn loudly on the
  bootloader/critical partitions that hard-brick. The read-only commands never write.
- **Signatures are enforced by the bootloader.** You can only flash officially-signed images
  for the exact model; a mismatch is rejected (`Auth`), not written as garbage.
- **Anti-rollback is enforced too.** Flashing firmware older than the level fused into the
  device fails with `SW REV CHECK FAIL` — you cannot downgrade past the rollback counter.
- **A factory reset can re-lock the bootloader.** On an unlocked device, wiping `/data` trips
  Samsung's *VaultKeeper*, which re-locks the bootloader until you complete setup online.
- **`set-region` is unverified.** In the reference tool it shares a command opcode with T-Flash
  enable (almost certainly a bug), so it may enable T-Flash rather than change the region. It's
  shipped with that warning, not as a trusted operation.
- **No partition read exists** in the Odin protocol itself — download mode is write-only, so you
  cannot dump a partition through it. On an **OEM-unlocked** device you can instead flash a custom
  recovery (TWRP) with thor, then `dd` the raw partitions out over adb — that's how the
  irreplaceable per-device partitions (`efs`, `modemst`, `persist`) are backed up before a flash.
  A locked device can't take a custom recovery, so this backup path needs OEM unlock.
- **Newest devices are locked.** Samsung disabled download mode on the Galaxy S26 / Z Fold 7
  (2026); this targets the large base of existing devices.

## Tested against

Development was validated on a live Qualcomm-based Samsung (bootloader v3). `thor dump-pit`
produced a PIT byte-identical to the reference C# Thor's dump, and `thor print-pit` matched
its output field-for-field.

The **write path was exercised end-to-end** on a **Galaxy J2 (SM-J250Y, `j2y18lte`, Snapdragon
425)**: thor flashed TWRP 3.7.0 to `RECOVERY` (18.4 MB, one sequence / 18 parts) with a clean
session and shutdown, and the phone booted it. From TWRP, every device-unique partition (`efs`,
`modemst1/2`, `fsg`, `fsc`, `persist`, …) was dumped and verified — on-device sha256 == PC
sha256, zero mismatches. The kernel-log (`sec_log`) buffer was located at physical `0x85200000`,
groundwork for a no-root boot-log feature.

## License

[Mozilla Public License 2.0](../LICENSE), like the original project.

## Credits

[TheAirBlow](https://github.com/theairblow) for Thor · [Benjamin-Dobell](https://github.com/Benjamin-Dobell)
for documenting the Odin protocol · this Rust port and its documentation.
