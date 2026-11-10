//! Headless harness.
//!
//! - `bios`: loads a BIOS, checks it, and reads the reset vector.
//! - `run`: boots a BIOS for N cycles, printing the kernel's TTY output.
//!
//! Later phases add: `exe` (sideload a PS-EXE test), `vram-hash` (compare GPU
//! output against a reference) and `suite` (run every test under tests/ and
//! print a pass/fail table).

use psx_core::disasm::disassemble;
use psx_core::{map, Bus, Psx};
use std::io::Write;
use std::process::ExitCode;

const DEFAULT_CYCLES: u64 = 100_000_000;

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  \
         harness bios <bios.bin>                      check a BIOS image and read the reset vector\n  \
         harness run <bios.bin> [--cycles N] [--trace]\n      \
         boot the BIOS for N cycles (default {DEFAULT_CYCLES}); TTY output goes to stdout,\n      \
         the instruction trace (--trace) and the final report go to stderr"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, path] if cmd == "bios" => cmd_bios(path),
        [cmd, path, options @ ..] if cmd == "run" => match RunOptions::parse(options) {
            Some(options) => cmd_run(path, options),
            None => usage(),
        },
        _ => usage(),
    }
}

fn read_bios(path: &str) -> Result<Vec<u8>, ExitCode> {
    std::fs::read(path).map_err(|e| {
        eprintln!("FAIL  could not read {path}: {e}");
        ExitCode::FAILURE
    })
}

fn cmd_bios(path: &str) -> ExitCode {
    let data = match read_bios(path) {
        Ok(d) => d,
        Err(code) => return code,
    };
    let mut bus = match Bus::new(data) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("FAIL  {e}");
            return ExitCode::FAILURE;
        }
    };

    let first = bus.load32(map::RESET_VECTOR).expect("BIOS is always mapped");
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

struct RunOptions {
    cycles: u64,
    trace: bool,
}

impl RunOptions {
    fn parse(args: &[String]) -> Option<Self> {
        let mut options = Self { cycles: DEFAULT_CYCLES, trace: false };
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--cycles" => options.cycles = args.next()?.replace('_', "").parse().ok()?,
                "--trace" => options.trace = true,
                _ => return None,
            }
        }
        Some(options)
    }
}

fn cmd_run(path: &str, options: RunOptions) -> ExitCode {
    let data = match read_bios(path) {
        Ok(d) => d,
        Err(code) => return code,
    };
    let mut psx = match Psx::new(data) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("FAIL  {e}");
            return ExitCode::FAILURE;
        }
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut tty_bytes = 0usize;

    while psx.cpu.cycles() < options.cycles {
        psx.step();
        if options.trace {
            let (pc, word) = (psx.cpu.current_pc(), psx.cpu.last_instr());
            eprintln!("{pc:08X}  {word:08X}  {}", disassemble(pc, word));
        }
        let tty = psx.take_tty();
        if !tty.is_empty() {
            tty_bytes += tty.len();
            // Ignore a closed pipe (e.g. `| head`).
            let _ = out.write_all(&tty).and_then(|()| out.flush());
        }
    }

    eprintln!("\n---- stopped after {} cycles ----", psx.cpu.cycles());
    eprintln!("pc 0x{:08X}   TTY bytes {tty_bytes}", psx.cpu.pc());
    eprintln!(
        "sr 0x{:08X}   cause 0x{:08X}   epc 0x{:08X}   badvaddr 0x{:08X}",
        psx.cpu.sr(),
        psx.cpu.cause(),
        psx.cpu.epc(),
        psx.cpu.badvaddr()
    );
    if psx.cpu.gte_unimplemented() > 0 {
        eprintln!("GTE (COP2) instructions ignored: {}", psx.cpu.gte_unimplemented());
    }
    let unhandled = psx.bus.unhandled();
    if !unhandled.is_empty() {
        eprintln!("unhandled bus accesses ({} distinct):", unhandled.len());
        for access in unhandled {
            eprintln!("  {access}");
        }
    }
    ExitCode::SUCCESS
}
