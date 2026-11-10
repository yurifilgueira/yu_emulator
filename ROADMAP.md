# Roadmap

Each phase lists its **acceptance criteria**. A phase is done only when all of
them pass and are recorded in `DEVLOG.md`.

## Phase 0 — Bus and memory map ✅

- [x] Workspace: `core` (no I/O) + `harness`
- [x] Segment masking (KUSEG / KSEG0 / KSEG1 / KSEG2)
- [x] RAM 2 MB mirrored over 8 MB, BIOS 512 KB read-only, scratchpad
      (not reachable via KSEG1), expansion 1 open bus, cache control
- [x] I/O ports stubbed and unhandled accesses recorded
- [x] `harness bios <file>` loads a BIOS and reads the reset vector

## Phase 1 — CPU (R3000A)

- [ ] Interpreter for every R3000A instruction
- [ ] Branch delay slots and load delay slots
- [ ] COP0: SR, CAUSE, EPC, BadVaddr; exceptions (syscall, break, overflow,
      address error, bus error, reserved instruction); RFE
- [ ] Unmapped accesses raise bus error (replace the Phase 0 stub)
- [ ] `harness run <bios> --cycles N` with an instruction trace option
- [ ] Hook BIOS `putchar` (B-function 0x3D) and print TTY output
- **Accept:** the BIOS prints its boot text on the terminal; amidog's CPU
  tests pass once Phase 2 can load them.

## Phase 2 — Sideload, interrupts, timers, DMA

- [ ] Load PS-EXE files directly into RAM (skip the CD)
- [ ] Interrupt controller (I_STAT / I_MASK)
- [ ] Timers 0-2
- [ ] DMA: channel 6 (OTC) and channel 2 (GPU, linked list and block)
- [ ] `harness exe <bios> <file.exe>`
- **Accept:** CPU test programs from `tests/` run and report pass.

## Phase 3 — GPU

- [ ] GP0/GP1 commands, VRAM 1024x512, software rasteriser
- [ ] Polygons (flat, Gouraud, textured), rectangles, lines, transfers
- [ ] Semi-transparency, dithering, texture windows, masking
- [ ] `frontend` crate with a window showing VRAM / display area
- [ ] `harness vram-hash` compares output against reference images
- **Accept:** BIOS shell (memory card / CD player menu) renders; GPU test
  programs match their reference images.

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
