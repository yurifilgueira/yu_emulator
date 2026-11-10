# Yu-Emulator

A multi-system emulator built with Claude Fable 5.2: PlayStation (PS1) first,
Game Boy next. Written in Rust as a public experiment: how far can an AI model
take a long, hard, objectively testable engineering project?

Everything is recorded: the git history, `DEVLOG.md` (including the model's
mistakes) and the roadmap with acceptance criteria for each phase.

## Status — PlayStation

| Subsystem        | Status | Tests |
|------------------|--------|-------|
| Bus / memory map | ✅     | unit tests |
| CPU (R3000A)     | ✅ boots BIOS to TTY banner; ps1-tests `cpu/cop`, `cpu/code-in-io` pass; amidog needs GPU | unit tests + 2 test programs |
| Interrupts / DMA / timers | ✅ functional, not cycle-exact; ps1-tests `dma/otc-test` passes | unit tests + 1 test program |
| GPU              | ⏳     | —     |
| GTE              | ⏳     | —     |
| Controllers      | ⏳     | —     |
| CD-ROM           | ⏳     | —     |
| SPU / MDEC       | ⏳     | —     |

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
# run every test program that ships with a console log and print a table
cargo run --release -p psx-harness -- suite bios/your-bios.bin tests/ps1-tests
```

`cargo test` runs 109 unit tests and needs no BIOS. The test programs are not
in the repository: unpack the [ps1-tests](https://github.com/JaCzekanski/ps1-tests/releases)
release into `tests/ps1-tests/` and amidog's CPU tests into `tests/amidog/`
(both gitignored). Currently 3 of the 21 ps1-tests programs that have a
console log pass; most of the others exercise hardware from later phases.

## BIOS and games

This repository contains **no BIOS and no game images**. Use a dump of your
own console's BIOS or the open-source OpenBIOS from the PCSX-Redux project,
and only disc images you own. Put the BIOS in `bios/` (gitignored).

## References

- psx-spx (Martin Korth / Nocash), the hardware reference
- Community test programs (e.g. amidog's CPU tests, JaCzekanski's `ps1-tests`)
