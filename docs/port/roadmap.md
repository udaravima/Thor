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

M1 read-only skeleton → M2 flashing engine → M3 remaining Odin ops → M4 archives (tar/lz4)
→ M5 confirm Windows/macOS backends → M6 polish (REPL parity, packaging). See
[rust-milestone-1.md](rust-milestone-1.md) for the current task board.
