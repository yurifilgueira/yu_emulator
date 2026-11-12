# Yu-Emulator

A multi-system emulator built with Claude Fable 5.2: PlayStation (PS1) first,
Game Boy next. Written in Rust as a public experiment: how far can an AI model
take a long, hard, objectively testable engineering project?

Everything is recorded: the git history, `DEVLOG.md` (including the model's
mistakes) and the roadmap with acceptance criteria for each phase.

## Status — PlayStation

| Subsystem        | Status | Checked by |
|------------------|--------|------------|
| Bus / memory map | ✅     | unit tests |
| CPU (R3000A)     | ✅ boots the BIOS; amidog `psxtest_cpu` and `psxtest_cpx` all OK | unit tests, ps1-tests `cpu/cop`, `cpu/code-in-io`, amidog |
| Interrupts / DMA / timers | ✅ functional, not cycle-exact; Sony BIOS (SCPH1001) boots to its logo | unit tests, ps1-tests `dma/otc-test`, `dma/dpcr` |
| GPU              | ✅ software rasteriser; 9 of 11 ps1-tests VRAM references match exactly | unit tests, ps1-tests `gpu/*` |
| GTE              | ✅ all commands, flags, inexact division | unit tests, ps1-tests `gte/test-all` (1150/1150) |
| Controllers / memory cards | ✅ digital pad; memory card untested by any program | unit tests, ps1-tests `input/pad`, programs driven through their menus |
| CD-ROM           | ⏳     | —     |
| SPU / MDEC       | ⏳ (SPU: only sound RAM transfers) | — |

Game Boy support is planned after the PS1 (see `ROADMAP.md`).

## Running

```sh
cargo test
cargo run -p psx-harness -- bios bios/your-bios.bin

# boot the BIOS headless; kernel TTY output is printed on stdout
cargo run --release -p psx-harness -- run bios/your-bios.bin --cycles 20000000
# same, with an instruction trace on stderr
cargo run --release -p psx-harness -- run bios/your-bios.bin --cycles 1000 --trace

# run a PS-EXE (the BIOS boots first, then the program replaces the shell)
cargo run --release -p psx-harness -- exe bios/your-bios.bin tests/ps1-tests/cpu/cop/cop.exe \
    --expect tests/ps1-tests/cpu/cop/psx.log
# save the displayed picture and the whole VRAM as PNG
cargo run --release -p psx-harness -- exe bios/your-bios.bin tests/amidog/psxtest_cpu.exe \
    --cycles 300000000 --frame-out screen.png --vram-out vram.png
# compare VRAM with a reference image
cargo run --release -p psx-harness -- vram-hash bios/your-bios.bin tests/ps1-tests/gpu/quad/quad.exe \
    --expect tests/ps1-tests/gpu/quad/vram.png
# run every test program that ships with a console log or VRAM image
cargo run --release -p psx-harness -- suite bios/your-bios.bin tests/ps1-tests

# open a window (Tab: display / whole VRAM, Esc: quit)
cargo run --release -p frontend -- bios/your-bios.bin [program.exe]
```

`cargo test` runs 243 unit tests and needs no BIOS. The test programs are not
in the repository: unpack the [ps1-tests](https://github.com/JaCzekanski/ps1-tests/releases)
release into `tests/ps1-tests/` and amidog's CPU tests into `tests/amidog/`
(both gitignored). Currently 16 of the 32 ps1-tests programs that have a
console reference pass; most of the others exercise hardware from later
phases.

In the window the keyboard is a digital pad (arrows, Enter = START,
Backspace = SELECT, Z/X/A/S = cross/circle/square/triangle, Q/W/1/2 =
shoulders) and `--card file.mcd` plugs in a memory card. It runs at about a
third of real time; speed is a later phase. Headless, `--press start@200`
holds a button from a given video frame, which is how test programs with
menus are driven.

## BIOS and games

This repository contains **no BIOS and no game images**. Use a dump of your
own console's BIOS or the open-source OpenBIOS from the PCSX-Redux project,
and only disc images you own. Put the BIOS in `bios/` (gitignored).

## References

- psx-spx (Martin Korth / Nocash), the hardware reference
- Community test programs (e.g. amidog's CPU tests, JaCzekanski's `ps1-tests`)
