# Security policy

Thor talks to Samsung devices at a very low level — flashing firmware, repartitioning, erasing
userdata, and dumping RAM. Please use it, and report issues with it, responsibly.

## Reporting a vulnerability

If you find a security issue in the tool itself (for example, a bug that could corrupt the
wrong partition, mishandle a confirmation gate, or mis-parse a device reply in a dangerous way),
please report it privately first rather than opening a public issue:

- Use GitHub's **"Report a vulnerability"** (Security → Advisories) on this repository, or
- contact the maintainer directly.

Include the command, the device model and mode, and a `--debug` wire trace if you have one.
We'll acknowledge and work on a fix before any public disclosure.

## Scope and responsible use

- **Intended use** is on devices **you own or are authorized to service**, for development,
  repair, firmware research, and recovery.
- Every destructive operation is off by default (`--execute` plus a typed confirmation) — please
  don't file "it wiped my device" reports for operations you explicitly confirmed. Do file them
  if a gate failed to trigger, or a *non*-destructive command wrote to the device.
- **Out of scope:** using or extending this tool to bypass Factory Reset Protection (FRP) or
  other anti-theft measures on devices you don't own. The FRP guidance in the docs covers only
  the legitimate account-based path, and contributions that add theft-enablement features will
  not be accepted.

## A note on bricking

Flashing the bootloader chain, a mismatched PIT, or firmware older than the device's fused
anti-rollback level can permanently brick a device. Thor tries hard to make these deliberate
(warnings, typed confirmations), but it cannot make them safe. Keep a known-good stock firmware
for your exact model before writing anything.
