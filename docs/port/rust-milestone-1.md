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
| 3 | LE byte helpers (read/write int/long, align) | pure | round-trip unit tests | ⬜ next |
| 4 | Odin command framing + error decode | pure | encode known packets; decode `0xFF`+codes | ⬜ |
| 5 | Flash-sequence math (ported as pure fn) | pure | vectors for v0/1 (128 KiB) and v2+ (1 MiB) | ⬜ |
| 6 | `UsbHandler` trait + nusb Linux backend | I/O | live device (supervised) | ⬜ |
| 7 | Odin session: handshake, begin, dump_pit | I/O | live device (supervised) | ⬜ |
| 8 | `thor-cli` minimal REPL (read-only cmds) | I/O | manual, vs C# on the same phone | ⬜ |

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
