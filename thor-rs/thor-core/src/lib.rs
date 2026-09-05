//! # thor-core
//!
//! The reusable engine behind Thor — a from-scratch reimplementation of Samsung's
//! Odin firmware-flashing protocol. This crate is UI-free: it parses PIT partition
//! tables, speaks the Odin/LOKE protocol, and moves bytes over USB. A separate binary
//! crate (`thor-cli`) provides the interactive shell.
//!
//! Ported from the original C# (`TheAirBlow.Thor.Library`). See `../../docs/` for the
//! module-by-module documentation this port is built against.

pub mod pit;
