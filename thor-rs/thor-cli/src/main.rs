//! Thor CLI (Rust port) — Milestone 1: read-only commands.
//!
//! Subcommands (scriptable, unlike the interactive-only C#):
//!   thor list                 List connected Samsung devices.
//!   thor dump-pit <out.pit>   Connect, begin an Odin session, dump the PIT to a file.
//!   thor print-pit [file]     Print a PIT: from <file> if given, else dumped live.
//!
//! Everything here is non-destructive. Flashing lands in later milestones.

mod progress;

use std::error::Error;
use std::io::{IsTerminal, Read, Seek, Write};
use std::process::ExitCode;

use thor_core::archive::{
    for_each_image, list_archive_images, list_tar, lz4_content_size, lz4_stream_reader,
};
use thor_core::backend::{self, NusbTransport};
use thor_core::flash::{plan_flash, FlashParams};
use thor_core::odin::{FlashState, Odin};
use thor_core::pit::{FieldMapper, PitData, PitEntry};
use thor_core::upload::{Upload, UploadError};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("list") => cmd_list(),
        Some("dump-pit") => cmd_dump_pit(&args[2..]),
        Some("print-pit") => cmd_print_pit(args.get(2)),
        Some("tar-list") => cmd_tar_list(args.get(2)),
        Some("flash-plan") => cmd_flash_plan(&args[2..]),
        Some("flash") => cmd_flash(&args[2..]),
        Some("factory-reset") => cmd_factory_reset(&args[2..]),
        Some("erase") => cmd_erase(&args[2..]),
        Some("set-region") => cmd_set_region(&args[2..]),
        Some("upload-probe") => cmd_upload_probe(),
        Some("upload-dump") => cmd_upload_dump(&args[2..]),
        Some("upload-reboot") => cmd_upload_reboot(),
        Some("reboot") => cmd_reboot(args.get(2)),
        Some("end") => cmd_end(),
        Some("shell") => cmd_shell(),
        _ => {
            print_usage();
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    eprintln!(
        "Thor (Rust port) — read-only preview\n\n\
         USAGE:\n\
         \x20 thor list                 List connected Samsung devices\n\
         \x20 thor dump-pit <out.pit> [--reboot|--reboot-download|--shutdown]\n\
         \x20                           Dump the partition table, then optionally reboot\n\
         \x20 thor print-pit [file]     Print a PIT (from a file, or dumped live)\n\
         \x20 thor tar-list <archive>   List the files in an Odin .tar / .tar.md5 archive\n\
         \x20 thor flash-plan [--pit <pit>] <file> [partition]\n\
         \x20                           Dry run: show what flashing <file> would do (no writes)\n\
         \x20 thor flash [--execute] [--yes] [--tflash] [--reboot|--reboot-download|--shutdown] <file> [partition]\n\
         \x20                           Flash <file> (image or .tar) to its partition(s). WITHOUT --execute\n\
         \x20                           this is a dry run; --execute writes. --tflash targets a microSD.\n\
         \x20 thor factory-reset --execute [--yes] [--reboot|…]   Wipe /data (factory reset)\n\
         \x20 thor erase <partition> --size <bytes> --execute [--yes]   Zero-fill a partition\n\
         \x20 thor set-region <XAA> --execute [--yes]   Set the CSC region code (UNVERIFIED, see docs)\n\
         \x20 thor upload-probe          List RAM regions of a device in upload mode (read-only)\n\
         \x20 thor upload-dump <start> <end> <out>   Dump a memory range over upload mode\n\
         \x20 thor upload-reboot         Reboot a device out of upload mode\n\
         \x20 thor reboot [normal|download]   Reboot the device (default normal)\n\
         \x20 thor end                  Shut the device down / end the session\n\
         \x20 thor shell                Interactive session: connect once, run many commands\n"
    );
}

/// Interactive REPL over a single persistent Odin session. Because a download-mode
/// connection can't be reused across invocations, this is the ergonomic way to run several
/// operations: connect once, then dump / plan / reboot without reconnecting.
fn cmd_shell() -> Result<(), Box<dyn Error>> {
    let mut odin = open_session()?;
    println!("\nInteractive Odin session — type 'help' for commands.");
    let stdin = std::io::stdin();
    loop {
        print!("thor> ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            println!();
            break; // EOF (Ctrl-D)
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        let (cmd, cmd_args) = match parts.split_first() {
            Some(x) => x,
            None => continue,
        };
        match *cmd {
            "help" | "?" => print_shell_help(),
            "pit" | "print-pit" => report(shell_print_pit(&mut odin)),
            "dump-pit" => report(shell_dump_pit(&mut odin, cmd_args)),
            "flash-plan" => report(shell_flash_plan(&mut odin, cmd_args)),
            "tar-list" => report(shell_tar_list(cmd_args)),
            "reboot" => {
                let finish = match cmd_args.first().copied() {
                    None | Some("normal") => Finish::RebootNormal,
                    Some("download") => Finish::RebootDownload,
                    Some(o) => {
                        eprintln!("unknown reboot mode '{o}' (use normal | download)");
                        continue;
                    }
                };
                return finish_session(odin, finish);
            }
            "end" | "shutdown" => return finish_session(odin, Finish::Shutdown),
            "quit" | "exit" => {
                println!("Leaving — device stays in download mode (run 'thor reboot' to exit).");
                return Ok(());
            }
            other => eprintln!("unknown command '{other}' (try 'help')"),
        }
    }
    Ok(())
}

/// Print an error from a shell subcommand without tearing down the session.
fn report(result: Result<(), Box<dyn Error>>) {
    if let Err(e) = result {
        eprintln!("error: {e}");
    }
}

fn print_shell_help() {
    println!(
        "  pit | print-pit                dump & print the partition table\n\
         \x20 dump-pit <file>                dump the PIT to a file\n\
         \x20 flash-plan <file> [partition]  dry-run a flash (no writes)\n\
         \x20 tar-list <archive>             list an Odin .tar/.tar.md5\n\
         \x20 reboot [normal|download]       reboot the device and exit\n\
         \x20 end                            shut the device down and exit\n\
         \x20 quit | exit                    leave (device stays in download mode)"
    );
}

fn shell_print_pit(odin: &mut Odin<NusbTransport>) -> Result<(), Box<dyn Error>> {
    let pit = PitData::parse(&odin.dump_pit()?)?;
    print_pit(&pit);
    Ok(())
}

fn shell_dump_pit(odin: &mut Odin<NusbTransport>, args: &[&str]) -> Result<(), Box<dyn Error>> {
    let path = *args.first().ok_or("usage: dump-pit <file>")?;
    let pit = odin.dump_pit()?;
    std::fs::write(path, &pit)?;
    println!("Dumped {} bytes to {path}", pit.len());
    Ok(())
}

fn shell_flash_plan(odin: &mut Odin<NusbTransport>, args: &[&str]) -> Result<(), Box<dyn Error>> {
    let file = *args.first().ok_or("usage: flash-plan <file> [partition]")?;
    let partition = args.get(1).copied();
    let base = std::path::Path::new(file)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(file);
    let pit = PitData::parse(&odin.dump_pit()?)?;
    let params = odin.params().ok_or("session has no flash params")?;
    do_flash_plan(file, base, partition, &pit, &params)
}

fn shell_tar_list(args: &[&str]) -> Result<(), Box<dyn Error>> {
    let path = *args.first().ok_or("usage: tar-list <archive>")?;
    for e in &list_tar(std::fs::File::open(path)?)? {
        let note = if e.name.ends_with(".lz4") {
            "  (LZ4)"
        } else {
            ""
        };
        println!("  {:>12} bytes  {}{note}", e.size, e.name);
    }
    Ok(())
}

fn cmd_tar_list(path: Option<&String>) -> Result<(), Box<dyn Error>> {
    let path = path.ok_or("usage: thor tar-list <archive.tar[.md5]>")?;
    let entries = list_tar(std::fs::File::open(path)?)?;
    if entries.is_empty() {
        println!("No top-level files in {path}.");
        return Ok(());
    }
    println!("{} file(s) in {path}:", entries.len());
    for e in &entries {
        let note = if e.name.ends_with(".lz4") {
            "  (LZ4-compressed)"
        } else {
            ""
        };
        println!("  {:>12} bytes  {}{note}", e.size, e.name);
    }
    Ok(())
}

/// Connect to the first Samsung device and begin an Odin session.
fn open_session() -> Result<Odin<NusbTransport>, Box<dyn Error>> {
    let devices = backend::list_samsung_devices()?;
    let device = devices
        .first()
        .ok_or("no Samsung device found — is one connected in download mode?")?;
    println!(
        "Connecting to {} (id {})",
        device.display_name, device.identifier
    );

    let transport = NusbTransport::open(device)?;
    let mut odin = Odin::new(transport);
    odin.handshake().map_err(|e| {
        format!(
            "{e}\nhint: if this timed out, reboot the device back into download mode — a \
             Samsung download-mode connection can't be reused after a prior attempt"
        )
    })?;
    let v = odin.begin_session()?;
    println!(
        "Odin session started (bootloader version {}, unknown1={}, unknown2={})",
        v.version, v.unknown1, v.unknown2
    );
    if v.unknown1 != 0 || v.unknown2 != 0 {
        println!("note: unknown1/unknown2 are non-zero — possible undiscovered capabilities");
    }
    Ok(odin)
}

fn cmd_list() -> Result<(), Box<dyn Error>> {
    let devices = backend::list_samsung_devices()?;
    if devices.is_empty() {
        println!("No Samsung devices found.");
        return Ok(());
    }
    println!("Found {} Samsung device(s):", devices.len());
    for d in &devices {
        println!(
            "  {} — {}  (VID {:04x}, PID {:04x})",
            d.identifier, d.display_name, d.vendor_id, d.product_id
        );
    }
    Ok(())
}

fn cmd_dump_pit(args: &[String]) -> Result<(), Box<dyn Error>> {
    let path = args
        .first()
        .ok_or("usage: thor dump-pit <output.pit> [--reboot | --reboot-download | --shutdown]")?;
    let finish = parse_finish(args.get(1).map(String::as_str))?;
    let mut odin = open_session()?;
    let pit = odin.dump_pit()?;
    std::fs::write(path, &pit)?;
    println!("Dumped {} bytes of PIT to {path}", pit.len());
    finish_session(odin, finish)
}

/// What to do with the session when a command finishes.
enum Finish {
    Leave,
    RebootNormal,
    RebootDownload,
    Shutdown,
}

fn parse_finish(flag: Option<&str>) -> Result<Finish, Box<dyn Error>> {
    Ok(match flag {
        None => Finish::Leave,
        Some("--reboot") => Finish::RebootNormal,
        Some("--reboot-download") => Finish::RebootDownload,
        Some("--shutdown") => Finish::Shutdown,
        Some(o) => {
            return Err(format!(
                "unknown option '{o}' (use --reboot | --reboot-download | --shutdown)"
            )
            .into())
        }
    })
}

/// End a session and optionally reboot/shutdown the device, so it isn't left stuck in
/// download mode. A download-mode connection can't be reused across invocations, so this is
/// the way to leave the device usable after a command.
fn finish_session(mut odin: Odin<NusbTransport>, finish: Finish) -> Result<(), Box<dyn Error>> {
    match finish {
        Finish::Leave => println!("(device left in download mode — `thor reboot` to exit)"),
        Finish::RebootNormal => {
            let _ = odin.end_session();
            odin.reboot()?;
            println!("Rebooting into normal mode.");
        }
        Finish::RebootDownload => {
            let _ = odin.end_session();
            if odin.reboot_to_odin().is_err() {
                println!("This device doesn't support reboot-to-download; doing a normal reboot.");
                odin.reboot()?;
            } else {
                println!("Rebooting into download mode.");
            }
        }
        Finish::Shutdown => {
            if odin.shutdown().is_err() {
                let _ = odin.end_session();
            }
            println!("Device shutting down.");
        }
    }
    Ok(())
}

fn cmd_reboot(mode: Option<&String>) -> Result<(), Box<dyn Error>> {
    let finish = match mode.map(String::as_str) {
        None | Some("normal") => Finish::RebootNormal,
        Some("download") => Finish::RebootDownload,
        Some(o) => return Err(format!("unknown reboot mode '{o}' (use normal | download)").into()),
    };
    finish_session(open_session()?, finish)
}

fn cmd_end() -> Result<(), Box<dyn Error>> {
    finish_session(open_session()?, Finish::Shutdown)
}

fn cmd_print_pit(path: Option<&String>) -> Result<(), Box<dyn Error>> {
    let pit = match path {
        Some(p) => PitData::parse(&std::fs::read(p)?)?,
        None => {
            let mut odin = open_session()?;
            let bytes = odin.dump_pit()?;
            println!("(device left in download mode — reboot it to exit)\n");
            PitData::parse(&bytes)?
        }
    };
    print_pit(&pit);
    Ok(())
}

/// The last path component of `path` (e.g. `boot.img.lz4`), used to recognise archive and
/// `.lz4` suffixes and to match a partition by its expected file name.
fn basename(path: &str) -> &str {
    std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
}

/// The "no matching partition" error, listing every partition and its expected file name so
/// the user can pick a valid target.
fn available_partitions_err(pit: &PitData) -> Box<dyn Error> {
    let names: Vec<String> = pit
        .entries
        .iter()
        .map(|e| format!("{} ({})", e.partition, e.file_name))
        .collect();
    format!(
        "no matching partition. Available:\n  {}",
        names.join("\n  ")
    )
    .into()
}

/// Dry run: connect, read the PIT, and show exactly what flashing `file` onto its partition
/// would do — sequences, parts, sizes — **without writing anything**.
fn cmd_flash_plan(args: &[String]) -> Result<(), Box<dyn Error>> {
    let mut pit_file: Option<&str> = None;
    let mut positional: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--pit" => {
                pit_file = Some(
                    args.get(i + 1)
                        .map(String::as_str)
                        .ok_or("--pit needs a file path")?,
                );
                i += 2;
            }
            other => {
                positional.push(other);
                i += 1;
            }
        }
    }
    let file = *positional
        .first()
        .ok_or("usage: thor flash-plan [--pit <pit>] <file> [partition]")?;
    let partition = positional.get(1).copied();
    run_flash_plan(file, basename(file), partition, pit_file)
}

/// Resolve the PIT + flash params (live from the device, or offline from `--pit`) and print
/// the dry-run plan. Shared by `flash-plan` and `flash` (without `--execute`).
fn run_flash_plan(
    file: &str,
    base: &str,
    partition: Option<&str>,
    pit_file: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let (pit, params) = if let Some(pf) = pit_file {
        println!("Offline planning (no device): assuming new-generation bootloader params.\n");
        (
            PitData::parse(&std::fs::read(pf)?)?,
            FlashParams::for_bootloader_version(3),
        )
    } else {
        let mut odin = open_session()?;
        let pit = PitData::parse(&odin.dump_pit()?)?;
        println!("(device left in download mode — reboot it to exit)\n");
        let params = odin.params().ok_or("session has no flash params")?;
        (pit, params)
    };
    do_flash_plan(file, base, partition, &pit, &params)
}

/// Flash `file` to its partition. Without `--execute` this is a dry run (identical to
/// `flash-plan`); with `--execute` it writes to the connected device after a typed
/// confirmation.
fn cmd_flash(args: &[String]) -> Result<(), Box<dyn Error>> {
    let mut pit_file: Option<&str> = None;
    let mut execute = false;
    let mut assume_yes = false;
    let mut tflash = false;
    let mut finish_flag: Option<&str> = None;
    let mut positional: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--pit" => {
                pit_file = Some(
                    args.get(i + 1)
                        .map(String::as_str)
                        .ok_or("--pit needs a file path")?,
                );
                i += 2;
            }
            "--tflash" => {
                tflash = true;
                i += 1;
            }
            "--execute" => {
                execute = true;
                i += 1;
            }
            "--yes" | "-y" => {
                assume_yes = true;
                i += 1;
            }
            f @ ("--reboot" | "--reboot-download" | "--shutdown") => {
                finish_flag = Some(f);
                i += 1;
            }
            other => {
                positional.push(other);
                i += 1;
            }
        }
    }
    let file = *positional.first().ok_or(
        "usage: thor flash [--pit <pit>] [--execute] [--yes] \
         [--reboot|--reboot-download|--shutdown] <file> [partition]",
    )?;
    let partition = positional.get(1).copied();
    let base = basename(file);

    if !execute {
        if finish_flag.is_some() {
            eprintln!(
                "note: reboot/shutdown flags are ignored without --execute (a dry run never \
                 touches the device)"
            );
        }
        return run_flash_plan(file, base, partition, pit_file);
    }

    if pit_file.is_some() {
        return Err(
            "--pit can't be combined with --execute: a live flash uses the device's own PIT".into(),
        );
    }
    do_flash_live(
        file,
        base,
        partition,
        parse_finish(finish_flag)?,
        assume_yes,
        tflash,
    )
}

/// **Destructive.** Flash a single image (or dispatch a whole archive) to the connected
/// device, after a typed confirmation. With `tflash`, T-Flash mode is enabled first so the
/// flash targets an inserted microSD instead of internal storage.
fn do_flash_live(
    file: &str,
    base: &str,
    partition: Option<&str>,
    finish: Finish,
    assume_yes: bool,
    tflash: bool,
) -> Result<(), Box<dyn Error>> {
    let mut odin = open_session()?;
    let pit = PitData::parse(&odin.dump_pit()?)?;
    let params = odin.params().ok_or("session has no flash params")?;

    if tflash {
        println!("Enabling T-Flash — the flash will target an inserted microSD card…");
        odin.enable_tflash()?;
    }

    if base.ends_with(".tar") || base.ends_with(".tar.md5") {
        if partition.is_some() {
            return Err(
                "a partition name can't be given for a whole-archive flash — every \
                        image is matched to its own partition"
                    .into(),
            );
        }
        return flash_archive_live(file, odin, &pit, &params, finish, assume_yes);
    }

    let entry = match_partition(&pit, base, partition)
        .ok_or_else(|| available_partitions_err(&pit))?
        .clone();
    let (mut source, length) = open_flash_source(file, base)?;

    println!("\nAbout to FLASH — this WRITES to the device:");
    let target = format!(
        "{} (id {}, binaryType {}, deviceType {})",
        entry.partition, entry.partition_id, entry.binary_type, entry.device_type
    );
    print_partition_plan(&target, length, &params);

    if !confirm_flash(&entry.partition, assume_yes)? {
        println!("Aborted — nothing was written.");
        return finish_session(odin, Finish::Leave);
    }

    println!("\nFlashing {}…", entry.partition);
    odin.set_total_bytes(length)?;
    let width = 30usize;
    odin.flash_partition(Some(&mut *source), &entry, length, |p| {
        let bar = progress::progress_bar(p.sent_bytes, p.total_bytes, width);
        let state = match p.state {
            FlashState::Sending => "send ",
            FlashState::Flashing => "flash",
        };
        print!(
            "\r  {bar}  seq {}/{}  {state}  {:>10}",
            p.sequence_index + 1,
            p.total_sequences,
            progress::human_bytes(p.sent_bytes.min(p.total_bytes)),
        );
        let _ = std::io::stdout().flush();
    })?;
    println!(
        "\r  {}  done{:<24}",
        progress::progress_bar(length, length, width),
        ""
    );
    println!(
        "Flashed {} ({}).",
        entry.partition,
        progress::human_bytes(length)
    );

    finish_session(odin, finish)
}

/// **Destructive.** Flash every image in an Odin `.tar` / `.tar.md5` that matches a PIT
/// partition, in one session. Two passes: first resolve every image's on-device size (so the
/// total is announced up front), then stream each matched image to its partition.
fn flash_archive_live(
    file: &str,
    mut odin: Odin<NusbTransport>,
    pit: &PitData,
    params: &FlashParams,
    finish: Finish,
    assume_yes: bool,
) -> Result<(), Box<dyn Error>> {
    // Pass 1: match each image to a partition and sum the real (on-device) sizes.
    let images = list_archive_images(std::fs::File::open(file)?)?;
    let mut targets: Vec<(String, PitEntry, i64)> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for img in &images {
        match match_archive_image(pit, &img.name) {
            Some(entry) => targets.push((img.name.clone(), entry.clone(), img.real_size as i64)),
            None => skipped.push(img.name.clone()),
        }
    }
    if targets.is_empty() {
        return Err("no image in the archive matches a PIT partition — nothing to flash".into());
    }
    let total: i64 = targets.iter().map(|(_, _, sz)| *sz).sum();

    println!(
        "\nAbout to FLASH {} image(s) from {file} — this WRITES to the device:",
        targets.len()
    );
    let mut any_critical = false;
    for (name, entry, sz) in &targets {
        print_partition_plan(
            &format!("{} (id {})  <= {name}", entry.partition, entry.partition_id),
            *sz,
            params,
        );
        any_critical |= progress::is_critical_partition(&entry.partition);
    }
    for s in &skipped {
        println!("  {s}  — no matching PIT partition (skipped)");
    }
    println!(
        "\nTotal: {} across {} partition(s).",
        progress::human_bytes(total),
        targets.len()
    );

    if !confirm_flash_archive(any_critical, assume_yes)? {
        println!("Aborted — nothing was written.");
        return finish_session(odin, Finish::Leave);
    }

    odin.set_total_bytes(total)?;

    // Pass 2: stream each matched image to its partition. The lookup pairs an entry name with
    // its partition + on-device length resolved in pass 1.
    let lookup: std::collections::HashMap<&str, (&PitEntry, i64)> = targets
        .iter()
        .map(|(name, entry, sz)| (name.as_str(), (entry, *sz)))
        .collect();
    let width = 30usize;
    let mut done = 0usize;
    for_each_image(
        std::fs::File::open(file)?,
        |name, reader| -> Result<(), Box<dyn Error>> {
            if let Some((entry, length)) = lookup.get(name).copied() {
                println!(
                    "\n[{}/{}] {} <= {name}  ({})",
                    done + 1,
                    targets.len(),
                    entry.partition,
                    progress::human_bytes(length)
                );
                odin.flash_partition(Some(reader), entry, length, |p| {
                    print!(
                        "\r  {}  seq {}/{}",
                        progress::progress_bar(p.sent_bytes, p.total_bytes, width),
                        p.sequence_index + 1,
                        p.total_sequences
                    );
                    let _ = std::io::stdout().flush();
                })?;
                println!(
                    "\r  {}  done{:<24}",
                    progress::progress_bar(length, length, width),
                    ""
                );
                done += 1;
            }
            Ok(())
        },
    )?;

    println!(
        "\nFlashed {}/{} image(s), {} total.",
        done,
        targets.len(),
        progress::human_bytes(total)
    );
    finish_session(odin, finish)
}

/// Confirm a whole-archive flash. Typing each partition name is impractical for many images,
/// so this gate requires typing the literal word `FLASH` (see [`confirm_typed`]).
fn confirm_flash_archive(any_critical: bool, assume_yes: bool) -> Result<bool, Box<dyn Error>> {
    let warn = any_critical.then_some(
        "this archive includes a bootloader/critical partition — a bad flash here can \
         HARD-BRICK the device.",
    );
    confirm_typed("FLASH", warn, assume_yes)
}

/// Shared confirmation gate for a destructive action keyed on a literal `word`: prints an
/// optional `warning`, then requires the user to type `word` exactly (case-sensitive) — a
/// stronger gate than y/n. `--yes` skips it (for automation); a non-interactive stdin is
/// refused so nothing destructive ever runs unattended without `--yes`.
fn confirm_typed(
    word: &str,
    warning: Option<&str>,
    assume_yes: bool,
) -> Result<bool, Box<dyn Error>> {
    if assume_yes {
        println!("(--yes given: skipping confirmation)");
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err(
            "refusing: stdin isn't a terminal and --yes wasn't given, so the confirmation \
             can't be answered"
                .into(),
        );
    }
    if let Some(w) = warning {
        println!("\nWARNING: {w}");
    }
    print!("\nType {word} to proceed (anything else aborts): ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim() == word)
}

/// Flags shared by the standalone destructive commands (factory-reset / set-region).
struct ActionArgs<'a> {
    execute: bool,
    assume_yes: bool,
    finish: Option<&'a str>,
    positional: Vec<&'a str>,
}

fn parse_action_args(args: &[String]) -> Result<ActionArgs<'_>, Box<dyn Error>> {
    let mut a = ActionArgs {
        execute: false,
        assume_yes: false,
        finish: None,
        positional: Vec::new(),
    };
    for arg in args {
        match arg.as_str() {
            "--execute" => a.execute = true,
            "--yes" | "-y" => a.assume_yes = true,
            s @ ("--reboot" | "--reboot-download" | "--shutdown") => a.finish = Some(s),
            other => a.positional.push(other),
        }
    }
    Ok(a)
}

/// **Destructive.** Factory-reset the device — wipe the userdata (`/data`) partition. Gated:
/// needs `--execute` and a typed `ERASE`.
fn cmd_factory_reset(args: &[String]) -> Result<(), Box<dyn Error>> {
    let a = parse_action_args(args)?;
    if !a.execute {
        return Err(
            "factory-reset wipes /data — re-run with --execute (and confirm) to do it".into(),
        );
    }
    let mut odin = open_session()?;
    println!("\nAbout to FACTORY RESET — this wipes the /data (userdata) partition.");
    let warn = "on a device with an unlocked bootloader this can trip Samsung's VaultKeeper, \
                which re-locks the bootloader after /data is wiped until you finish setup online.";
    if !confirm_typed("ERASE", Some(warn), a.assume_yes)? {
        println!("Aborted — nothing was erased.");
        return finish_session(odin, Finish::Leave);
    }
    println!("\nErasing userdata (this can take a few minutes)…");
    odin.erase_user_data()?;
    println!("Factory reset complete.");
    finish_session(odin, parse_finish(a.finish)?)
}

/// **Destructive.** Zero-fill a partition — the C#'s "erase partition", built on the flash
/// engine with an empty source. Requires `--size <bytes>` (the PIT carries no reliable
/// partition size to infer, so the caller states how many zero bytes to write).
fn cmd_erase(args: &[String]) -> Result<(), Box<dyn Error>> {
    let mut execute = false;
    let mut assume_yes = false;
    let mut finish_flag: Option<&str> = None;
    let mut size: Option<i64> = None;
    let mut positional: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--execute" => {
                execute = true;
                i += 1;
            }
            "--yes" | "-y" => {
                assume_yes = true;
                i += 1;
            }
            "--size" => {
                let v = args.get(i + 1).ok_or("--size needs a byte count")?;
                size = Some(
                    v.parse()
                        .map_err(|_| "invalid --size (want a byte count)")?,
                );
                i += 2;
            }
            f @ ("--reboot" | "--reboot-download" | "--shutdown") => {
                finish_flag = Some(f);
                i += 1;
            }
            other => {
                positional.push(other);
                i += 1;
            }
        }
    }
    let partition = *positional
        .first()
        .ok_or("usage: thor erase <partition> --size <bytes> --execute [--yes]")?;
    let size = size.ok_or("erase needs --size <bytes> (the number of zero bytes to write)")?;
    if size <= 0 {
        return Err("--size must be positive".into());
    }
    if !execute {
        return Err(format!(
            "erase would zero-fill {partition} with {size} bytes — re-run with --execute to do it"
        )
        .into());
    }

    let mut odin = open_session()?;
    let pit = PitData::parse(&odin.dump_pit()?)?;
    let params = odin.params().ok_or("session has no flash params")?;
    let entry = match_partition(&pit, partition, Some(partition))
        .ok_or_else(|| available_partitions_err(&pit))?
        .clone();

    println!("\nAbout to ERASE (zero-fill) — this WRITES zeros to the device:");
    print_partition_plan(
        &format!("{} (id {})", entry.partition, entry.partition_id),
        size,
        &params,
    );
    if !confirm_flash(&entry.partition, assume_yes)? {
        println!("Aborted — nothing was written.");
        return finish_session(odin, Finish::Leave);
    }

    println!("\nErasing {}…", entry.partition);
    odin.set_total_bytes(size)?;
    let width = 30usize;
    odin.flash_partition(None, &entry, size, |p| {
        print!(
            "\r  {}  seq {}/{}",
            progress::progress_bar(p.sent_bytes, p.total_bytes, width),
            p.sequence_index + 1,
            p.total_sequences
        );
        let _ = std::io::stdout().flush();
    })?;
    println!(
        "\r  {}  done{:<24}",
        progress::progress_bar(size, size, width),
        ""
    );
    println!(
        "Erased {} ({}).",
        entry.partition,
        progress::human_bytes(size)
    );
    finish_session(odin, parse_finish(finish_flag)?)
}

/// Set the device's region (CSC) code. **UNVERIFIED** — in the reference tool this shares
/// opcode `0x08` with T-Flash enable (see roadmap F8), so it may enable T-Flash instead.
/// Gated behind `--execute` and a typed `YES`.
fn cmd_set_region(args: &[String]) -> Result<(), Box<dyn Error>> {
    let a = parse_action_args(args)?;
    let code = a
        .positional
        .first()
        .copied()
        .ok_or("usage: thor set-region <XAA> --execute [--yes]")?;
    if code.len() != 3 {
        return Err("a region code is exactly 3 characters (e.g. XAA)".into());
    }
    let code = code.to_ascii_uppercase();
    if !a.execute {
        return Err(format!("would set region to {code} — re-run with --execute to do it").into());
    }
    let mut odin = open_session()?;
    let warn = "UNVERIFIED — in the reference tool 'set region' shares opcode 0x08 with \
                T-Flash enable (likely a bug), so this may enable T-Flash rather than change \
                the region. See docs/port/roadmap.md (F8).";
    if !confirm_typed("YES", Some(warn), a.assume_yes)? {
        println!("Aborted — region unchanged.");
        return finish_session(odin, Finish::Leave);
    }
    odin.set_region_code(&code)?;
    println!("Region code set to {code}.");
    finish_session(odin, parse_finish(a.finish)?)
}

/// Connect to a Samsung device that is in **upload mode** (SUC), confirming with the preamble
/// handshake. Same USB shape as download mode, different protocol.
fn open_upload() -> Result<Upload<NusbTransport>, Box<dyn Error>> {
    let devices = backend::list_samsung_devices()?;
    let device = devices
        .first()
        .ok_or("no Samsung device found — is one connected in upload mode?")?;
    println!(
        "Connecting to {} (id {})",
        device.display_name, device.identifier
    );
    let transport = NusbTransport::open(device)?;
    let mut up = Upload::new(transport);
    up.handshake()?;
    println!("Upload mode confirmed.");
    Ok(up)
}

/// Parse a hex address, with or without a `0x` prefix.
fn parse_hex(s: &str) -> Result<u64, Box<dyn Error>> {
    let t = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    u64::from_str_radix(t, 16).map_err(|_| format!("invalid hex address '{s}'").into())
}

/// List the dumpable memory regions of a device in upload mode. **Read-only.**
fn cmd_upload_probe() -> Result<(), Box<dyn Error>> {
    let mut up = open_upload()?;
    let regions = up.probe()?;
    if regions.is_empty() {
        println!("No regions reported (some bootloaders don't expose a probe table).");
        return Ok(());
    }
    println!("{} region(s):", regions.len());
    for r in &regions {
        println!(
            "  {:<16} {:#018x}..{:#018x}  ({})",
            r.name,
            r.start,
            r.end,
            progress::human_bytes(r.size() as i64)
        );
    }
    Ok(())
}

/// Dump a memory range from a device in upload mode to a file. **Read-only** (reads RAM).
fn cmd_upload_dump(args: &[String]) -> Result<(), Box<dyn Error>> {
    let usage = "usage: thor upload-dump <start> <end> <outfile>  (addresses in hex)";
    let start = parse_hex(args.first().ok_or(usage)?)?;
    let end = parse_hex(args.get(1).ok_or(usage)?)?;
    let out = args.get(2).ok_or(usage)?;
    if end <= start {
        return Err("end address must be greater than start".into());
    }
    let total = (end - start) as i64;

    let mut up = open_upload()?;
    let mut file = std::fs::File::create(out)?;
    println!(
        "Dumping {} from {start:#x}..{end:#x} to {out}…",
        progress::human_bytes(total)
    );
    let width = 30usize;
    let dumped = up.dump_range(
        start,
        end,
        &mut |chunk| {
            file.write_all(chunk)
                .map_err(|e| UploadError::Protocol(format!("writing dump: {e}")))
        },
        &mut |pos, tot| {
            print!(
                "\r  {}  {:>12}",
                progress::progress_bar(pos as i64, tot as i64, width),
                progress::human_bytes(pos as i64)
            );
            let _ = std::io::stdout().flush();
        },
    )?;
    println!(
        "\nDumped {} to {out}. (carve dmesg from it offline — the printk ring buffer lives in RAM)",
        progress::human_bytes(dumped as i64)
    );
    Ok(())
}

/// Reboot a device out of upload mode.
fn cmd_upload_reboot() -> Result<(), Box<dyn Error>> {
    let mut up = open_upload()?;
    up.power_down()?;
    println!("Rebooting out of upload mode.");
    Ok(())
}

/// Open `file` as a flash source, returning the reader and the number of bytes that will
/// land on the device. A `.lz4` file is decompressed on the fly, and its on-device length is
/// read from the frame's content-size header.
fn open_flash_source(file: &str, base: &str) -> Result<(Box<dyn Read>, i64), Box<dyn Error>> {
    let mut f = std::fs::File::open(file)?;
    if base.ends_with(".lz4") {
        let mut hdr = [0u8; 16];
        let n = f.read(&mut hdr)?;
        let size = lz4_content_size(&hdr[..n]).ok_or(
            "this .lz4 has no content-size header, so the on-device length is unknown — \
             decompress it first and flash the raw image",
        )? as i64;
        f.seek(std::io::SeekFrom::Start(0))?;
        Ok((Box::new(lz4_stream_reader(f)), size))
    } else {
        let size = f.metadata()?.len() as i64;
        Ok((Box::new(f), size))
    }
}

/// Gate a live flash behind an explicit, deliberate confirmation. Unless `--yes` was given,
/// the user must type the exact partition name (a stronger gate than y/n — it forces naming
/// what's about to be overwritten). Refuses to proceed on a non-interactive stdin.
fn confirm_flash(partition: &str, assume_yes: bool) -> Result<bool, Box<dyn Error>> {
    if assume_yes {
        println!("(--yes given: skipping confirmation)");
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err(
            "refusing to flash: stdin isn't a terminal and --yes wasn't given, so the \
             confirmation can't be answered"
                .into(),
        );
    }
    if progress::is_critical_partition(partition) {
        println!(
            "\nWARNING: {partition} is a bootloader/critical partition — a bad flash here can \
             HARD-BRICK the device."
        );
    }
    print!("\nType the partition name '{partition}' to flash it (anything else aborts): ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().eq_ignore_ascii_case(partition))
}

/// Print the dry-run plan for `file` against a PIT — a single image or a whole archive.
/// Shared by the one-shot CLI and the interactive shell.
fn do_flash_plan(
    file: &str,
    base: &str,
    partition: Option<&str>,
    pit: &PitData,
    params: &FlashParams,
) -> Result<(), Box<dyn Error>> {
    println!("DRY RUN — no data will be written to the device.");
    println!(
        "Packet:  {} bytes/part; one sequence = {} bytes\n",
        params.packet_size,
        params.sequence_size()
    );

    // Whole-archive planning for Odin .tar / .tar.md5 packages.
    if base.ends_with(".tar") || base.ends_with(".tar.md5") {
        return plan_archive(file, pit, params);
    }

    // Single image (optionally .lz4).
    let mut len = std::fs::metadata(file)?.len() as i64;
    if base.ends_with(".lz4") {
        let mut hdr = [0u8; 16];
        let n = std::fs::File::open(file)?.read(&mut hdr)?;
        match lz4_content_size(&hdr[..n]) {
            Some(sz) => {
                println!("LZ4: {len} compressed bytes → {sz} bytes on device (frame header)");
                len = sz as i64;
            }
            None => println!(
                "note: {base} is LZ4 but carries no content-size header; planning against the \
                 compressed size — the real on-device size will differ."
            ),
        }
    }
    let entry = match_partition(pit, base, partition).ok_or_else(|| {
        let names: Vec<String> = pit
            .entries
            .iter()
            .map(|e| format!("{} ({})", e.partition, e.file_name))
            .collect();
        format!(
            "no matching partition. Available:\n  {}",
            names.join("\n  ")
        )
    })?;
    let target = format!(
        "{} (id {}, binaryType {}, deviceType {})",
        entry.partition, entry.partition_id, entry.binary_type, entry.device_type
    );
    print_partition_plan(&target, len, params);
    Ok(())
}

/// Print the sequence/part plan for a single partition of `len` on-device bytes.
fn print_partition_plan(target: &str, len: i64, params: &FlashParams) {
    let plan = plan_flash(len, params);
    let on_wire: i64 = plan.iter().map(|s| s.aligned_size).sum();
    println!("{target}");
    println!(
        "    {len} B → {} sequence(s), {on_wire} B on the wire",
        plan.len()
    );
    for s in &plan {
        println!(
            "    seq {:>3}: {:>4} part(s), real {:>10} B, on-wire {:>10} B{}",
            s.index,
            s.parts,
            s.real_size,
            s.aligned_size,
            if s.is_last { "  (last)" } else { "" }
        );
    }
}

/// Plan every image in an Odin archive against the PIT, resolving `.lz4` real sizes.
fn plan_archive(file: &str, pit: &PitData, params: &FlashParams) -> Result<(), Box<dyn Error>> {
    let images = list_archive_images(std::fs::File::open(file)?)?;
    println!("Archive: {file} — {} image(s)\n", images.len());
    let mut matched = 0;
    for img in &images {
        match match_archive_image(pit, &img.name) {
            Some(entry) => {
                matched += 1;
                let comp = if img.compressed {
                    format!("  [lz4 {}→{} B]", img.stored_size, img.real_size)
                } else {
                    String::new()
                };
                let target = format!("{} (id {}){comp}", entry.partition, entry.partition_id);
                print_partition_plan(&target, img.real_size as i64, params);
            }
            None => println!("{}  — no matching PIT partition (skipped)", img.name),
        }
    }
    println!(
        "\n{matched}/{} image(s) matched to a partition.",
        images.len()
    );
    Ok(())
}

/// Match a tar entry name to a PIT partition — direct, then with a trailing `.lz4` stripped.
fn match_archive_image<'a>(pit: &'a PitData, entry_name: &str) -> Option<&'a PitEntry> {
    pit.entries
        .iter()
        .find(|e| e.file_name == entry_name)
        .or_else(|| {
            entry_name
                .strip_suffix(".lz4")
                .and_then(|n| pit.entries.iter().find(|e| e.file_name == n))
        })
}

fn match_partition<'a>(
    pit: &'a PitData,
    file_base: &str,
    explicit: Option<&str>,
) -> Option<&'a PitEntry> {
    match explicit {
        Some(name) => pit
            .entries
            .iter()
            .find(|e| e.partition.eq_ignore_ascii_case(name)),
        None => pit.entries.iter().find(|e| e.file_name == file_base),
    }
}

/// Render a PIT as an indented tree, matching the field order of the reference C# output.
fn print_pit(pit: &PitData) {
    let m = FieldMapper::for_version(pit.is_new_version);
    println!("PIT File");
    println!("  Header");
    println!("    Unknown string: {}", pit.unknown);
    println!("    Project name: {}", pit.project);
    println!(
        "    Version: {}",
        if pit.is_new_version {
            "v2 (new)"
        } else {
            "v1 (old)"
        }
    );
    println!("    Reserved: {}", pit.reserved);
    for (i, e) in pit.entries.iter().enumerate() {
        println!("  Entry #{i}");
        let row = |label: &str, desc: &str, raw: i32| {
            println!("    {label}: {desc} ({raw})");
        };
        row(
            m.update_attributes.label,
            m.update_attributes.describe(e.update_attributes),
            e.update_attributes,
        );
        row(
            m.attributes.label,
            m.attributes.describe(e.attributes),
            e.attributes,
        );
        row(
            m.binary_type.label,
            m.binary_type.describe(e.binary_type),
            e.binary_type,
        );
        row(
            m.device_type.label,
            m.device_type.describe(e.device_type),
            e.device_type,
        );
        println!("    {}: {}", m.block_size_label, e.block_size);
        println!("    {}: {}", m.block_count_label, e.block_count);
        println!("    Partition Name: {}", e.partition);
        println!("    Partition ID: {}", e.partition_id);
        println!("    File Offset: {}", e.file_offset);
        println!("    File Size: {}", e.file_size);
        println!("    File Name: {}", e.file_name);
        if e.delta_name.is_empty() {
            println!("    Empty Delta Name");
        } else {
            println!("    Delta Name: {}", e.delta_name);
        }
    }
}
