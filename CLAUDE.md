# PS1 emulator — instructions for Claude

This is a PlayStation (PS1) emulator written in Rust, built as a public
showcase of what an AI model can do on a long, hard engineering project.
Correctness and an honest record of the process matter more than speed.

## Layout

- `core/`    emulator core. **No file, window, audio or network access.**
             Everything comes in as bytes and goes out as data.
- `harness/` headless runner (`cargo run -p psx-harness -- <cmd>`).
- `docs/`    hardware references. `docs/psx-spx/` is the primary source.
- `bios/`    BIOS images. **Gitignored. Never commit a BIOS or game image.**
- `tests/`   test programs (PS-EXE) and reference outputs.

## Rules

1. **Cite the hardware docs.** Every hardware behaviour gets a comment naming
   the psx-spx section it implements (e.g. `// psx-spx: "CPU Load Delay Slots"`).
   If the docs are ambiguous or you deviate, say so in the comment.
2. **Tests first, then code.** A phase is done only when its acceptance
   criteria in `ROADMAP.md` pass. Never weaken or delete a test to make it pass.
3. **Before every commit:** `cargo test` and
   `cargo clippy --all-targets -- -D warnings` must be clean.
4. **One commit per meaningful step**, with a clear message. The git history
   is part of the showcase.
5. **Keep `DEVLOG.md` updated** at the end of every session: what was done,
   what broke, how it was found, how it was fixed, and anything the human had
   to step in for. Be honest about mistakes; that is the valuable part.
6. **No per-game hacks** without a `DEVLOG.md` entry explaining why.
7. **Accuracy over speed** until the roadmap says otherwise. Plain
   interpreter, no JIT, no premature optimisation.
8. If you are stuck for long, stop and write down in `DEVLOG.md` what you
   tried and what you suspect, then ask the human.

## Conventions

- PS1 is little-endian. Addresses are `u32`, written as `0x1F80_1810`.
- Bus loads/stores take **virtual** addresses; `map::mask_region` converts.
- Unimplemented hardware must not panic: record it via `Bus::unhandled`
  so the harness can report what to build next.
