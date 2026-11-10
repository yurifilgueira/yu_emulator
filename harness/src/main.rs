//! Headless harness.
//!
//! - `bios`: loads a BIOS, checks it, and reads the reset vector.
//! - `run`: boots a BIOS for N cycles, printing the kernel's TTY output.
//! - `exe`: boots the BIOS until its kernel is ready, then runs a PS-EXE
//!   instead of the shell, optionally comparing its TTY output with a
//!   reference log.
//!
//! Later phases add: `vram-hash` (compare GPU output against a reference)
//! and `suite` (run every test under tests/ and print a pass/fail table).

use psx_core::disasm::disassemble;
use psx_core::exe::Exe;
use psx_core::{map, Bus, Psx};
use std::io::Write;
use std::process::ExitCode;

const DEFAULT_CYCLES: u64 = 100_000_000;

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  \
         harness bios <bios.bin>                      check a BIOS image and read the reset vector\n  \
         harness run <bios.bin> [--cycles N] [--trace] [--trace-from CYCLE]\n      \
         boot the BIOS for N cycles (default {DEFAULT_CYCLES}); TTY output goes to stdout,\n      \
         the instruction trace (--trace) and the final report go to stderr\n  \
         harness exe <bios.bin> <file.exe> [--cycles N] [--trace] [--trace-from CYCLE] [--expect <log>]\n      \
         boot the BIOS up to its shell, then run the PS-EXE for N cycles;\n      \
         with --expect, stop at the reference log's last line and exit 0 only if the\n      \
         output agrees with it: if the log has \"pass - \"/\"fail - \" lines, every test that\n      \
         passes in the log must pass and nothing may fail; otherwise the text must match\n      \
         (blank lines, \"% \" prefixes and ResetGraph: lines are ignored)"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, path] if cmd == "bios" => cmd_bios(path),
        [cmd, path, options @ ..] if cmd == "run" => match RunOptions::parse(options) {
            Some(options) if options.expect.is_none() => cmd_run(path, None, options),
            _ => usage(),
        },
        [cmd, path, exe, options @ ..] if cmd == "exe" => match RunOptions::parse(options) {
            Some(options) => cmd_run(path, Some(exe), options),
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
    /// First cycle (counted like --cycles) at which --trace starts printing.
    trace_from: u64,
    expect: Option<String>,
}

impl RunOptions {
    fn parse(args: &[String]) -> Option<Self> {
        let mut options = Self { cycles: DEFAULT_CYCLES, trace: false, trace_from: 0, expect: None };
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--cycles" => options.cycles = args.next()?.replace('_', "").parse().ok()?,
                "--trace" => options.trace = true,
                "--trace-from" => {
                    options.trace = true;
                    options.trace_from = args.next()?.replace('_', "").parse().ok()?;
                }
                "--expect" => options.expect = Some(args.next()?.clone()),
                _ => return None,
            }
        }
        Some(options)
    }
}

/// Cycles the BIOS may take to reach its shell before `exe` gives up.
const BOOT_CYCLES: u64 = 200_000_000;

/// Reduces TTY text to the lines worth comparing with a reference log
/// captured on a console:
/// - line endings are unified and surrounding blanks and empty lines dropped;
/// - a leading "% " is removed (some reference logs were captured through a
///   tool that prefixes every line with it);
/// - "ResetGraph:" lines are dropped: they are debug messages of the SDK's
///   graphics library, vary with the BIOS and library build, and are missing
///   from most reference logs.
fn normalize(text: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(text)
        .lines()
        .map(|line| line.trim())
        .map(|line| line.strip_prefix("% ").unwrap_or(line).trim())
        .filter(|line| !line.is_empty() && !line.starts_with("ResetGraph:"))
        .map(str::to_string)
        .collect()
}

fn cmd_run(bios: &str, exe: Option<&String>, options: RunOptions) -> ExitCode {
    let data = match read_bios(bios) {
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
    let expected = match &options.expect {
        Some(path) => match std::fs::read(path) {
            Ok(log) => Some(normalize(&log)),
            Err(e) => {
                eprintln!("FAIL  could not read {path}: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };

    // The kernel's own boot messages are not part of a program's output.
    let mut start_cycle = 0;
    if let Some(path) = exe {
        let exe = match std::fs::read(path).map_err(|e| e.to_string()).and_then(|file| Exe::parse(&file).map_err(|e| e.to_string())) {
            Ok(exe) => exe,
            Err(e) => {
                eprintln!("FAIL  {path}: {e}");
                return ExitCode::FAILURE;
            }
        };
        if !psx.run_to_shell(BOOT_CYCLES) {
            eprintln!("FAIL  BIOS did not reach its shell within {BOOT_CYCLES} cycles (pc 0x{:08X})", psx.cpu.pc());
            return ExitCode::FAILURE;
        }
        psx.take_tty();
        psx.sideload(&exe);
        start_cycle = psx.cycles();
        eprintln!("---- kernel ready after {start_cycle} cycles; running {path} ----");
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut captured = Vec::new();

    while psx.cycles() - start_cycle < options.cycles {
        psx.step();
        if options.trace && psx.cycles() - start_cycle >= options.trace_from {
            let (pc, word) = (psx.cpu.current_pc(), psx.cpu.last_instr());
            eprintln!("{pc:08X}  {word:08X}  {}", disassemble(pc, word));
        }
        let tty = psx.take_tty();
        if !tty.is_empty() {
            // Ignore a closed pipe (e.g. `| head`).
            let _ = out.write_all(&tty).and_then(|()| out.flush());
            captured.extend_from_slice(&tty);
            // With a reference log there is nothing more to wait for once
            // the program has printed the reference's last line.
            if let Some(last) = expected.as_ref().and_then(|e| e.last()) {
                if normalize(&captured).last() == Some(last) {
                    break;
                }
            }
        }
    }

    eprintln!("\n---- stopped after {} cycles ----", psx.cycles() - start_cycle);
    eprintln!("pc 0x{:08X}   TTY bytes {}", psx.cpu.pc(), captured.len());
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
    if psx.bus.gp0_words() > 0 {
        eprintln!("GP0 words dropped by the GPU stub: {}", psx.bus.gp0_words());
    }
    let unhandled = psx.bus.unhandled();
    if !unhandled.is_empty() {
        eprintln!("unhandled bus accesses ({} distinct):", unhandled.len());
        for access in unhandled {
            eprintln!("  {access}");
        }
    }

    let Some(expected) = expected else { return ExitCode::SUCCESS };
    let actual = normalize(&captured);

    // Assertion-style programs print one "pass - name" or "fail - name ..."
    // line per test. Those are judged per test rather than as text, because
    // the released binaries are sometimes newer than the reference logs and
    // contain extra tests.
    let passes = |lines: &[String]| -> Vec<String> {
        lines.iter().filter_map(|l| l.strip_prefix("pass - ")).map(str::to_string).collect()
    };
    let expected_passes = passes(&expected);
    if !expected_passes.is_empty() {
        let actual_passes = passes(&actual);
        let failed: Vec<&String> = actual.iter().filter(|l| l.starts_with("fail - ")).collect();
        let missing: Vec<&String> = expected_passes.iter().filter(|t| !actual_passes.contains(t)).collect();
        let extra = actual_passes.iter().filter(|t| !expected_passes.contains(t)).count();
        for line in &failed {
            eprintln!("      {line}");
        }
        for test in &missing {
            eprintln!("      no pass reported for {test}");
        }
        let summary = format!(
            "{} of {} reference tests pass, {} fail, {} passing tests not in the reference log",
            expected_passes.len() - missing.len(),
            expected_passes.len(),
            failed.len(),
            extra
        );
        return if failed.is_empty() && missing.is_empty() {
            eprintln!("PASS  {summary}");
            ExitCode::SUCCESS
        } else {
            eprintln!("FAIL  {summary}");
            ExitCode::FAILURE
        };
    }

    if actual == expected {
        eprintln!("PASS  TTY output matches the reference log ({} lines)", expected.len());
        return ExitCode::SUCCESS;
    }
    let line = actual.iter().zip(&expected).take_while(|(a, e)| a == e).count();
    let show = |lines: &[String], end: &str| lines.get(line).cloned().unwrap_or_else(|| end.to_string());
    eprintln!("FAIL  TTY output differs from the reference log at compared line {}", line + 1);
    eprintln!("      expected: {:?}", show(&expected, "<end of log>"));
    eprintln!("      actual:   {:?}", show(&actual, "<no more output>"));
    ExitCode::FAILURE
}
