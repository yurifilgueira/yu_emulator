//! Headless harness.
//!
//! - `bios`: loads a BIOS, checks it, and reads the reset vector.
//! - `run`: boots a BIOS for N cycles, printing the kernel's TTY output.
//! - `exe`: boots the BIOS until its kernel is ready, then runs a PS-EXE
//!   instead of the shell, optionally comparing its TTY output with a
//!   reference log.
//! - `vram-hash`: runs a PS-EXE, prints a hash of VRAM and optionally
//!   compares VRAM with a reference image.
//! - `suite`: runs every PS-EXE under a directory that has a reference log
//!   or reference image next to it and prints a pass/fail table.

mod image;

use image::Rgb;
use psx_core::disasm::disassemble;
use psx_core::exe::Exe;
use psx_core::{map, Bus, Psx};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const DEFAULT_CYCLES: u64 = 100_000_000;
/// Cycles the BIOS may take to reach its shell before `exe` gives up.
const BOOT_CYCLES: u64 = 200_000_000;
/// Name of the console-captured log that ps1-tests ships next to each test.
const REFERENCE_LOG: &str = "psx.log";
/// Name of the VRAM dump that ps1-tests ships next to its GPU tests.
const REFERENCE_VRAM: &str = "vram.png";

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  \
         harness bios <bios.bin>\n      \
         check a BIOS image and read the reset vector\n  \
         harness run <bios.bin> [--cycles N] [--trace] [--trace-from CYCLE] [pictures]\n      \
         boot the BIOS for N cycles (default {DEFAULT_CYCLES}); TTY output goes to stdout,\n      \
         the instruction trace (--trace) and the final report go to stderr\n  \
         harness exe <bios.bin> <file.exe> [--cycles N] [--trace] [--trace-from CYCLE] [--expect <log>] [pictures]\n      \
         boot the BIOS up to its shell, then run the PS-EXE for N cycles;\n      \
         with --expect, stop at the reference log's last line and exit 0 only if the\n      \
         output agrees with it: if the log has \"pass - \"/\"fail - \" lines, every test that\n      \
         passes in the log must pass and nothing may fail; otherwise the text must match\n      \
         (blank lines, \"% \" prefixes and ResetGraph: lines are ignored)\n  \
         harness vram-hash <bios.bin> <file.exe> [--cycles N] [--expect <vram.png>] [--region X,Y,W,H]\n      \
         [--diff-out <png>] [pictures]\n      \
         run the PS-EXE for N cycles and print a hash of VRAM; with --expect, exit 0 only\n      \
         if every VRAM pixel equals the 1024x512 reference image at 5 bits per channel\n      \
         (the mask bit is not in the image and is not compared); --region limits the\n      \
         comparison to a rectangle, --diff-out marks the differing pixels\n  \
         harness suite <bios.bin> <dir> [--cycles N]\n      \
         run every PS-EXE under <dir> that has a {REFERENCE_LOG} or {REFERENCE_VRAM} next to it, judge\n      \
         each like `exe --expect` / `vram-hash --expect`, and print a table; exit 0 only\n      \
         if all of them pass\n\n\
         pictures: [--vram-out <png>] writes the whole VRAM, [--frame-out <png>] the displayed picture"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, path] if cmd == "bios" => cmd_bios(path),
        [cmd, path, options @ ..] if cmd == "run" => match RunOptions::parse(options) {
            Some(options) if options.expect.is_none() && options.diff_out.is_none() && options.region.is_none() => {
                cmd_run(path, None, options)
            }
            _ => usage(),
        },
        [cmd, path, exe, options @ ..] if cmd == "exe" => match RunOptions::parse(options) {
            Some(options) if options.diff_out.is_none() && options.region.is_none() => cmd_run(path, Some(exe), options),
            _ => usage(),
        },
        [cmd, path, exe, options @ ..] if cmd == "vram-hash" => match RunOptions::parse(options) {
            Some(options) if !options.trace => cmd_vram_hash(path, exe, options),
            _ => usage(),
        },
        [cmd, path, dir, options @ ..] if cmd == "suite" => match RunOptions::parse(options) {
            Some(options) if options.is_plain() => cmd_suite(path, dir, options),
            _ => usage(),
        },
        _ => usage(),
    }
}

fn cmd_bios(path: &str) -> ExitCode {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => return fail(format!("could not read {path}: {e}")),
    };
    let mut bus = match Bus::new(data) {
        Ok(b) => b,
        Err(e) => return fail(e),
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

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("FAIL  {message}");
    ExitCode::FAILURE
}

struct RunOptions {
    cycles: u64,
    trace: bool,
    /// First cycle (counted like --cycles) at which --trace starts printing.
    trace_from: u64,
    expect: Option<String>,
    vram_out: Option<String>,
    frame_out: Option<String>,
    diff_out: Option<String>,
    /// --region X,Y,W,H: the part of VRAM that vram-hash --expect compares.
    region: Option<image::Region>,
}

impl RunOptions {
    fn parse(args: &[String]) -> Option<Self> {
        let mut options = Self {
            cycles: DEFAULT_CYCLES,
            trace: false,
            trace_from: 0,
            expect: None,
            vram_out: None,
            frame_out: None,
            diff_out: None,
            region: None,
        };
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
                "--vram-out" => options.vram_out = Some(args.next()?.clone()),
                "--frame-out" => options.frame_out = Some(args.next()?.clone()),
                "--diff-out" => options.diff_out = Some(args.next()?.clone()),
                "--region" => {
                    let numbers: Vec<usize> = args.next()?.split(',').map(|n| n.parse().ok()).collect::<Option<_>>()?;
                    let [x, y, w, h] = numbers[..] else { return None };
                    options.region = Some((x, y, w, h));
                }
                _ => return None,
            }
        }
        Some(options)
    }

    /// Only --cycles was given.
    fn is_plain(&self) -> bool {
        let paths = [&self.expect, &self.vram_out, &self.frame_out, &self.diff_out];
        !self.trace && self.region.is_none() && paths.iter().all(|o| o.is_none())
    }

    /// Writes the pictures requested with --vram-out / --frame-out.
    fn write_pictures(&self, psx: &Psx) -> Result<(), String> {
        if let Some(path) = &self.vram_out {
            Rgb::from_vram(psx.bus.gpu.vram()).write_png(Path::new(path))?;
        }
        if let Some(path) = &self.frame_out {
            Rgb::from_frame(psx.bus.gpu.frame()).write_png(Path::new(path))?;
        }
        Ok(())
    }
}

// ---- reference logs -------------------------------------------------------

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

struct Verdict {
    pass: bool,
    summary: String,
    details: Vec<String>,
}

/// Compares normalized program output with a normalized reference log.
fn judge(expected: &[String], actual: &[String]) -> Verdict {
    // Assertion-style programs print one "pass - name" or "fail - name ..."
    // line per test. Those are judged per test rather than as text, because
    // the released binaries are sometimes newer than the reference logs and
    // contain extra tests.
    let passes = |lines: &[String]| -> Vec<String> {
        lines.iter().filter_map(|l| l.strip_prefix("pass - ")).map(str::to_string).collect()
    };
    let expected_passes = passes(expected);
    if !expected_passes.is_empty() {
        let actual_passes = passes(actual);
        let failed: Vec<String> = actual.iter().filter(|l| l.starts_with("fail - ")).cloned().collect();
        let missing: Vec<&String> = expected_passes.iter().filter(|t| !actual_passes.contains(t)).collect();
        let extra = actual_passes.iter().filter(|t| !expected_passes.contains(t)).count();
        let mut summary = format!(
            "{} of {} reference tests pass, {} fail",
            expected_passes.len() - missing.len(),
            expected_passes.len(),
            failed.len()
        );
        if extra > 0 {
            summary += &format!(", {extra} more pass that the log does not have");
        }
        let pass = failed.is_empty() && missing.is_empty();
        let mut details = failed;
        details.extend(missing.iter().map(|test| format!("no pass reported for {test}")));
        return Verdict { pass, summary, details };
    }

    if actual == expected {
        let summary = format!("output matches the reference log ({} lines)", expected.len());
        return Verdict { pass: true, summary, details: Vec::new() };
    }
    let line = actual.iter().zip(expected).take_while(|(a, e)| a == e).count();
    let show = |lines: &[String], end: &str| lines.get(line).cloned().unwrap_or_else(|| end.to_string());
    Verdict {
        pass: false,
        summary: format!("output differs from the reference log at compared line {} of {}", line + 1, expected.len()),
        details: vec![
            format!("expected: {:?}", show(expected, "<end of log>")),
            format!("actual:   {:?}", show(actual, "<no more output>")),
        ],
    }
}

// ---- running ----------------------------------------------------------------

/// Boots `bios` and, if given, replaces the shell with the PS-EXE at `exe`.
/// Returns the console and the cycle count at which the program started.
fn boot(bios: &[u8], exe: Option<&Path>) -> Result<(Psx, u64), String> {
    let mut psx = Psx::new(bios.to_vec()).map_err(|e| e.to_string())?;
    let Some(path) = exe else { return Ok((psx, 0)) };

    let file = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let exe = Exe::parse(&file).map_err(|e| format!("{}: {e}", path.display()))?;
    if !psx.run_to_shell(BOOT_CYCLES) {
        return Err(format!(
            "BIOS did not reach its shell within {BOOT_CYCLES} cycles (pc 0x{:08X})",
            psx.cpu.pc()
        ));
    }
    // The kernel's own boot messages are not part of the program's output.
    psx.take_tty();
    psx.sideload(&exe);
    let start = psx.cycles();
    Ok((psx, start))
}

/// Runs for `options.cycles` cycles, or until the program has printed the
/// last line of the reference log. Returns the TTY output.
fn run(psx: &mut Psx, start: u64, options: &RunOptions, expected: Option<&[String]>, live: bool) -> Vec<u8> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut captured = Vec::new();

    while psx.cycles() - start < options.cycles {
        psx.step();
        if options.trace && psx.cycles() - start >= options.trace_from {
            let (pc, word) = (psx.cpu.current_pc(), psx.cpu.last_instr());
            eprintln!("{pc:08X}  {word:08X}  {}", disassemble(pc, word));
        }
        let tty = psx.take_tty();
        if tty.is_empty() {
            continue;
        }
        if live {
            // Ignore a closed pipe (e.g. `| head`).
            let _ = out.write_all(&tty).and_then(|()| out.flush());
        }
        captured.extend_from_slice(&tty);
        // With a reference log there is nothing more to wait for once the
        // program has printed the reference's last line.
        if let Some(last) = expected.and_then(|e| e.last()) {
            if tty.contains(&b'\n') && normalize(&captured).last() == Some(last) {
                break;
            }
        }
    }
    captured
}

fn read_reference(path: &Path) -> Result<Vec<String>, String> {
    std::fs::read(path).map(|log| normalize(&log)).map_err(|e| format!("could not read {}: {e}", path.display()))
}

fn cmd_run(bios: &str, exe: Option<&String>, options: RunOptions) -> ExitCode {
    let bios_data = match std::fs::read(bios) {
        Ok(d) => d,
        Err(e) => return fail(format!("could not read {bios}: {e}")),
    };
    let expected = match options.expect.as_deref().map(|path| read_reference(Path::new(path))) {
        Some(Ok(lines)) => Some(lines),
        Some(Err(e)) => return fail(e),
        None => None,
    };
    let (mut psx, start) = match boot(&bios_data, exe.map(Path::new)) {
        Ok(booted) => booted,
        Err(e) => return fail(e),
    };
    if let Some(path) = exe {
        eprintln!("---- kernel ready after {start} cycles; running {path} ----");
    }

    let captured = run(&mut psx, start, &options, expected.as_deref(), true);

    eprintln!("\n---- stopped after {} cycles ----", psx.cycles() - start);
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
    let (width, height) = psx.bus.gpu.display_size();
    eprintln!(
        "GPU: {} GP0 words, display {width}x{height}, VRAM hash {:016x}",
        psx.bus.gp0_words(),
        image::vram_hash(psx.bus.gpu.vram())
    );
    let unhandled = psx.bus.unhandled();
    if !unhandled.is_empty() {
        eprintln!("unhandled bus accesses ({} distinct):", unhandled.len());
        for access in unhandled {
            eprintln!("  {access}");
        }
    }

    if let Err(e) = options.write_pictures(&psx) {
        return fail(e);
    }

    let Some(expected) = expected else { return ExitCode::SUCCESS };
    report(judge(&expected, &normalize(&captured)))
}

fn report(verdict: Verdict) -> ExitCode {
    for detail in &verdict.details {
        eprintln!("      {detail}");
    }
    eprintln!("{}  {}", if verdict.pass { "PASS" } else { "FAIL" }, verdict.summary);
    if verdict.pass {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

// ---- VRAM comparison ----------------------------------------------------------

/// Compares VRAM with a reference picture of it.
fn judge_vram(reference: &Rgb, psx: &Psx, region: Option<image::Region>) -> Result<Verdict, String> {
    let c = image::compare(reference, &Rgb::from_vram(psx.bus.gpu.vram()), region)?;
    let mut details = Vec::new();
    if let Some((x, y, expected, actual)) = c.first {
        details.push(format!("first difference at ({x},{y}): expected rgb5 {expected:?}, got {actual:?}"));
    }
    let summary = if c.different == 0 {
        format!("VRAM matches the reference image ({} pixels)", c.total)
    } else {
        format!(
            "{} of {} VRAM pixels differ ({:.3}%), largest channel difference {} of 31",
            c.different,
            c.total,
            100.0 * c.different as f64 / c.total as f64,
            c.max_delta
        )
    };
    Ok(Verdict { pass: c.different == 0, summary, details })
}

fn cmd_vram_hash(bios: &str, exe: &str, options: RunOptions) -> ExitCode {
    let bios_data = match std::fs::read(bios) {
        Ok(d) => d,
        Err(e) => return fail(format!("could not read {bios}: {e}")),
    };
    let reference = match options.expect.as_deref().map(|path| Rgb::read_png(Path::new(path))) {
        Some(Ok(image)) => Some(image),
        Some(Err(e)) => return fail(e),
        None => None,
    };
    let (mut psx, start) = match boot(&bios_data, Some(Path::new(exe))) {
        Ok(booted) => booted,
        Err(e) => return fail(e),
    };
    run(&mut psx, start, &options, None, false);
    println!("{:016x}  {exe}", image::vram_hash(psx.bus.gpu.vram()));
    if let Err(e) = options.write_pictures(&psx) {
        return fail(e);
    }

    let Some(reference) = reference else { return ExitCode::SUCCESS };
    if let Some(path) = &options.diff_out {
        let actual = Rgb::from_vram(psx.bus.gpu.vram());
        if (reference.width, reference.height) == (actual.width, actual.height) {
            if let Err(e) = image::difference(&reference, &actual).write_png(Path::new(path)) {
                return fail(e);
            }
        }
    }
    match judge_vram(&reference, &psx, options.region) {
        Ok(verdict) => report(verdict),
        Err(e) => fail(e),
    }
}

// ---- suite --------------------------------------------------------------------

/// Finds `<dir>/**/<name>.exe` files that have a reference log or VRAM image
/// beside them. A directory with several executables is skipped unless one
/// is named after the directory, because the reference could belong to any
/// of them.
fn find_tests(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();

    let is_exe = |p: &PathBuf| p.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
    let exes: Vec<&PathBuf> = paths.iter().filter(|p| p.is_file() && is_exe(p)).collect();
    if dir.join(REFERENCE_LOG).is_file() || dir.join(REFERENCE_VRAM).is_file() {
        let named = exes.iter().find(|p| p.file_stem() == dir.file_name());
        match (named, exes.as_slice()) {
            (Some(exe), _) => found.push((*exe).clone()),
            (None, [exe]) => found.push((*exe).clone()),
            _ => {}
        }
    }
    for path in paths.iter().filter(|p| p.is_dir()) {
        find_tests(path, found);
    }
}

fn cmd_suite(bios: &str, dir: &str, options: RunOptions) -> ExitCode {
    let bios_data = match std::fs::read(bios) {
        Ok(d) => d,
        Err(e) => return fail(format!("could not read {bios}: {e}")),
    };
    let root = Path::new(dir);
    let mut tests = Vec::new();
    find_tests(root, &mut tests);
    if tests.is_empty() {
        return fail(format!("no PS-EXE with a {REFERENCE_LOG} or {REFERENCE_VRAM} next to it under {dir}"));
    }

    let mut passed = 0;
    for exe in &tests {
        let name = exe.parent().and_then(|p| p.strip_prefix(root).ok()).unwrap_or(exe).display().to_string();
        let log = exe.with_file_name(REFERENCE_LOG);
        let verdict = if log.is_file() {
            read_reference(&log).and_then(|expected| {
                let (mut psx, start) = boot(&bios_data, Some(exe))?;
                let captured = run(&mut psx, start, &options, Some(&expected), false);
                Ok(judge(&expected, &normalize(&captured)))
            })
        } else {
            Rgb::read_png(&exe.with_file_name(REFERENCE_VRAM)).and_then(|reference| {
                let (mut psx, start) = boot(&bios_data, Some(exe))?;
                run(&mut psx, start, &options, None, false);
                judge_vram(&reference, &psx, None)
            })
        };
        match verdict {
            Ok(verdict) => {
                passed += verdict.pass as usize;
                println!("{}  {name:<28} {}", if verdict.pass { "PASS" } else { "FAIL" }, verdict.summary);
            }
            Err(e) => println!("FAIL  {name:<28} {e}"),
        }
    }
    println!("\n{passed} of {} test programs pass", tests.len());
    if passed == tests.len() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        normalize(text.as_bytes())
    }

    #[test]
    fn normalize_drops_noise() {
        let text = "% cpu/cop\r\n\r\nResetGraph:itb=80013574\n  pass - a  \n%\nDone.\n";
        assert_eq!(lines(text), ["cpu/cop", "pass - a", "%", "Done."]);
    }

    #[test]
    fn assertion_logs_are_judged_per_test() {
        let expected = lines("title\npass - a\npass - b\nDone.");

        let v = judge(&expected, &lines("other title\npass - a\npass - extra\npass - b\n"));
        assert!(v.pass, "extra passing tests and other text are fine");
        assert!(v.summary.contains("2 of 2"));
        assert!(v.summary.contains("1 more"));

        let v = judge(&expected, &lines("pass - a\nfail - b:12 `x == y`"));
        assert!(!v.pass);
        assert_eq!(v.details, ["fail - b:12 `x == y`", "no pass reported for b"]);

        let v = judge(&expected, &lines("pass - a\npass - b\nfail - new"));
        assert!(!v.pass, "a failing test is a failure even if the log lacks it");

        let v = judge(&expected, &lines("pass - a"));
        assert!(!v.pass, "a missing test is a failure");
    }

    #[test]
    fn other_logs_are_compared_as_text() {
        let expected = lines("Timer0: 1011\nTimer1: 5011");
        assert!(judge(&expected, &lines("\nTimer0: 1011\r\nTimer1: 5011\n")).pass);
        let v = judge(&expected, &lines("Timer0: 1011\nTimer1: 5012"));
        assert!(!v.pass);
        assert!(v.summary.contains("line 2 of 2"));
        assert!(!judge(&expected, &lines("Timer0: 1011")).pass);
    }
}
