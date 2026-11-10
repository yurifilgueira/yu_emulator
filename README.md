# Yu-Emulator

A multi-system emulator built with Claude Fable 5.2: PlayStation (PS1) first,
Game Boy next. Written in Rust as a public experiment: how far can an AI model
take a long, hard, objectively testable engineering project?

Everything is recorded: the git history, `DEVLOG.md` (including the model's
mistakes) and the roadmap with acceptance criteria for each phase.

## Status — PlayStation

| Subsystem        | Status | Tests |
|------------------|--------|-------|
| Bus / memory map | ✅     | 12/12 |
| CPU (R3000A)     | ⏳     | —     |
| Interrupts / DMA / timers | ⏳ | — |
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
```

## BIOS and games

This repository contains **no BIOS and no game images**. Use a dump of your
own console's BIOS or the open-source OpenBIOS from the PCSX-Redux project,
and only disc images you own. Put the BIOS in `bios/` (gitignored).

## References

- psx-spx (Martin Korth / Nocash), the hardware reference
- Community test programs (e.g. amidog's CPU tests, JaCzekanski's `ps1-tests`)
