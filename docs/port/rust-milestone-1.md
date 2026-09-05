# Rust Port — Milestone 1: read-only walking skeleton

**Goal:** prove the two hard things — native USB transport and the Odin handshake — by
doing only **non-destructive** operations (`connect → begin → dumpPit → printPit`). No
flashing, erase, or writes of any kind in this milestone.

**Decisions (locked):**
- Language: **Rust** (single static binary, like the current .NET AOT build).
- USB: **nusb 0.2.x** (pure-Rust, no libusb, cross-platform from day one).
- Layout mirrors the C# split: `thor-core` (engine) + `thor-cli` (shell), so the
  `IHandler` → trait seam that makes a Windows/macOS port cheap carries over.

**Ground truth / oracles:**
- The [`docs/`](../) module documentation (written from a full read of the C# source).
- `dotnet` 10 is installed → the C# reference is runnable for byte-diffing.
- `dev_files/sample-pit.pit` + `dev_files/sample-pit.log` (a real Galaxy S II PIT and the
  reference `printPit` output) → golden test vectors.
- A Galaxy S II (`04e8:685d`) is connected in download mode for the live transport test.

## Task board

| # | Task | Kind | Test strategy | Status |
|--:|------|------|---------------|--------|
| 1 | PIT parser (`PitData`/`PitEntry`) | pure | golden sample vs C# log | ✅ done (5 tests) |
| 2 | PIT field mappers (+ fix off-by-one) | pure | golden labels vs C# log | ✅ done (3 tests) |
| 3 | LE byte helpers (`Packet` build, read i32) | pure | round-trip unit tests | ✅ done (8 tests) |
| 4 | Odin failure decode (`0xFF` + error codes) | pure | decode `-2..-7` + unknown | ✅ done (5 tests) |
| 5 | Flash-sequence math (`plan_flash`) | pure | vectors for v0/1 (128 KiB) and v2+ (1 MiB) | ✅ done (7 tests) |
| 6a | `Transport` trait + scripted mock | seam | used by task 7 tests | ✅ done |
| 7 | Odin session: handshake, begin, dump_pit | logic | mock-tested (6 tests); live validation pending | ✅ logic done |
| 6b | nusb backend (list/open/claim/bulk) | I/O | live enumerate ✓; transport reaches handshake | ✅ built, compiles |
| 8 | `thor-cli` subcommands (list/dump-pit/print-pit) | I/O | `list` works live; dump pending reboot | ✅ built |

**Engine status:** all pure + protocol-sequence logic complete — 35 tests green, clippy
clean. Modules: `pit`, `proto`, `odin` (failure decode + session), `flash`, `transport`
(trait + mock), `backend` (nusb), plus the `thor` CLI.

**Live bring-up (in progress):** `thor list` enumerates the device without root (udev set).
`dump-pit` claims the CDC-data interface (class 0x0A, bulk IN 0x81 / OUT 0x01), so open/
claim/endpoints all work. Fixed a real nusb-vs-libusb difference: **IN buffers must request a
multiple of the endpoint max packet size** (else `InvalidArgument`) — `bulk_read` now rounds
up and trims to `actual_len`. Current blocker is expected device state: a prior attempt left
the bootloader past the handshake, so a fresh connect times out on `ODIN`/`LOKE`. **Needs a
device reboot into download mode**, then re-run `dump-pit` for the M1 acceptance test.

**Note:** the connected unit is an `MSM8953` (the `04e8:685d`→"Galaxy S II" name is a reused
PID), so its live PIT won't byte-match `sample-pit.pit` (an `MSM8937` from a different phone);
acceptance = a clean live dump that parses sanely, cross-checked with the C# on the same file.

**Acceptance test for M1:** on the user's machine, `dumpPit out.pit` from the Rust build
produces a file that matches the C# Thor's dump of the same phone byte-for-byte, and
`printPit` renders the same tree as `dev_files/sample-pit.log`.

## Notes & intentional improvements over the C# original

- **Off-by-one fixed:** `Field::describe` returns `"Unknown"` for any out-of-range value
  (incl. negatives) instead of the C# `GetMapping`'s `index > len` bug. Pinned by a test.
- Field name is a separate `label`, not element 0 of the value array — removes the awkward
  `value + 1` indexing the C# used throughout.
- Tests assert against real captured data, not hand-waved expectations.

## Explicit non-goals for M1

Flashing, erase, factory reset, set-region, reboot writes, tar/`.tar.md5`/`.lz4` handling,
Windows/macOS backends, full REPL command parity. All deferred to later milestones.
