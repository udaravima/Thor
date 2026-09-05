//! Thor CLI (Rust port) — Milestone 1: read-only commands.
//!
//! Subcommands (scriptable, unlike the interactive-only C#):
//!   thor list                 List connected Samsung devices.
//!   thor dump-pit <out.pit>   Connect, begin an Odin session, dump the PIT to a file.
//!   thor print-pit [file]     Print a PIT: from <file> if given, else dumped live.
//!
//! Everything here is non-destructive. Flashing lands in later milestones.

use std::error::Error;
use std::io::Read;
use std::process::ExitCode;

use thor_core::archive::{list_archive_images, list_tar, lz4_content_size};
use thor_core::backend::{self, NusbTransport};
use thor_core::flash::{plan_flash, FlashParams};
use thor_core::odin::Odin;
use thor_core::pit::{FieldMapper, PitData, PitEntry};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("list") => cmd_list(),
        Some("dump-pit") => cmd_dump_pit(&args[2..]),
        Some("print-pit") => cmd_print_pit(args.get(2)),
        Some("tar-list") => cmd_tar_list(args.get(2)),
        Some("flash-plan") => cmd_flash_plan(&args[2..]),
        Some("reboot") => cmd_reboot(args.get(2)),
        Some("end") => cmd_end(),
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
         \x20 thor reboot [normal|download]   Reboot the device (default normal)\n\
         \x20 thor end                  Shut the device down / end the session\n"
    );
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
        let note = if e.name.ends_with(".lz4") { "  (LZ4-compressed)" } else { "" };
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
    println!("Connecting to {} (id {})", device.display_name, device.identifier);

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
            return Err(
                format!("unknown option '{o}' (use --reboot | --reboot-download | --shutdown)")
                    .into(),
            )
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

/// Dry run: connect, read the PIT, and show exactly what flashing `file` onto its partition
/// would do — sequences, parts, sizes — **without writing anything**.
fn cmd_flash_plan(args: &[String]) -> Result<(), Box<dyn Error>> {
    // Optional `--pit <file>` plans offline (no device); otherwise plan live.
    let mut pit_file: Option<&str> = None;
    let mut positional: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--pit" => {
                pit_file = Some(
                    args.get(i + 1).map(String::as_str).ok_or("--pit needs a file path")?,
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

    let base = std::path::Path::new(file)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(file);

    // Get the PIT and flash params — live from the device, or offline from a --pit file
    // (assuming new-generation bootloader params, which we state).
    let (pit, params) = if let Some(pf) = pit_file {
        println!("Offline planning (no device): assuming new-generation bootloader params.\n");
        (PitData::parse(&std::fs::read(pf)?)?, FlashParams::for_bootloader_version(3))
    } else {
        let mut odin = open_session()?;
        let pit = PitData::parse(&odin.dump_pit()?)?;
        println!("(device left in download mode — reboot it to exit)\n");
        let params = odin.params().ok_or("session has no flash params")?;
        (pit, params)
    };

    println!("DRY RUN — no data will be written to the device.");
    println!(
        "Packet:  {} bytes/part; one sequence = {} bytes\n",
        params.packet_size,
        params.sequence_size()
    );

    // Whole-archive planning for Odin .tar / .tar.md5 packages.
    if base.ends_with(".tar") || base.ends_with(".tar.md5") {
        return plan_archive(file, &pit, &params);
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
    let entry = match_partition(&pit, base, partition).ok_or_else(|| {
        let names: Vec<String> = pit
            .entries
            .iter()
            .map(|e| format!("{} ({})", e.partition, e.file_name))
            .collect();
        format!("no matching partition. Available:\n  {}", names.join("\n  "))
    })?;
    let target = format!(
        "{} (id {}, binaryType {}, deviceType {})",
        entry.partition, entry.partition_id, entry.binary_type, entry.device_type
    );
    print_partition_plan(&target, len, &params);
    Ok(())
}

/// Print the sequence/part plan for a single partition of `len` on-device bytes.
fn print_partition_plan(target: &str, len: i64, params: &FlashParams) {
    let plan = plan_flash(len, params);
    let on_wire: i64 = plan.iter().map(|s| s.aligned_size).sum();
    println!("{target}");
    println!("    {len} B → {} sequence(s), {on_wire} B on the wire", plan.len());
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
    println!("\n{matched}/{} image(s) matched to a partition.", images.len());
    Ok(())
}

/// Match a tar entry name to a PIT partition — direct, then with a trailing `.lz4` stripped.
fn match_archive_image<'a>(pit: &'a PitData, entry_name: &str) -> Option<&'a PitEntry> {
    pit.entries.iter().find(|e| e.file_name == entry_name).or_else(|| {
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
        Some(name) => pit.entries.iter().find(|e| e.partition.eq_ignore_ascii_case(name)),
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
        if pit.is_new_version { "v2 (new)" } else { "v1 (old)" }
    );
    println!("    Reserved: {}", pit.reserved);
    for (i, e) in pit.entries.iter().enumerate() {
        println!("  Entry #{i}");
        let row = |label: &str, desc: &str, raw: i32| {
            println!("    {label}: {desc} ({raw})");
        };
        row(m.update_attributes.label, m.update_attributes.describe(e.update_attributes), e.update_attributes);
        row(m.attributes.label, m.attributes.describe(e.attributes), e.attributes);
        row(m.binary_type.label, m.binary_type.describe(e.binary_type), e.binary_type);
        row(m.device_type.label, m.device_type.describe(e.device_type), e.device_type);
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
