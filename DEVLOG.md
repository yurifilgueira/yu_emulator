# Dev log

Honest record of the build: what worked, what broke, what the human had to
fix. Newest entries at the top.

Template:

```
## YYYY-MM-DD — Phase N: short title
Model: claude-fable-5-2 · Session time: Xh · Human interventions: N
- Done:
- Broke / surprised us:
- How it was found and fixed:
- Human stepped in for:
- Next:
```

---

## 2026-11-09 — Phase 2: PS-EXE sideload, interrupts, timers, DMA

Model: claude-fable-5-2 · Session time: ~1.5h (estimate, not measured) · Human interventions: 0

- Done:
  - New core modules: `irq` (I_STAT/I_MASK), `timers` (clock sources, sync
    modes, one-shot/repeat/toggle IRQs), `dma` (registers, DPCR, DICR flag
    logic), `video` (hblank/vblank/dot clock generator standing in for the
    GPU), `exe` (PS-EXE parser). The bus routes the registers and runs the
    transfers: OTC, GPU block/slice, GPU linked list one node per tick.
  - `Psx::run_to_shell` + `Psx::sideload`: boot the real BIOS until it jumps
    to 0x80030000, then start the PS-EXE instead of the shell.
  - Harness: `exe <bios> <file.exe> [--expect psx.log]`, `suite <bios> <dir>`
    and `--trace-from CYCLE`.
  - Test programs downloaded by me into gitignored folders: the ps1-tests
    release (build-158) into `tests/ps1-tests/`, amidog's `psxtest_cpu` and
    `psxtest_cpx` into `tests/amidog/`. Not committed.
  - 109 unit tests (106 core, 3 harness).
- Results (`harness suite bios/openbios.bin tests/ps1-tests`): 3 of 21 pass.
  - PASS `cpu/cop` 17/17, `cpu/code-in-io` 7/7, `dma/otc-test` 13/13 (its 2
    newer cases, missing from the 2021 log, also print pass).
  - `timers`: for 1000/5000-cycle delays the "System clock" rows (1011,
    5011) and the "Hblank" rows equal the console log, apart from one
    6737 outlier in the log; the "/8" rows show the same 126/127 and
    626/627 values in a different order; frame delays are within 0.4%.
    Dot clock rows and the sync-mode tables differ (see below). Compared
    as text it is a FAIL.
  - `dma/chain-looping`: the finished/irq flags match the console for all
    four cases; the tick counts do not (7008 vs 16040).
  - Everything else needs GPU, GTE, CD-ROM, SPU or MDEC.
- Not done / still open:
  - **amidog's CPU tests do not report.** They print "args: 0", then poll
    GPUSTAT forever and would show their results on screen only. This is
    the criterion carried over from Phase 1; it now moves to Phase 3. So no
    independent CPU test suite beyond ps1-tests' 24 assertions has judged
    the interpreter yet.
  - `cpu/access-time` prints `1.%  .d` instead of numbers: its printf
    format is not understood by OpenBIOS's printf. Even with a Sony BIOS it
    would not match, since every instruction costs one cycle here.
- Broke / surprised us:
  - **psx-spx is wrong, or incomplete, about coprocessors.** My Phase 1 code
    followed it and failed several `cpu/cop` tests on the first run. On a
    real console COP1/COP3 instructions and LWCn/SWCn raise "coprocessor
    unusable" only while the CU bit is clear (for COP0 even in kernel mode)
    and are otherwise ignored, and an unknown COP0 command raises nothing.
    The Phase 1 unit tests had encoded the psx-spx reading, so they passed
    while the behaviour was wrong: tests written from the same document as
    the code cannot catch a mistake in the document.
  - I had also misjudged Phase 1's "next" note: the SPUSTAT wait is in the
    BIOS *shell*, which sideloading skips, so it did not block Phase 2.
  - Three of my own tests were wrong, not the code: an off-by-one in the
    expected dot count for a frame, and two that assumed 178 CPU cycles are
    280 video cycles (it is 279.7). Fixed by using exact multiples.
  - The first `--expect` compared whole logs and stopped once the output was
    as long as the reference, which cut programs off mid-line and failed on
    harmless differences ("% " prefixes, library `ResetGraph:` lines, tests
    added after the log was captured). Replaced by per-test judging for
    pass/fail logs and text comparison for the rest.
  - `code-in-io` executes code written into SPU registers. The I/O stub
    read zeros there, so the CPU ran off through I/O space; it needed
    registers that remember their value.
  - The timers log shows 198 dot-clock ticks per 1011 cycles (a /8 dot
    clock, 320 pixels); I get 159 (/10). The released binary writes
    GP1(08h)=0 (256 pixels) before measuring — I logged its GP1 writes —
    so the binary and the log seem to be from different versions. Not
    resolved; I did not change the emulator to match the log.
  - A 20M-line trace piped through sort took minutes and told me nothing;
    `--trace-from` exists because of that.
- How it was found and fixed: all of the above came from running the
  console-verified programs and reading their sources and logs, not from
  unit tests. Each hardware-derived deviation from psx-spx is commented at
  the code with the test that shows it.
- Human stepped in for: nothing this session.
- Known gaps and deviations (commented in the code):
  - Timing is still one cycle per instruction. DMA charges 1 cycle/word.
  - Timer details from psx-spx "Reset and Wrap" (1-2 cycle holds) and the
    INTF pulse are not modelled; DMA chopping and DICR per-block interrupts
    are ignored.
  - Hblank/vblank positions are derived from the default display range;
    only the dot clock follows GP1(08h). This is why the timer sync-mode
    tables differ. The GPU (Phase 3) must own this.
  - Stubs that return plausible values so software proceeds, all still
    listed as unhandled by the harness: GPUSTAT reads 0x14802000, SPU
    registers are plain memory with STATX mirroring ATTR bits 0-5,
    expansion 2/3 read all ones.
  - Narrow stores to the middle of a 32-bit I/O register: psx-spx is
    silent; only the addressed bytes are written.
  - DMA to devices that do not exist yet stays pending forever.
- Next: Phase 3, the GPU. First milestone inside it: enough GPUSTAT and GP0
  for amidog's two programs to draw their result screens, then read those.

## 2026-11-09 — Phase 1: R3000A CPU, BIOS boots to its TTY banner

Model: claude-fable-5-2 · Session time: ~1h (estimate, not measured) · Human interventions: 3

- Done:
  - Bus loads/stores return `Result<_, BusFault>`; unused regions now fault
    instead of reading zero (13 bus tests).
  - `psx-core/src/cpu.rs`: interpreter for every opcode in psx-spx "CPU
    Opcode Encoding", branch delay slots, load delay slots (including
    LWL/LWR seeing the in-flight load and a second load cancelling the
    first), mul/div error results, COP0 (SR, CAUSE, EPC, BadVaddr, TAR,
    PRID, RFE, debug registers stored only), all exception kinds with BD/BT,
    user-mode address errors, interrupt entry (IP/Im/IEc, software
    interrupts), cache isolation. 47 CPU tests, written with the code.
  - `psx-core/src/system.rs`: `Psx` (CPU + bus) with the putchar hook on
    A(3Ch)/B(3Dh); the core only collects bytes, the harness prints them.
  - `psx-core/src/disasm.rs` and `harness run <bios> [--cycles N] [--trace]`.
  - **Acceptance:** `harness run bios/openbios.bin --cycles 20000000` prints

        PS-X Realtime Kernel OpenBios - build id unknown.
        Copyright (C) 2019-2026 PCSX-Redux authors.
        KERNEL SETUP!

        Configuration : EvCB	0x10		TCB	0x04

    Only tested with OpenBIOS; no Sony BIOS was available. amidog's CPU
    tests are still open: they need the PS-EXE loader of Phase 2.
- Broke / surprised us:
  - The machine had no Rust toolchain, `bios/` and `docs/psx-spx/` were
    empty and `origin` was not configured, so nothing could be built,
    cited, accepted or pushed at the start.
  - Two test failures on the first CPU run. One was a mistake in my own
    test: the syscall test set SR to a pattern with KUc=1, so the CPU was
    (correctly) in user mode and raised AdEL on the KSEG0 fetch instead of
    reaching the syscall. The other was a real bug: the scratchpad was
    executable, while psx-spx "Scratchpad" says a fetch there bus-errors.
  - The disassembler printed negative immediates as `0xffe8`; Rust's `{:#x}`
    on an `i16` prints two's complement.
  - The BIOS booted to its banner on the first full run. That is worth
    distrusting: it proves the instructions OpenBIOS uses early in boot, not
    the rest. amidog's tests are the real check.
- How it was found and fixed:
  - Both CPU failures were caught by the unit tests before any BIOS code
    ran. The syscall test was corrected to use a kernel-mode SR pattern (the
    assertion on the KU/IE stack is as strict as before); the scratchpad
    fetch check was added in the CPU, since the bus must keep mapping it for
    data. The disassembler bug was caught by its own test.
- Human stepped in for:
  1. Approving the Rust install (rustup, user-local, `--no-modify-path`, so
     `~/.cargo/bin` is not on PATH in new shells unless added by hand).
  2. Providing `bios/openbios.bin` and cloning psx-spx into `docs/psx-spx/`.
  3. Configuring the `origin` remote.
- Known gaps and deviations (all commented in the code):
  - Timing: every instruction is one cycle. psx-spx's mul/div and memory
    access times are not modelled, so `--cycles` really counts instructions.
  - No i-cache: with SR.IsC set, stores are dropped and loads read zero.
  - Gaps inside the I/O region do not bus-error yet (whole window is a stub).
  - COP2 with CU2 set is a counted no-op until Phase 4; "Interrupts vs GTE
    Commands" is noted in `fetch_and_execute` for then.
  - COP0 breakpoints (BPC/BDA/DCIC) are stored but never trigger.
  - SR reset value, BcondZ with undocumented rt values, and the
    load-cancels-load rule are not specified by psx-spx; the comments say
    what was assumed.
- Next: Phase 2. After the banner OpenBIOS spins at 0x800392D0 polling
  SPUSTAT (0x1F801DAE) bit 5, which the I/O stub reads as 0. That is an SPU
  register (Phase 7), so Phase 2 will need at least a minimal answer there
  to get further, on top of the PS-EXE loader, I_STAT/I_MASK, timers and
  DMA. First thing to run once the loader exists: amidog's CPU tests.

## 2026-11-10 — Project renamed to Yu-Emulator

Human interventions: 0
- Done: project renamed to Yu-Emulator (PS1 first, Game Boy later). Core
  crate moved to `psx-core/` to leave room for `gb-core/`. CLAUDE.md now
  requires pushing to the owner's GitHub repo after every green commit.

## 2026-11-10 — Phase 0: bus and memory map

Model: scaffold generated before the Fable sessions start · Human interventions: 0

- Done: Cargo workspace (`core`, `harness`), memory map with segment masking,
  RAM mirroring, read-only BIOS, scratchpad (blocked via KSEG1), expansion 1
  open bus, cache control, I/O stub that records unhandled accesses.
  12 unit tests. `harness bios <file>` reads the reset vector.
- Next: Phase 1, the CPU.
