# Changelog — Thor (Rust)

All notable changes to the Rust port (`thor-rs/`) are documented here. The port is a
from-scratch reimplementation of the C#/.NET Thor flasher, so its version line is independent
of the upstream C# tool (whose releases are tagged `1.x`). Rust-port releases are tagged
`rust-v<version>`.

## [0.1.0] — 2026-09-06

First public release of the Rust port: a cross-platform, test-driven Samsung **Odin** flasher
with an interactive shell, live flashing, the full Odin command set, upload-mode RAM dumping,
and a kernel-log carver. **Validated on real hardware, including live flashing.**

### Device & partition table (read-only)
- `thor list` — enumerate connected Samsung devices (reads the USB product string).
- `thor dump-pit` / `thor print-pit` — dump the PIT to a file or pretty-print it (from a file or
  live). Proven **byte-identical** to the reference C# tool on real hardware.
- `thor tar-list` — list the images inside an Odin `.tar` / `.tar.md5`.
- `thor flash-plan` — offline **dry run**: match every image to a PIT partition, resolve `.lz4`
  sizes from the frame header, and print the exact sequence/part plan. Writes nothing.

### Flashing & the Odin command set (write, gated)
- `thor flash` — flash a single image **or a whole `.tar`/`.tar.md5`** to its partition(s);
  `.lz4` decompressed on the fly; `--tflash` targets an inserted microSD. Dry run without
  `--execute`; with it, prints the plan and requires a typed confirmation.
- `thor factory-reset`, `thor erase`, `thor set-region`, `thor flash-pit` — the destructive Odin
  operations, each behind `--execute` + a typed confirmation word, non-interactive stdin refused
  without `--yes`, with loud warnings on brick-prone partitions.
- `thor reboot [normal|download]`, `thor end` — session teardown / reboot / shutdown.

### Upload mode (SUC) & kernel log
- `thor upload-probe` / `thor upload-dump` / `thor upload-reboot` — in upload/ramdump mode, list
  RAM regions and **dump physical memory** (read-only).
- `thor dmesg-carve` / `thor upload-dmesg` — reconstruct the kernel `printk` log out of a RAM
  dump (offline from a file, or dump-and-carve live). Handles both the classic `printk_log`
  record format and the 5.10+ `printk_ringbuffer`.

### Interactive shell
- `thor shell` — connect once, run many commands in one download-mode session (which can't be
  reused across program runs). Real line editing via rustyline: Tab-completion, `↑/↓` history
  (saved to `~/.thor_history`), `Ctrl-A/E/U/K/W`, `Ctrl-R` search, quote-aware parsing, a cached
  PIT, and `Ctrl-C` that aborts one operation rather than the session.

### Debugging
- `--debug` / `THOR_DEBUG=1` / `debug on` — trace every USB bulk transfer to stderr, each
  outgoing command decoded to its Odin region/sub-command name with a hex preview.

### Engine & safety
- Two crates: `thor-core` (UI-free, fully unit-tested engine) and `thor-cli` (the binary). The
  whole protocol is generic over a `Transport` trait — real USB (`nusb`, no libusb) in
  production, a scripted mock in tests. A Windows/macOS port is one trait implementation.
- Every destructive command is off by default, requires a typed confirmation, and warns on
  critical partitions. Signatures and anti-rollback remain bootloader-enforced.
- **102 tests**, built test-first; `cargo fmt` and `cargo clippy` clean.

### Hardware validation (2026-09-06)
- On a **Galaxy J2 (SM-J250Y, `j2y18lte`, Snapdragon 425)**, thor flashed **TWRP 3.7.0** to
  `RECOVERY` (18.4 MB, one sequence / 18 parts) with a clean session + shutdown; the phone booted
  it. The full read path (`list`, `dump-pit`, `print-pit`) was confirmed against the same device.
- Using TWRP, every device-unique partition (`efs`, `modemst1/2`, `fsg`, `fsc`, `persist`, and the
  smaller identity/attestation partitions) was dumped and **verified** — on-device sha256 == PC
  sha256, zero mismatches.
- The device's `sec_log` kernel-log buffer was located at physical `0x85200000` (2 MiB),
  groundwork for a planned no-root boot-log feature (upload-mode dump → `dmesg-carve`).

### Known gaps / not yet done
- Live flashing verified on Linux only; the `nusb` backend compiles for Windows/macOS but is
  unverified there.
- The whole-archive flash, factory-reset, erase, set-region, and flash-pit write paths share the
  now-exercised flash engine but have not each been fired individually at hardware.
- A partition **read** is impossible in the Odin protocol; raw backup needs an OEM-unlocked device
  and a custom recovery (see README).
