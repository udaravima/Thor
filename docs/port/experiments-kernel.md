# Experiments: kernel-level & USB-as-console (future directions)

Research notes for the "after the current implementation" ideas — getting kernel-level
visibility out of a Samsung device over USB, including **turning the USB link into printk /
UART output**. None of this is built yet; it's mapped so we know where to dig. All of it is
*separate from* the Odin/LOKE flashing protocol the current port implements.

There are **three distinct avenues**, and they differ in what they touch (bootloader vs
kernel), whether they need hardware, and what they yield.

## 1. Upload Mode — full RAM dump over USB (S-Boot) ★ most promising

Samsung's bootloader (S-Boot) has an **Upload Mode** (a.k.a. "kernel panic upload mode" /
ramdump): after a kernel panic — or when armed via the `*#9900#` *SysDump* menu (set debug
level MID/HIGH, enable upload mode) — the device reboots into an S-Boot state that **exposes
the entire RAM over USB** through its own protocol (distinct from Odin/LOKE).

**Why it's the big one for "USB → printk":** the kernel's `printk` log is a ring buffer *in
RAM* (`__log_buf` / the `dmesg` buffer). If you can dump RAM, you can carve the kernel log
out of it offline. So a RAM dump is a superset of "get the kernel log over USB" — no custom
kernel required, just the stock upload-mode path.

- **Protocol is already reverse-engineered.** `bkerler/sboot_dump` ("SUC") and
  `m4radt/upload-mode-dumper` implement the S-Boot Upload Client over USB. Studying those is
  the fast path.
- **How it'd fit thor-rs:** a new `upload` module implementing the SUC protocol over the same
  `Transport` seam we already have (nusb bulk transfers), plus a `dmesg`/ring-buffer carver
  that scans a dump for the `printk` log structure. The transport is done; this is a new
  protocol on top.
- **Caveats:** requires the device to be *in* upload mode (panic or SysDump-armed); debug
  level gating varies by model; on locked/newer devices this may be restricted.

### The SUC wire protocol (verified 2026-09-06 from `bkerler/sboot_dump/samupload.py`)

Same USB shape as Odin: **VID/PID `04e8:685d`**, interface **class 10** (CDC-data), bulk
in/out — i.e. our existing `Transport`/`backend` reaches it unchanged. (Note: `685d` is the
*same PID the bench device reports in download mode* — the upload-mode interface is reachable
on the same hardware once it's armed.) The framing is ASCII, in Samsung's signature
mixed-case magic-string style:

| Step | Host → device | Device → host |
|------|---------------|---------------|
| Detect / are-you-there | `PrEaMbLe\0` | `AcKnOwLeDgMeNt\0` (yes) or `NeGaTiVeAcKmNt\0` (no) — neither ⇒ not in upload mode |
| Probe regions | `PrObE\0` | up to `0x8000` bytes: a table of dumpable regions (RAM / FTL / CP), each an entry of `type`, null-terminated `name` (≤16–20 bytes), `start`, `end`; a leading `+` marks 64-bit addressing |
| Dump `[start,end)` | `PrEaMbLe\0`, then start & end as **ASCII hex** strings, then `DaTaXfEr\0` | streams the range in chunks; host sends `AcKnOwLeDgMeNt\0` after each chunk; device sends `PoStAmBlE\0` at end |
| Reboot | `PoWeRdOwN\0` | — |

Two clean, independently testable pieces fall out: **(a)** a probe-table parser (bytes → list
of regions), and **(b)** a dump state machine (preamble → hex range → `DaTaXfEr` → ack-per-chunk
→ `PoStAmBlE`). Both are pure and mockable with our existing `MockTransport` — no hardware to
write the tests. Only *running* it end-to-end needs a device in upload mode.

### ✅ Built (2026-09-06) — `thor-core::upload`

Implemented as a `thor-core` module (`upload.rs`) over the existing `Transport`, TDD with the
mock (9 tests): `parse_probe_table` (both 32- and 64-bit entry layouts), `Upload::handshake`
(preamble → ack, else `NotUploadMode`), `probe`, `dump_range` (the ack-per-chunk/postamble
loop), and `power_down`. CLI: `thor upload-probe`, `thor upload-dump <start> <end> <out>`,
`thor upload-reboot` — all **read-only** (dumping RAM writes nothing to the device). The one
part still needing a hardware capture to confirm is the exact address framing on the wire
(fixed-width ASCII hex is our reading of `samupload.py`); it's marked as such in the code.

### ✅ Built (2026-09-06) — `thor-core::dmesg` (the carver)

The payoff: `thor-core::dmesg` carves the kernel `printk` log out of a RAM dump (TDD, 7 tests).
`parse_printk_records` walks the structured `struct printk_log` records (Linux 3.5–5.9 — right
for the ~3.18 kernels on the target devices), and `carve_dmesg` scans a dump byte-by-byte for
the log buffer (a cheap plausibility check keeps it O(n); found at any offset, not just aligned
ones). CLI: `thor dmesg-carve <dumpfile>` (offline, pairs with `upload-dump`) and
`thor upload-dmesg <start> <end>` (dump + carve live). Demonstrated end to end on a synthetic
dump → real `dmesg`-style output. For the Linux **5.10+ lockless `printk_ringbuffer`**,
`carve_ringbuffer` recovers the log **text** (no timestamps) as a fallback — the 5.10 format
keeps text in `[unsigned long id][text]` data blocks but holds length/timestamp in a separate
descriptor+info array that isn't reliably locatable without kernel symbols, so a byte-exact
timestamped 5.10 carve remains a TODO needing a real dump. The syslog level bit is best-effort
throughout (text and, for the structured format, timestamp are exact).

## 2. UART jig — real serial console from a resistor on the USB ID pin

Samsung's micro-USB/USB-C port doubles as a **3.3V TTL UART** when a specific resistor sits
on the ID pin. The resistor value selects a factory/boot mode, and **D+/D- become UART
TX/RX** — a genuine serial console into S-Boot (press Enter during early boot) and, if the
kernel is configured for it, the kernel console.

- **Resistor → mode is model-specific.** Commonly cited: **619K** and **301K** select factory
  UART mode on different families (e.g. 301K on the older i9100-class; 619K on many others);
  523K turns factory mode off. Verify per model before trusting a value.
- **This is hardware**, not something thor-rs drives — but it's the only avenue that gives
  **early-boot** output (bootloader messages), which the software paths below cannot.
- **How it'd fit:** documentation + a known-good jig wiring reference, and possibly a small
  serial-console helper. Not a USB-protocol feature.
- **Device note:** the unit currently on the bench reports USB product string `MSM8953`
  (a Qualcomm SoC — the `04e8:685d` "Galaxy S II" name from `usb.ids` is a reused PID, not
  the real model), so pin/resistor specifics must be looked up for that board, not assumed.

## 3. USB gadget console — live printk over USB (needs a cooperating kernel)

On the Android side, Linux can route `printk` to a **USB CDC-ACM gadget** (`g_serial` /
configfs `f_acm`), exposing `/dev/ttyGS0` on the device and `/dev/ttyACM0` on the host. Add
`console=ttyGS0,115200` (or the right `ttyGSx`) to the kernel command line and kernel
messages stream over USB live.

- **Requires a custom/eng kernel** (the gadget console configured, and often
  `CONFIG_U_SERIAL_CONSOLE`). Stock retail kernels usually don't have it.
- **Limitation:** the gadget driver isn't up during early boot, so **bootloader and
  early-kernel messages are missed** — only post-init `dmesg`-era output appears. (UART jig,
  avenue 2, is the one that catches early boot.)
- **How it'd fit:** not a thor-rs feature per se; it's a device-kernel build option. thor-rs
  could offer a host-side reader for `/dev/ttyACM*` for convenience.

## How the three compare

| Avenue | Touches | Needs custom kernel? | Needs hardware? | Gets early boot? | Yields |
|--------|---------|----------------------|-----------------|------------------|--------|
| Upload mode RAM dump | S-Boot (bootloader) | No | No (just arm it) | n/a (post-mortem) | full RAM → carve dmesg |
| UART jig | S-Boot + kernel | No | Yes (resistor jig) | **Yes** | live serial console |
| USB gadget console | kernel | Yes | No | No | live dmesg over USB |

## Suggested order (once the Odin port is solid)

1. **Upload-mode dumper** — highest value, reuses our `Transport`, protocol already
   documented by open tools, no hardware. Gets us kernel memory (and thus the printk log).
2. **UART jig reference** — cheap to document; unlocks early-boot debugging for the truly
   low-level experiments.
3. **Gadget console reader** — only worthwhile if we're building custom kernels anyway.

## Sources

- Samsung upload mode / ramdump: `bkerler/sboot_dump`, `m4radt/upload-mode-dumper`, MSAB
  "Capturing RAM in Android systems".
- UART jig resistor values: XDA "USB UART on Galaxy S devices", `Otus9051/uart-usb-jig`.
- USB gadget console: kernel `Documentation/usb/gadget_serial.txt`; bootloader.wikidot serial
  console notes.
