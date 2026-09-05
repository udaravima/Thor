# The PIT Format (Partition Information Table)

> Source of truth: [`PIT/PitData.cs`](../TheAirBlow.Thor.Library/PIT/PitData.cs),
> [`PIT/PitEntry.cs`](../TheAirBlow.Thor.Library/PIT/PitEntry.cs),
> [`PIT/FieldMapper.cs`](../TheAirBlow.Thor.Library/PIT/FieldMapper.cs).

## What a PIT is, and why Thor can't flash without it

A PIT is Samsung's **map of the phone's storage** — which partitions exist, how big they
are, what filename each expects, and what kind of data each holds. Thor reads it for one
concrete reason: **the flash protocol addresses partitions by numeric ID and type, not by
name.** When you hand Thor `boot.img`, it has to look that filename up in the PIT to learn
the `PartitionId`, `BinaryType` and `DeviceType` to put in the end-of-sequence packet
(see [odin-protocol.md](odin-protocol.md#region-0x66--flashing-the-hot-path)). No PIT, no
addressing, no flash.

Thor gets the PIT two ways: pulled live off the device (`DumpPIT`, region `0x65`) or read
from a `.pit` file on disk. Same parser either way — `PitData` takes either a byte array
or a file path.

## The binary layout

Everything is **little-endian**, fixed-width, no padding, no alignment tricks. The parser
is a straight sequential read (`BinaryReader`), so the layout *is* the read order.

### Header — 28 bytes

| Offset | Size | Field | Notes |
|-------:|-----:|-------|-------|
| 0  | 4  | **Magic** | must equal `0x12349876`, else "Magic number mismatch!" |
| 4  | 4  | **Entry count** | how many partition entries follow |
| 8  | 8  | **Unknown** | ASCII string, purpose undocumented |
| 16 | 8  | **Project** | ASCII project/model name |
| 24 | 4  | **Reserved** | |

### Each entry — 132 bytes

Nine 32-bit integers (36 bytes) followed by three 32-byte ASCII strings (96 bytes). Read
in exactly this order:

| # | Offset | Size | Field | What it is |
|--:|-------:|-----:|-------|-----------|
| 1 | 0   | 4  | `BinaryType` | Phone/AP vs Modem/CP — **drives which flash layout is used** |
| 2 | 4   | 4  | `DeviceType` | storage type (NAND, EMMC, …) |
| 3 | 8   | 4  | `PartitionId` | the numeric address the flash protocol targets |
| 4 | 12  | 4  | `Attributes` | read-only / read-write / partition-type (see mapper) |
| 5 | 16  | 4  | `UpdateAttributes` | update policy / filesystem (see mapper) |
| 6 | 20  | 4  | `BlockSize` | **also the version heuristic** — see below |
| 7 | 24  | 4  | `BlockCount` | partition size in blocks |
| 8 | 28  | 4  | `FileOffset` | |
| 9 | 32  | 4  | `FileSize` | |
| 10 | 36 | 32 | `Partition` | human name, e.g. `BOOT`, `USERDATA` |
| 11 | 68 | 32 | `FileName` | expected flash filename, e.g. `boot.img` |
| 12 | 100 | 32 | `DeltaName` | delta/OTA name, usually empty |

Strings are null-trimmed ASCII (`ReadString` reads N bytes then `TrimEnd('\0')`).

So total file size = `28 + 132 × entryCount` bytes.

## The "old vs new" heuristic (and why it's fragile)

Here's the clever, slightly alarming part. There are **two generations of PIT** whose
fields carry different meanings, and the format itself contains no version flag. Thor
guesses the generation from the data:

```csharp
// PitData.Parse — while reading entries
if (i > 0 && lastBlockSize != entry.BlockSize)
    IsNewVersion = true;
```

The logic: on **old** PITs the `BlockSize` field really is a constant block size, identical
across every partition. On **new** PITs that same field slot was repurposed as
`StartBlock`, which naturally differs per partition. So *"if field #6 ever changes between
entries, it must be a new-style PIT."*

**Stakes / trap:** this is a heuristic, not a spec. A new-style PIT where every partition
coincidentally shares a start block, or an old PIT with unusual data, could be
misclassified — and misclassification changes how every numeric field is *labelled* for
the human (below). It does not corrupt a flash (the raw ints are still sent verbatim), but
it can mislabel a `printPit` report. Worth knowing before you trust the pretty output.

The chosen generation selects a `FieldMapper.Mapper` that turns raw integers into words.

## The field mappers (integer → human label)

`FieldMapper` holds two lookup tables. Each array's **element 0 is the field's display
name**; elements 1+ are the value→description list. Callers index with `value + 1` to skip
the name slot.

### New-style PIT (`NewPitMapper`)

| Raw field | Shown as | Values (0, 1, 2, …) |
|-----------|----------|----------------------|
| `BinaryType` | **Binary Type** | Phone/AP, Modem/CP |
| `DeviceType` | **Device Type** | OneNAND, NAND, EMMC, SPI, IDE, NAND X16 |
| `Attributes` | **Partition Type** | None, BCT, Bootloader, Partition Table, NV-Data, Data, MBR, EBR, GP1, GP1 |
| `UpdateAttributes` | **Filesystem** | None, Basic, Enhanced, EXT2, YAFFS2, EXT4 |
| `BlockSize` | **Start Block** | (numeric) |
| `BlockCount` | **Block Count** | (numeric) |

### Old-style PIT (`OldPitMapper`)

| Raw field | Shown as | Values (0, 1, 2, …) |
|-----------|----------|----------------------|
| `BinaryType` | **Binary Type** | Phone/AP, Modem/CP |
| `DeviceType` | **Device Type** | OneNAND, NAND, MoviNAND |
| `Attributes` | **Attributes** | Read-only, Read-write, STL |
| `UpdateAttributes` | **Update Attributes** | None, FOTA, Secure, Secure FOTA |
| `BlockSize` | **Block Size** | (numeric) |
| `BlockCount` | **Block Count** | (numeric) |

Notice the same raw fields (`Attributes`, `UpdateAttributes`, and the `BlockSize` *label*)
carry entirely different semantics between generations. That's the whole reason the
heuristic has to exist.

## A latent bug worth documenting

`FieldMapper.GetMapping` is meant to be the safe accessor with an "Unknown" fallback:

```csharp
public static string GetMapping(this string[] array, int index)
    => index > array.Length ? "Unknown" : array[index];
```

The guard is off by one: it should be `index >= array.Length`. When `index == array.Length`
the ternary takes the *safe-looking* branch and then does `array[array.Length]` — an
out-of-range access that throws. In practice this only triggers on a field value one past
the last known label, but it's a real edge.

Compounding it, the two `printPit` commands don't agree on safety:

- the top-level [`Commands/PrintPIT.cs`](../TheAirBlow.Thor.Shell/Commands/PrintPIT.cs)
  uses `GetMapping` (fallback, mostly),
- the in-session [`Commands/ProtoOdin/PrintPIT.cs`](../TheAirBlow.Thor.Shell/Commands/ProtoOdin/PrintPIT.cs)
  indexes the array **directly** (`mapper.BinaryType[entry.BinaryType + 1]`), so an
  unexpected value throws, gets caught, and reports the PIT as "not valid" — misleading,
  since the PIT parsed fine and only a *label* lookup failed.

Neither breaks flashing (the raw ints are what get sent), but both are the kind of thing a
port should fix rather than faithfully reproduce.

## How the rest of Thor uses this

- **Auto-partition matching:** `flashFile`/`flashTar` match your image's filename against
  each entry's `FileName` to pick the partition automatically, then confirm with you.
- **Addressing the flash:** `PartitionId`, `BinaryType`, `DeviceType` from the matched
  entry go straight into the `0x66/0x03` end-of-sequence packet.
- **Pretty printing:** `printPit` renders the parsed table as a Spectre.Console tree, using
  the mapper for labels.

## Verified vs. inferred

- **Verified:** magic number, field order, sizes, the version heuristic, the mapper tables,
  and the `GetMapping` off-by-one — all read directly from source.
- **Inferred:** the *reason* `BlockSize`/`StartBlock` diverges between generations (stated
  as fact in the mapper comments, consistent with Heimdall's findings, but not provable
  from this repo alone). The `Unknown` and `Reserved` header fields are exactly that —
  unknown.

See also: [Odin protocol](odin-protocol.md) · [end-to-end flash](end-to-end-flash.md).
