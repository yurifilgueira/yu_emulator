# Yu-Emulator — instructions for Claude

Yu-Emulator is a multi-system emulator written in Rust: first the
PlayStation (PS1), later the Game Boy. It is a public showcase of what an AI
model can do on a long, hard engineering project. Correctness and an honest
record of the process matter more than speed.

## Layout

- `psx-core/` PS1 emulator core. **No file, window, audio or network access.**
              Everything comes in as bytes and goes out as data.
- `harness/`  headless runner (`cargo run -p psx-harness -- <cmd>`).
- `docs/`     hardware references. `docs/psx-spx/docs/*.md` is the primary PS1 source (start at index.md).
- `bios/`     BIOS images. **Gitignored. Never commit a BIOS or game image.**
- `tests/`    test programs (PS-EXE) and reference outputs.

Future systems get their own core crate (e.g. `gb-core/`) with the same
rules. Do not start the Game Boy until the PS1 roadmap says so. Code that is
truly shared between systems (frontend, audio output, input, save states)
goes in shared crates; hardware emulation is never shared between cores.

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
5. **Push after every commit** (see "Git and GitHub" below).
6. **Keep `DEVLOG.md` updated** at the end of every session: what was done,
   what broke, how it was found, how it was fixed, and anything the human had
   to step in for. Be honest about mistakes; that is the valuable part.
7. **No per-game hacks** without a `DEVLOG.md` entry explaining why.
8. **Accuracy over speed** until the roadmap says otherwise. Plain
   interpreter, no JIT, no premature optimisation.
9. If you are stuck for long, stop and write down in `DEVLOG.md` what you
   tried and what you suspect, then ask the human.

## Git and GitHub

- The project lives in the owner's GitHub repository **Yu-Emulator**,
  configured as the `origin` remote. Work on `main` unless told otherwise.
- After each commit that passes the checks in rule 3, run `git push origin main`.
  Never push a commit with failing tests or clippy warnings.
- At the end of a session, make sure `DEVLOG.md` is committed and pushed, so
  the repository always reflects where the project stands.
- If `origin` is missing or the push fails (authentication, rejected,
  diverged history), **stop and tell the human**. Never force-push, never
  rewrite published history, never change the remote URL on your own.
- Before every push, check that no BIOS, game image or other copyrighted
  binary is staged (`git status`, `git diff --cached --stat`). If one is,
  unstage it and tell the human.

## Conventions

- PS1 is little-endian. Addresses are `u32`, written as `0x1F80_1810`.
- Bus loads/stores take **virtual** addresses; `map::mask_region` converts.
- Unimplemented hardware must not panic: record it via `Bus::unhandled`
  so the harness can report what to build next.
