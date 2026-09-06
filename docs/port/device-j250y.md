# Device guide: Galaxy J2 (2018) — SM-J250Y (`j2y18lte`)

A community "open it all the way up" guide for the **SM-J250Y**, and how the Thor Rust port
applies at each step. This is a *legacy* device (2018), which is exactly what makes it a great
target: it predates every lockdown Samsung added later, so the whole ladder is available.

> **Scope.** This covers unlocking and instrumenting a device **you own**. It does **not**
> cover FRP (Factory Reset Protection) *bypass* — see [FRP](#frp-factory-reset-protection),
> where the only method here is the legitimate account-based one.

## The device

| | |
|---|---|
| Model | SM-J250Y (Galaxy J2 2018 / "J2 Pro 2018") |
| Codename | `j2y18lte` |
| SoC | **Exynos 7570** (quad Cortex-A53) |
| Android | 7.1.1 Nougat → 8.0/8.1 Oreo |
| Why it's open | Ships **before** VaultKeeper auto-relock, before One UI 8 removed unlocking, before the 2026 Maintenance-Mode gate on download mode |

> **Bench-device note.** The device this port was validated against reported USB product
> string `MSM8953` — a *Qualcomm* SoC, not the Exynos 7570 of a J250Y. So either that's a
> different unit or the string is misleading. Confirm which SoC you actually have before
> trusting Exynos-specific details below (UART resistor, upload-mode specifics differ by SoC).

## The unlock ladder

Each rung enables the next; do them in order.

1. **Developer Options + OEM Unlock.** Settings → About phone → tap **Build number** 7×. Then
   Settings → Developer options → enable **OEM unlocking** and **USB debugging**. OEM unlocking
   *must* be on before any flashing, or the next flash dies with `BLOCKED BY FRP LOCK`.
2. **Bootloader unlock.** Power off → **Vol-Up + Vol-Down + USB** to enter download mode →
   long-press **Vol-Up** to confirm the unlock. This **wipes `/data`**. On this generation it's
   a clean toggle — none of the VaultKeeper auto-relock behaviour of 2025+ devices.
3. **Custom recovery (TWRP).** Flash `twrp-*-j2y18lte.tar` into the **AP** slot with auto-reboot
   OFF, then boot straight to recovery (Power + Home + Vol-Up). Official TWRP 3.2.x exists.
   - With Thor: `thor flash --execute twrp-j2y18lte.tar` (and confirm), then `thor reboot`.
4. **Root.** Either flash Magisk from TWRP, or the **TWRP-less** route: Magisk-patch the stock
   `AP` tar on a phone with Magisk installed, then flash the patched `AP` via Odin/Thor.
5. **Custom ROMs.** Community LineageOS/AOSP builds exist for `j2y18lte` (LOS 14.1/15.1-era,
   unofficial). Flash from TWRP.

## The three traps that brick *this* device

- **dm-verity.** Enforced. Modifying `/system` (or mounting it read-write) triggers a bootloop
  unless you also flash a no-verity / vbmeta-disable zip. This is the classic J2-2018 gotcha.
- **Knox e-fuse.** Flashing TWRP burns Knox `0x1` **irreversibly** — it disables Knox-dependent
  features (Secure Folder, Samsung Pay) and voids warranty. One-way; accept it before you flash.
- **Anti-rollback (`SW REV CHECK FAIL`).** You cannot flash firmware older than the level fused
  into the device. Thor now surfaces this as a hint on a rejected flash (`Auth`/`Unknown`).

## FRP (Factory Reset Protection)

FRP is Google's anti-theft lock: after a factory reset the device demands the Google account
that was previously signed in. The **only** approach documented here is the legitimate one,
because that's the one that belongs to the owner:

- **Prevent it:** before resetting/flashing, remove the Google account **and** enable OEM
  Unlocking. Then FRP isn't armed.
- **Clear it:** sign in with the device's own Google account after the reset.
- **`BLOCKED BY FRP LOCK` during flashing is not a wall to bypass** — it means OEM Unlocking
  was not enabled first (rung 1). Enable it and the message goes away.

FRP-*bypass* tooling (getting past the account check *without* the credentials) is out of scope:
its predominant real-world use is unlocking devices the operator doesn't own, and ownership
can't be verified from a guide.

## Deep diagnostics (the "do anything" tail)

- **`*#9900#` SysDump.** Dial it to open the SysDump menu: set the debug level (LOW/MID/HIGH)
  and **arm upload mode**. That's the bridge to a RAM dump — arm it here, then read RAM over USB
  with Thor's upload commands:
  - `thor upload-probe` — list the dumpable RAM/CP regions.
  - `thor upload-dump <start> <end> out.bin` — dump a range. The kernel `printk`/`dmesg` ring
    buffer lives in RAM, so a dump is a superset of "get the kernel log over USB". See
    [experiments-kernel.md](experiments-kernel.md) for the SUC protocol details.
- **Combination / ENG firmware.** Samsung factory ("combination") binaries for the J250 family
  boot a stripped image with **ADB root, diagnostics, and a relaxed eng-bootloader** — deep
  poking without even unlocking. These are publicly-distributed factory firmware, used here as
  a diagnostic tool, not an unlock trick.
- **UART jig.** The USB port doubles as a 3.3 V TTL UART with the right resistor on the ID pin,
  giving an early-boot serial console. The exact resistor is SoC/board-specific — look up the
  `j2y18lte`/Exynos-7570 value; don't assume one. See [experiments-kernel.md](experiments-kernel.md).

## Where Thor fits

| Step | Thor command |
|---|---|
| Read the partition map | `thor dump-pit out.pit` · `thor print-pit` |
| Preview a flash (no writes) | `thor flash-plan AP_*.tar.md5` |
| Flash TWRP / a ROM / an image | `thor flash --execute <file>` (typed confirm) |
| Whole stock firmware | `thor flash --execute AP_*.tar.md5` (type `FLASH`) |
| Factory reset / erase / region | `thor factory-reset` · `thor erase` · `thor set-region` |
| RAM dump (upload mode) | `thor upload-probe` · `thor upload-dump` |
| Reboot out of a mode | `thor reboot [normal\|download]` · `thor upload-reboot` |

## Sources

- Bootloader unlock / TWRP / root: XDA `j2y18lte` threads; DroidViews / GetDroidTips J2-2018
  TWRP+root guides.
- dm-verity / Knox fuse behaviour: XDA J2-2018 recovery threads.
- SysDump / upload mode: `*#9900#` SysDump references; `bkerler/sboot_dump`.
- FRP mechanism: Google/Android Factory Reset Protection documentation.
