# Yu-Emulator roadmap

PlayStation first, then Game Boy.

Each phase lists its **acceptance criteria**. A phase is done only when all of
them pass and are recorded in `DEVLOG.md`.

## Phase 0 — Bus and memory map ✅

- [x] Workspace: `psx-core` (no I/O) + `harness`
- [x] Segment masking (KUSEG / KSEG0 / KSEG1 / KSEG2)
- [x] RAM 2 MB mirrored over 8 MB, BIOS 512 KB read-only, scratchpad
      (not reachable via KSEG1), expansion 1 open bus, cache control
- [x] I/O ports stubbed and unhandled accesses recorded
- [x] `harness bios <file>` loads a BIOS and reads the reset vector

## Phase 1 — CPU (R3000A) ✅ (amidog pending Phase 2)

- [x] Interpreter for every R3000A instruction
- [x] Branch delay slots and load delay slots
- [x] COP0: SR, CAUSE, EPC, BadVaddr; exceptions (syscall, break, overflow,
      address error, bus error, reserved instruction); RFE
- [x] Unmapped accesses raise bus error (replace the Phase 0 stub)
- [x] `harness run <bios> --cycles N` with an instruction trace option
- [x] Hook BIOS `putchar` (B-function 0x3D) and print TTY output
- **Accept:** the BIOS prints its boot text on the terminal ✅ (OpenBIOS,
  2026-11-09); amidog's CPU tests pass once Phase 2 can load them (open:
  Phase 2 loads them, but they report on screen only — carried to Phase 3).

## Phase 2 — Sideload, interrupts, timers, DMA ✅ (amidog pending Phase 3)

- [x] Load PS-EXE files directly into RAM (skip the CD)
- [x] Interrupt controller (I_STAT / I_MASK)
- [x] Timers 0-2
- [x] DMA: channel 6 (OTC) and channel 2 (GPU, linked list and block)
- [x] `harness exe <bios> <file.exe>` (plus `harness suite <bios> <dir>`)
- **Accept:** CPU test programs from `tests/` run and report pass.
  ps1-tests `cpu/cop` (17/17) and `cpu/code-in-io` (7/7) pass against
  their console logs, as does `dma/otc-test` (2026-11-09, OpenBIOS).
  Still open: amidog's `psxtest_cpu` / `psxtest_cpx` only show their
  results on screen and wait on GPUSTAT, so they need the Phase 3 GPU;
  ps1-tests `cpu/access-time` needs cycle-accurate memory timing (Phase 8).

## Phase 3 — GPU

- [ ] GP0/GP1 commands, VRAM 1024x512, software rasteriser
- [ ] Polygons (flat, Gouraud, textured), rectangles, lines, transfers
- [ ] Semi-transparency, dithering, texture windows, masking
- [ ] `frontend` crate with a window showing VRAM / display area
- [ ] `harness vram-hash` compares output against reference images
- **Accept:** BIOS shell (memory card / CD player menu) renders; GPU test
  programs match their reference images; amidog's `psxtest_cpu` and
  `psxtest_cpx` show all tests passing (carried over from Phases 1-2).

## Phase 4 — GTE

- [ ] All GTE commands, bit-exact fixed point, flags register
- **Accept:** all GTE tests in `tests/` pass.

## Phase 5 — Controllers and memory cards (SIO0)

- **Accept:** BIOS menu is navigable with a pad; memory card manager works.

## Phase 6 — CD-ROM

- [ ] `.bin/.cue` loading, command state machine, response timing
- **Accept:** PlayStation logo boot sequence; first commercial 3D game is
  playable.

## Phase 7 — SPU and MDEC

- **Accept:** BIOS boot sound; music and FMVs in the first game.

## Phase 8 — Compatibility and performance

- [ ] `COMPATIBILITY.md` table: boots / playable / perfect
- [ ] Full speed on a mid-range laptop

---

# Game Boy (after the PS1)

Starts once PS1 Phase 6 is done (a commercial 3D game is playable).

- [ ] `gb-core/` crate: SM83 CPU, memory map and MBCs, PPU, APU, timers, joypad
- [ ] Frontend supports choosing the system
- **Accept:** Blargg's `cpu_instrs` and `instr_timing` pass; Mooneye
  acceptance tests tracked in a table; a commercial game is playable.
