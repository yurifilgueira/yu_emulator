//! Headless harness.
//!
//! Phase 0: loads a BIOS, checks it, and reads the reset vector through the bus.
//! Later phases add: `run` (execute N cycles), `exe` (sideload a PS-EXE test),
//! `vram-hash` (compare GPU output against a reference) and `suite`
//! (run every test under tests/ and print a pass/fail table).

use psx_core::{map, Bus};
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  harness bios <path-to-bios.bin>   check a BIOS image and read the reset vector"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, path] if cmd == "bios" => cmd_bios(path),
        _ => usage(),
    }
}

fn cmd_bios(path: &str) -> ExitCode {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("FAIL  could not read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut bus = match Bus::new(data) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("FAIL  {e}");
            return ExitCode::FAILURE;
        }
    };

    let first = bus.load32(map::RESET_VECTOR);
    println!("OK    BIOS loaded ({} KB)", map::BIOS_SIZE / 1024);
    println!("      reset vector 0x{:08X} -> 0x{first:08X}", map::RESET_VECTOR);

    // Sony BIOSes and OpenBIOS both start by touching memory control
    // registers; a "lui" (opcode 0x0F) is the usual first instruction.
    if first >> 26 == 0x0F {
        println!("      first instruction is LUI, looks like a real BIOS");
    } else {
        println!("WARN  first instruction is not LUI; double-check the image");
    }
    ExitCode::SUCCESS
}
