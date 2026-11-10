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
