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
