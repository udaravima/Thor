# Rust Port — Roadmap, Fixes & Features

This port is also a re-architecting pass: we fix what the C# got wrong and fold in features
that are cheap to include now. This doc tracks research, the fixes we're making, and the
feature candidates. Living document.

## Research findings (2026-09-05)

**USB library — `nusb` 0.2.7** (released Aug 2026), pure-Rust, cross-platform, no libusb:
- `nusb::list_devices()` → `DeviceInfo` (with `vendor_id()`, `product_id()`).
- `DeviceInfo::open()` → `MaybeFuture<Device>`; `.wait()` to block.
- `Device::detach_and_claim_interface(n)` → detaches the kernel driver (our `cdc_acm`
  problem) **and** claims, in one call.
- `Device::active_configuration()` → parsed descriptors; iterate interfaces/endpoints to
  find the CDC-data (`0x0a`) bulk IN/OUT endpoints. **Replaces the manual descriptor
  byte-seeking in `Linux.cs`** — less code, less fragility.
- `Interface::endpoint::<Bulk, In/Out>(addr)` → `Endpoint`; `endpoint.transfer_blocking(Buffer, Duration)`
  → `Completion { status, buffer }`. Timeout surfaces as `TransferError::Cancelled`.

**Samsung landscape:**
- Odin/Download Mode **disabled on Galaxy S26 & Z Fold 7 (2026)**; recent devices gate
  download mode behind enabling *Maintenance Mode* in settings first.
- **Implication:** target the large installed base of existing/older Galaxy devices. Don't
  design for S26+; it's a dead end by vendor decision.
- Protocol is unchanged for supported devices — our [docs/](../) remain the spec.

## Fixes we're folding in (vs. the C# original)

| # | Issue (from the docs) | Plan | Status |
|--:|------------------------|------|--------|
| F1 | `GetMapping` off-by-one (could panic) | `Field::describe` returns `"Unknown"` | ✅ done |
| F2 | 32-bit `totalBytes` overflow > 2 GiB | `i64` throughout `flash` | ✅ done |
| F3 | Manual, fragile USB descriptor parsing | use nusb's parsed descriptors | ⬜ (task 6) |
| F4 | Two copy-paste command help strings | correct text in the CLI | ⬜ (task 8) |
| F5 | `disconnect` leaves stale session state | model session as an owned value; dropping it ends the session (Rust ownership makes the bug structurally impossible) | ⬜ |
| F6 | Dormant `_writeZlp` write path | make ZLP-after-write an explicit, documented option (default off); keep ZLP read | ⬜ |
| F7 | `SharpCompress` unused dependency | simply not carried over | ✅ n/a |
| F8 | `SetRegionCode` and `EnableTFlash` **both send session sub-command `0x08`** — almost certainly a copy-paste bug in the C# (`SetRegionCode` cloned from `EnableTFlash` without changing the opcode). Consequence: "set region" may actually enable T-Flash. | Ported faithfully but marked **unverified**; `set_region_code` carries the caveat in its docs and the `set-region` CLI prints it before a typed confirmation. Needs a hardware capture to resolve the true opcode. | ⬜ flagged |

## Feature candidates (include where cheap; revisit the rest)

**Committed for this port:**
- **Cross-platform (Linux + Windows + macOS)** — free with nusb; the whole reason we chose it.
- **Scriptable subcommands** alongside the REPL (e.g. `thor dump-pit out.pit`) — enables CI
  and automation, which the interactive-only C# can't do.
- **Typed errors** end to end (already underway: `PitError`, `OdinFailure`).
- **PIT export to JSON** (in addition to the pretty tree) — machine-readable, diffable.

**Under consideration (decide before M2+):**
- Verify-after-flash / `--dry-run` planning output (uses `plan_flash`, which is already tested).
- `--yes` non-interactive confirmations for automation, keeping default-No interactively.
- Reconnect/resume investigation — the C# README says a USB connection can't be reused after
  a session; check whether nusb's cleaner teardown changes that.
- Parity options: T-Flash, EFS clear, bootloader update, reset-flash-count.

**Explicitly out of scope:**
- Galaxy S26+ / any device where Samsung removed download mode.
- A GUI (this is a CLI project).

## Milestone sequence

M1 read-only skeleton ✅ → **M2 flashing engine ✅ (engine + dry-run + single-image & whole-
archive live flash, all gated; not yet fired at real hardware)** →
M3 remaining Odin ops ✅ (session lifecycle; factory-reset, partition erase, T-Flash, set-region
— all gated; set-region unverified per F8) →
M4 archives (tar/lz4) ✅ → M5 confirm Windows/macOS backends → M6 polish (REPL parity,
packaging). See [rust-milestone-1.md](rust-milestone-1.md) for M1.

**Session lifecycle (0x67) done (2026-09-05):** `Odin::end_session/reboot/reboot_to_odin/
shutdown`; CLI `thor reboot [normal|download]`, `thor end`, and a `--reboot`/`--reboot-download`/
`--shutdown` finish flag on `dump-pit` (dump-then-reboot in one session, since a download-mode
connection can't be reused across invocations). 48 tests, clippy clean.

**M6 polish — interactive REPL + package done (2026-09-05):** `thor shell` runs a single
persistent session (connect once; `pit` / `dump-pit` / `flash-plan` / `tar-list` / `reboot` /
`end` / `quit`), which is the real fix for the connection-reuse limitation. Release binary
builds at ~788 KB. The C#'s copy-paste help-string bugs (F4) never existed in the Rust CLI, so
that fix is moot here. Still non-destructive. (REPL loop not yet exercised live — needs a fresh
download-mode boot.)

### M2 status (2026-09-05)

**Done (test-first, no live destructive flash yet):**
- `Odin::set_total_bytes` (0x64/0x02) and `Odin::flash_partition` (region 0x66) — the full
  sequence/part engine driven by the tested `plan_flash`. Handles phone vs modem end-of-
  sequence layouts, erase (None source → zeros), per-part index verification, EFS/bootloader/
  reset-flash-count flags. 5 new mock-tested cases (40 tests total, clippy clean).
- `thor flash-plan [--pit <file>] <file> [partition]` — a **dry run** that shows exactly what
  a flash would do (sequences, parts, real vs on-wire bytes) and writes nothing. Works live
  or fully offline against a saved PIT. Verified: a 70 MiB image → 3 sequences (30+30+10 MiB)
  with new-gen 1 MiB packets.

**Live single-image flash (2026-09-06) — ✅ built and gated:**
- `thor flash [--execute] [--yes] [--reboot|…] <file> [partition]` drives the tested
  `flash_partition` engine against the connected device. `.lz4` is decompressed on the fly
  via `archive::lz4_stream_reader` (new, streaming — never buffers the whole image), with the
  on-device length read from the frame's content-size header.
- Safety gating: dry-run unless `--execute`; then it prints the plan and requires the user to
  **type the partition name** (a stronger gate than y/n — it forces naming what's overwritten),
  refuses a non-interactive stdin unless `--yes`, and warns on bootloader/critical partitions
  (`is_critical_partition`). Guards (`--pit`+`--execute`, archive target) refuse *before* any
  device I/O. New pure helpers are TDD-tested in `thor-cli/src/progress.rs`; 57 tests total.
- **Not yet fired at real hardware** — still awaiting a **safe target** (real signed stock
  firmware, or a spare device). Odin has no partition-read, so "dump and flash back unchanged"
  is not available as a safety net.

**Whole-archive live flash (2026-09-06) — ✅ built and gated:**
- `thor flash --execute <archive.tar[.md5]>` flashes every image matching a PIT partition in
  one session. Two passes: `list_archive_images` resolves each image's on-device size (so the
  summed total is announced via `set_total_bytes` up front), then the new streaming
  `archive::for_each_image` hands each entry to `flash_partition` as a decompressed reader —
  `.lz4` entries are decoded on the fly (wrapping the tar entry directly, so no header-chain
  trick was needed after all), never buffering a whole image.
- One confirmation for the archive (typing each partition name is impractical): type `FLASH`,
  with the critical-partition warning and non-interactive-stdin refusal from the single-image
  path. Unmatched images are listed and skipped. 58 tests total.

**M2 is complete.** Nothing writes to real hardware yet — still awaiting a safe target.

Session end/reboot (0x67 region) for clean teardown — ✅ done (see M3 line).

### M4 status (2026-09-05) — ✅ archive + LZ4 handling complete

- `archive` module: `lz4_content_size` (reads the decompressed size cheaply from the LZ4
  frame header), `decompress_lz4`, `list_tar` / `extract_tar` (generic over `Read`, so a
  `File` streams instead of buffering multi-GB firmware), and `list_archive_images` (per-image
  real sizes, peeking only ~16 bytes of each `.lz4` entry). 7 unit tests against data built
  with the real `lz4_flex` / `tar` crates.
- CLI: `thor tar-list <archive>` lists an Odin `.tar`/`.tar.md5`; `thor flash-plan` now does
  **whole-archive** dry-run planning — matches each contained image to a PIT partition (direct,
  then with `.lz4` stripped), resolves `.lz4` real sizes, and skips unmatched entries. Also
  handles a single image (`.lz4` size-resolved) and offline (`--pit`) planning.
- Demoed on a mixed `AP_demo.tar.md5`: `sbl1.mbn.lz4`→SBL1 (2 parts), `aboot.mbn`→ABOOT (padded
  to 1 MiB), `unknown.img` skipped. Still zero device risk — nothing writes to a partition.

### M3 status (2026-09-06) — ✅ destructive ops, all gated

The remaining session-region (`0x64`) commands, ported from the C# `Odin.cs` (TDD, 5 new
mock tests → 63 total):

- `erase_user_data` (`0x64/0x07`, `LONG_TIMEOUT` 600 s) — factory reset. CLI `factory-reset`,
  typed `ERASE`, warns about VaultKeeper (below).
- `enable_tflash` (`0x64/0x08`) — sets the `tflash_enabled` flag; errors if already on. CLI
  `flash --tflash` enables it before flashing, so the flash targets an inserted microSD.
- `set_region_code` (`0x64/0x08` + 3-char string) — **unverified, see F8**: shares `0x08` with
  T-Flash enable. CLI `set-region`, typed `YES`, prints the F8 caveat.
- Partition zero-erase — not a new opcode; it's `flash_partition(None, …)` (the tested erase
  path). CLI `erase <partition> --size <bytes>`, typed partition name. `--size` is required
  because the PIT has no reliable per-partition byte size to infer.

All four are dry-run-by-default (`--execute` to act), refuse a non-interactive stdin without
`--yes`, and reuse the same typed-confirmation gate as `flash`. Nothing fired at real hardware.

## Community research (2026-09-06) — hidden mechanisms & protocol updates

Sweep of Heimdall, XDA, and download-mode write-ups for anything we should keep in mind. What's
actionable:

- **Odin protocol version 4.** Odin 3.12.xx sends BeginSession proto-version `3`; newer Odin
  sends `4` (Heimdall PR #459 — first needed for US S8/S8+). We sidestep the whole issue by
  sending `i32::MAX` as a catch-all (as the C# does), which is *why* it works across
  generations — but a real v4 bootloader could in principle behave differently; worth a capture
  if we ever see a version-related `BeginSession` failure. No code change now.
- **Rollback protection (anti-rollback fuses).** Flashing firmware older than the fused RP
  (rollback-protection) level fails with `SW REV CHECK FAIL`. This is a common real failure our
  decoder currently lumps into `Unknown`/`Auth`. TODO: capture the actual `0xFF` code for it and
  add a `FlashFailKind` variant with a clear message ("firmware too old — anti-rollback").
- **VaultKeeper.** On unlocked bootloaders, wiping `/data` triggers VaultKeeper to *re-lock* the
  bootloader until setup is completed online. Already surfaced as a warning on `factory-reset`.
- **Any non-handshake request locks the device.** Confirmed: in download mode the device waits
  only for `ODIN`/`LOKE`; any other first packet drops it into an error state — which is exactly
  why a connection can't be reused across invocations. Validates our reboot-between-attempts UX
  and the `shell` REPL.
- **Empty-bulk-transfer / ZLP after file parts.** Heimdall sends (and tolerates a failed) empty
  bulk transfer around file transfers; some bootloaders expect a ZLP at end-of-transfer. Ties
  directly to fix **F6** (our dormant ZLP-after-write path) — the item to revisit if a real
  flash stalls at "end of file transfer sequence".
- **Access is shrinking, as expected.** One UI 8 (mid-2025) removed bootloader unlocking even
  on international models; 2026 devices require enabling *Maintenance Mode* before download mode
  is reachable. Reinforces the port's framing: target the large base of existing devices.
