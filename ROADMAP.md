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

## Phase 3 — GPU ✅ (Sony BIOS menu and amidog `psxtest_cpx` not verifiable yet)

- [x] GP0/GP1 commands, VRAM 1024x512, software rasteriser
- [x] Polygons (flat, Gouraud, textured), rectangles, lines, transfers
- [x] Semi-transparency, dithering, texture windows, masking
- [x] `frontend` crate with a window showing VRAM / display area
- [x] `harness vram-hash` compares output against reference images
- **Accept:** BIOS shell (memory card / CD player menu) renders; GPU test
  programs match their reference images; amidog's `psxtest_cpu` and
  `psxtest_cpx` show all tests passing (carried over from Phases 1-2).
  Status on 2026-11-10, OpenBIOS:
  - BIOS shell: OpenBIOS's shell (a rotating cube, 640x480 interlaced)
    renders. The memory card / CD player menu is Sony's shell and has not
    been run: no Sony BIOS is available here.
  - ps1-tests GPU programs with a `vram.png`: 9 of 11 match every pixel of
    VRAM (`clipping`, `clut-cache`, `quad`, `rectangles`, `texture-flip`,
    `texture-overflow`, `triangle`, `uv-interpolation`,
    `vram-to-vram-overlap`). `transparency` matches in the 320x240 area it
    draws (the reference has white elsewhere); `lines` differs in 64 pixels
    whose colour the test program leaves uninitialised. `gp0-e1` and
    `mask-bit` pass against their logs; `bandwidth` needs GPU timing.
  - amidog `psxtest_cpu`: every test shows OK for exception, flag and
    value (timing column N/A). `psxtest_cpx` reaches its menu and waits for
    the START button — it needs a controller (Phase 5).

## Phase 4 — GTE ✅

- [x] All GTE commands, bit-exact fixed point, flags register
- **Accept:** all GTE tests in `tests/` pass.
  2026-11-10, OpenBIOS: ps1-tests `gte/test-all` reports 1150 passed, 0
  failed. The other GTE program, `gte-fuzz`, stops at a menu waiting for
  the START button, so it has not run; the 1150 cases of `test-all` are
  the ones recorded in `gte-fuzz`'s console log (same count, and the same
  parameters at the index checked). Running `gte-fuzz` itself is listed
  under Phase 5.

## Phase 5 — Controllers and memory cards (SIO0) ✅ (Sony BIOS menu and card manager not verifiable yet)

- [x] SIO0 port: registers, byte timing, /ACK and IRQ7
- [x] Digital pad; keyboard mapping in the frontend, scripted input in the harness
- [x] Memory card: read, write and get-ID commands; card image files
- **Accept:** BIOS menu is navigable with a pad; memory card manager works;
  amidog's `psxtest_cpx` runs from its menu and shows all tests passing
  (carried over from Phases 1-3); ps1-tests `gte-fuzz`, started from its
  menu, reproduces its console log (carried over from Phase 4).
  Status on 2026-11-11, OpenBIOS:
  - amidog `psxtest_cpx`: started with a scripted START press, every group
    (exception, interrupt, slot, slot branch, break, illegal) is OK and it
    prints no mismatch. Its first run printed 4567; fixing them is part of
    this phase.
  - ps1-tests `gte-fuzz`: started with a scripted START press, its output
    contains all 149,475 lines of the console log, in order.
  - ps1-tests `input/pad` reports the scripted buttons.
  - BIOS menu and memory card manager: these are Sony's shell. Not run: no
    Sony BIOS is available, and OpenBIOS has neither a menu nor memory card
    functions. The memory card is only covered by unit tests written from
    psx-spx; no program has read or written one.

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
