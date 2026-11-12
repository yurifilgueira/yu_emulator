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

## 2026-11-12 — Phase 6: CD-ROM

Model: claude-fable-5-2 · Session time: ~2h (estimate, not measured; the session was cut once by a usage limit) · Human interventions: 0

- Done (branch `phase-6-cdrom`, pull request):
  - `psx-core/src/disc.rs`: disc images. Cue sheets with several BINARY
    files, MODE1/2352, MODE2/2352 and AUDIO tracks, INDEX 01 and PREGAP;
    sector lookup, track table, region from the licence text of sector 4.
    The core still opens no files: the harness and the frontend read them
    and hand over bytes.
  - `psx-core/src/cdrom.rs`: the controller. Banked registers, parameter
    and result FIFOs, interrupts INT1-INT5 queued until the previous one is
    acknowledged, IRQ2; commands Nop, Setloc, Play, ReadN/ReadS, MotorOn,
    Stop, Pause, Init, Mute/Demute, Setfilter, Setmode, Getparam, GetlocL,
    GetlocP, SetSession, GetTN, GetTD, SeekL/SeekP, Test 20h, GetID,
    ReadTOC; response times from psx-spx "CDROM - Response Timings";
    sectors at 75 or 150 per second, 800h or 924h bytes.
  - Bus: registers at 1F801800h-1F801803h, DMA channel 3.
  - `--disc <cue|bin>` in the harness and the frontend.
  - 268 unit tests (252 core, 11 harness, 5 frontend).
- Results (Sony BIOS SCPH-1001):
  - No disc: main menu after about 800 million cycles; scripted pad
    presses open the memory card manager and the CD player.
  - Symphony of the Night (NTSC-U): PlayStation logo with the SCEA licence
    text, `SYSTEM.CNF` read, `SLUS_000.67` loaded and started. The game
    then prints "MDEC_out_sync timeout" repeatedly: its first screen is a
    video and there is no MDEC. START does not skip it.
- Broke / surprised us:
  - **A commit with tests that did not compile was pushed** (4ae2f64). I
    added the `disc` field to the frontend's `Options` and ran test,
    clippy, commit and push in one shell line joined with `;` instead of
    `&&`, so the failing test build did not stop the commit. The next
    commit (99c5a7c) fixes the test; history was not rewritten. Rule 3
    says this must not happen: checks and commit are now always chained
    with `&&`.
  - The disc module was committed (aedda25) before `pub mod disc;` was in
    `lib.rs`, so that commit builds but does not compile the file. The
    next commit adds it.
  - One of my controller tests read a single byte of a sector and expected
    the next request to deliver the following sector. The controller keeps
    the current buffer until software clears the request bit, which real
    programs do; the test helper now does the same. The test was wrong,
    not the code.
  - The data-request test for "missing device" DMA and the "unhandled I/O"
    test used the CD-ROM as their missing device; they now use MDEC-in and
    SIO1.
- Deviations from hardware, all commented in the code: seek time is a
  fixed 4 sectors (psx-spx: "seek timings are still unknown"); the delay
  between acknowledging one interrupt and seeing the next is a guess
  (2000 cycles); XA-ADPCM sectors are dropped and Play only moves the
  head, no sound; DMA3 always costs 24 cycles per word; the controller
  version is reported as vC1 (a) without knowing what an SCPH-1001 has
  (the BIOS prints "System Controller ROM Version 95/05/16 c1" from it);
  16- and 32-bit reads of the registers return one register.
- Not verified: Play/report/auto-pause against a real program (unit tests
  only); the memory card manager's copy and delete; multi-track games.
- Human stepped in for: nothing.
- Next: MDEC (Phase 7), because the only game available cannot get past
  its first video without it; then the SPU. A 3D game image from the
  owner is needed for the Phase 6 "3D game is playable" criterion.

## 2026-11-11 — Sony BIOS: timing fixes

Model: claude-fable-5-2 · Session time: ~1h (estimate, not measured) · Human interventions: 2

- Context: the owner added a Sony BIOS (`bios/SCPH1001.BIN`, SHA-1
  `10155d8d6e6e832d6ea66db9bc098321fb5e8ebf`) and ran the frontend on a
  second machine (Windows, Ryzen 5 5600), where 600 frames took 11.4 s:
  full speed, against roughly a third on the development notebook
  (Ryzen 5 5500U). On both, the BIOS drew a striped Sony logo and printed
  "VSync: timeout" every frame and "GPU timeout" now and then.
- Done (branch `sony-bios-timing`, pull request):
  - CPU reads and uncached instruction fetches now cost the access times a
    console measures (`Bus::read_delay`, from ps1-tests `cpu/access-time`).
  - GPU linked-list DMA advances 64 nodes per CPU instruction instead of 1.
  - The harness report shows I_STAT, I_MASK and the video position.
  - 243 unit tests (227 core, 11 harness, 5 frontend).
- Results:
  - Sony BIOS: no timeout messages in 1.2 billion cycles; the logo screen
    is clean. It then stays on the logo: I_MASK shows it has enabled the
    CD-ROM interrupt and is waiting for a drive that does not exist yet.
    So the BIOS menu is still not reached; that needs Phase 6.
  - No regressions: `harness suite` 16 of 32 as before; amidog
    `psxtest_cpu` gives the same VRAM hash; `psxtest_cpx` prints no
    mismatch.
- What was wrong, and how it was found:
  - "VSync: timeout". The vblank interrupt was arriving (the counter in
    the message went up by one each time), so it was not a missing
    interrupt. The BIOS waits in a loop that counts iterations, and with
    every instruction costing one cycle the loop ran out before the
    vblank. This is the "known gap" I had listed since Phase 1 ("every
    instruction is one cycle"); it took a real BIOS to turn it into a bug.
  - "GPU timeout" and the stripes were one bug. A linked list reached
    address 0 and looped forever. A ring buffer of the last nodes showed a
    primitive with a zero link: the BIOS reuses primitive memory for the
    next frame right after starting the transfer, and my Phase 2 design
    (one node per instruction, chosen so an endless list could not hang
    the emulator) let the CPU overtake the list. I had read psx-spx's "CPU
    operation resumes ... after SyncMode 2 list entries" as "the CPU keeps
    running" and missed the sentence before it: any RAM read stalls the
    CPU until the DMA is done.
  - A wrong lead: the first clue was 900 million GP0 words, and I spent
    three debug runs looking at ordinary lists before logging only lists
    that had gone 300,000 nodes without ending.
- Still approximate:
  - No i-cache: cached code never pays for a miss.
  - Access times are fixed values, not computed from the memory control
    registers; mul/div and GTE command durations are still one cycle.
  - 64 nodes per instruction is a guess that works. On hardware
    `dma/chain-looping` shows the CPU at about 60% speed during an endless
    list; here it gets one instruction per 64 nodes.
  - `timers`, `cpu/access-time`, `dma/chain-looping` still do not match
    their logs; I did not re-examine how close they are now.
- Human stepped in for: providing the Sony BIOS and testing on a second
  machine; telling me to stop ("agora não") when I started this fix before
  being asked, then to proceed.
- Also: the owner put a game archive in `roms-for-test/`, which was not
  gitignored. I added the folder and `*.7z`/`*.zip` to `.gitignore` in the
  Phase 5 branch before any `git add -A` could stage 368 MB.
- Next: Phase 6, CD-ROM. The BIOS now waits on it, and a game image is
  available to test with.

## 2026-11-11 — Phase 5: controllers, memory cards, and what amidog's CPX found

Model: claude-fable-5-2 · Session time: ~3h across two days (estimate, not measured) · Human interventions: 1

- Done (branch `phase-5-controllers`, pull request, not on `main`):
  - `psx-core/src/sio.rs`: SIO0 registers with byte timing from the baud
    rate, RX FIFO, /ACK pulse and IRQ7; a digital pad; a 128 KB memory card
    (read, write, get-ID, new-card flag, formatted image). Nothing is
    plugged in by default.
  - CPU: COP0 hardware breakpoints, and four corrections to exception
    behaviour (below).
  - Harness: a pad in port 1, `--press BUTTON@FRAME[+LENGTH]`, `--card`,
    `--trace-pc LO-HI`, and a register dump in the final report.
  - Frontend: keyboard as a pad, `--card`.
  - 240 unit tests (224 core, 11 harness, 5 frontend).
- Results (OpenBIOS):
  - amidog `psxtest_cpx`, started by a scripted START: all six groups OK
    on its result screen (read by eye from a screenshot) and no mismatch
    line on the TTY (0 lines containing "got"; the first run had 4567).
  - amidog `psxtest_cpu`: same VRAM hash as the all-green Phase 3 run
    (`91bcaf744e265cac`), so still all OK after the CPU changes.
  - ps1-tests `gte-fuzz`, started by a scripted START: PASS. Its output
    contains all 149,475 lines of the console log in order, plus two
    banner lines the log lacks. It needed 1.05 billion cycles; my first
    attempt, given 4 billion, I stopped by mistake believing the budget was
    too small (I had misjudged how long it had been running), so the test
    ran twice.
  - ps1-tests `input/pad` prints `PAD_UP` and `PAD_X` for those presses
    (this one uses the SDK's interrupt-driven driver; amidog polls).
  - `harness suite`: 16 of 32, unchanged.
- **What amidog's CPX found in the CPU.** Until now only ps1-tests' 24
  assertions and amidog's basic test had judged the interpreter. CPX is the
  first hard check, and it found things in code I had called done in
  Phase 1:
  1. COP0 breakpoints did nothing ("stored but never trigger", I wrote
     then). Most of the 4567 mismatches.
  2. Interrupt timing. Phase 1 took an interrupt before the instruction
     following the MTC0 that enabled or raised it. On hardware a change of
     SR is seen one instruction late; a change of CAUSE at once.
  3. CAUSE.CE is bits 26-27 of the faulting instruction for address errors
     and reserved instructions too; psx-spx only mentions coprocessor
     unusable.
  4. TAR is also written for an untaken branch.
  5. LWCn/SWCn of an enabled COP0/1/3 still raise address and bus errors.
     In Phase 2 I made them do nothing, on the strength of a test that
     only tried valid addresses.
- Deviations from psx-spx that the test forced (all commented in `cpu.rs`):
  - DCIC bits 29/30 ("break in kernel/user mode") select by the address
    being compared, not by the CPU mode.
  - Priorities: address error, then breakpoint, then bus error; a match
    sets the DCIC status bits even when the address error wins.
  - The address compared for LWL/SWL is the aligned one, for LWR/SWR the
    address itself (the lowest byte each touches).
- Broke / surprised us (my mistakes):
  - **Three Phase 1 unit tests were wrong and I changed them.** They
    asserted that an interrupt is taken at once; I had derived that from
    my own reading, not from a test on hardware. Rule 2 says never weaken
    a test to make it pass; these were replaced because hardware evidence
    contradicts them, and the new versions are stricter (they pin the
    exact EPC for both orders of the two MTC0s).
  - It took three rounds, not one. My first interrupt fix delayed
    everything by one instruction: the four failing cases passed and four
    others started failing. Only then did I trace the test's code
    (`--trace-pc`) and see the two orders of "mtc0 sr" / "mtc0 cause". My
    first CE fix applied it to every exception, the second to too few.
    Each round was guided by the suite's "got / wanted" lines, grouped by
    which fields differ with a throwaway script.
  - I pressed START too early. The program spends its first ~250M cycles
    clearing VRAM one pixel at a time; I sampled it at 60-80M, saw no
    controller traffic and briefly suspected the pad code. A register dump
    of the loop showed what it was doing.
  - I killed my own shell: `pkill -f target/release/harness` matches the
    command line of the shell running it. The edits queued after it in
    the same command never ran, and a debug `eprintln!` stayed in
    `cpu.rs` across a break in the session. It was not committed (I
    checked for it before every commit), but only by that check.
  - One debug run printed a line for every data access and did not finish
    in 10 minutes; I stopped it and restricted the print.
  - Two unit tests I wrote this phase had wrong expectations of my own
    (a load that I expected not to happen; a status field that hardware
    does set).
- Not done / cannot be verified here:
  - **The memory card has never been used by a program.** Its protocol is
    unit-tested from psx-spx only. OpenBIOS has no memory card functions
    ("Unimplemented memory card function" is in its strings) and none of
    the test programs touch one.
  - BIOS menu with a pad, memory card manager: Sony's shell; no Sony BIOS.
  - Analog pads, multitaps, other controllers, SIO1, the TX interrupt, the
    31000-cycle delay of Sony cards.
- Human stepped in for: merging pull request #2.
- Next: Phase 6, CD-ROM. A Sony BIOS would close the open criteria of
  Phases 3 and 5 and is close to a requirement for Phase 6's "PlayStation
  logo boot sequence".

## 2026-11-10 — Phase 4: GTE

Model: claude-fable-5-2 · Session time: ~45 min (estimate, not measured) · Human interventions: 2

- Done (branch `phase-4-gte`, pull request, not on `main`):
  - `psx-core/src/gte.rs`: the 64 registers with their read/write quirks
    (sign extension, the H read bug, SXYP push, IRGB/ORGB, LZCR, FLAG
    mask), the 22 commands, 44-bit MAC overflow and all saturation flags,
    the Newton-Raphson division with its table.
  - CPU: MFC2/CFC2 with load delay, MTC2/CTC2, LWC2/SWC2, BC2F/BC2T, GTE
    commands; a GTE command hit by an interrupt still executes (psx-spx
    "Interrupts vs GTE Commands"). The `gte_unimplemented` counter is gone.
  - Harness: a text log now passes if all its lines appear in the output
    in order; extra output lines are counted, not fatal.
  - 205 unit tests (194 core, 8 harness, 3 frontend).
- Results (OpenBIOS):
  - ps1-tests `gte/test-all`: 1150 passed, 0 failed.
  - `harness suite`: 16 of 32 programs pass (was 15).
  - The OpenBIOS shell's cube now shows three differently lit faces and
    is in a different orientation in screenshots taken 30M cycles apart.
    In Phase 3 its COP2 instructions were ignored and the picture showed
    one face and a sliver of a second.
- Not done:
  - `gte-fuzz` did not run: it shows a menu and waits for START (seen in a
    screenshot). Its 3 MB console log holds 1150 cases, the same number as
    `test-all`, and the one index I compared has the same parameters
    (RTPT case 20 = `test-all` test 970), so I believe `test-all` is that
    log compiled into a program — but I only checked one index. Moved to
    Phase 5's acceptance.
  - GTE command durations (5-44 cycles) and the 2-3 cycle delay of
    register writes are not modelled.
- Broke / surprised us:
  - `test-all` stopped at test 970 on the first run (969 passed). RTPS/
    RTPT derive SZ3 from the sum shifted right by 12, and that value has to
    be truncated to 32 bits before it is saturated: a sum that overflowed
    44 bits then flips sign and saturates to FFFFh instead of 0. I had kept
    it 64 bits wide. All 28 unit tests had passed, including my RTPS
    overflow test, because none of them drove MAC3 that far.
  - **I wasted almost five minutes of machine time**: I ran `gte-fuzz` for
    3,000,000,000 cycles to "let it finish" before looking at what it was
    doing. A 60M-cycle run with a screenshot showed the menu at once.
  - I left a nonsense placeholder assertion in a unit test
    (`rtps_depth_cue`) while writing the file and caught it on re-reading,
    before the first run.
  - The harness marked `test-all` FAIL although it printed "Failed tests:
    0", because the released binary prints a "Total tests" line that the
    2021 log lacks. I changed the judge (extra lines allowed). That makes
    passing easier, so to be explicit: a reference line that is missing or
    different still fails, and the summary reports the extra lines.
  - As in Phase 3, several details are not from psx-spx (where the 44-bit
    check happens inside a sum, which flags the bugged far-colour MVMVA
    sets, depth cue only for the last RTPT vertex). They follow what other
    emulators do, from memory, are marked in `gte.rs`, and are backed by
    `test-all` passing.
- How it was found and fixed: `test-all`'s own failure report listed the
  registers that differed (SZ3 0 vs FFFFh, the IR0 flag), which pointed at
  the depth value.
- Human stepped in for: merging pull request #1 (Phase 3), and logging the
  GitHub CLI in so that I can open pull requests myself.
- Next: Phase 5, controllers and memory cards. It unblocks three checks
  that are waiting on a START button: amidog `psxtest_cpx`, `gte-fuzz`,
  and (with a Sony BIOS) the BIOS menu.

## 2026-11-10 — Phase 3: GPU, window frontend, first pull request

Model: claude-fable-5-2 · Session time: ~2h (estimate, not measured) · Human interventions: 1

- Done (branch `phase-3-gpu`, not on `main`):
  - `psx-core/src/gpu/`: 1024x512 VRAM, every GP0 command (fill, the three
    VRAM transfers, polygons, rectangles, lines and polylines, attributes
    E1h-E6h, IRQ), every GP1 command, GPUSTAT, GPUREAD, the displayed
    picture in 15 and 24 bit. Software rasteriser with flat/Gouraud/
    textured (4/8/15-bit) drawing, modulation, dithering, texture windows,
    four semi-transparency modes, mask set/check, CLUT cache, interlaced
    drawing.
  - `video.rs` now belongs to the GPU and follows its registers: NTSC/PAL
    frame sizes, hblank from GP1(06h), vblank from GP1(07h).
  - `psx-core/src/spu.rs`: register file plus sound RAM transfers (manual
    and DMA4). No sound. Pulled forward from Phase 7 because the BIOS shell
    uploads samples before it draws.
  - Harness: `vram-hash`, `--vram-out`, `--frame-out`, `--diff-out`,
    `--region`; `suite` also judges programs that ship a `vram.png`. Uses
    the `png` crate.
  - `frontend/` crate (binary `yu-emulator`, `minifb` window): display or
    whole VRAM, Tab to switch.
  - 171 unit tests (161 core, 7 harness, 3 frontend).
  - CLAUDE.md "Git and GitHub" rewritten for the pull-request workflow the
    owner asked for, and its staging rule tightened after my mistake below.
- Results (OpenBIOS):
  - `harness suite bios/openbios.bin tests/ps1-tests`: 15 of 32 pass (was
    3 of 21 before image tests were counted). GPU programs: `clipping`,
    `clut-cache`, `quad`, `rectangles`, `texture-flip`, `texture-overflow`,
    `triangle`, `uv-interpolation`, `vram-to-vram-overlap` match all
    524,288 VRAM pixels; `gp0-e1` 10/10 and `mask-bit` 5/5 against logs.
  - `transparency` FAILS as a whole-VRAM comparison but matches all 76,800
    pixels of the 320x240 area it draws (`--region 0,0,320,240`); the rest
    of the reference is white, i.e. VRAM the program never writes.
  - `lines` FAILS by 64 pixels: two polyline segments whose end colour the
    test never sets (its source calls `setRGB2` twice and never `setRGB3`),
    so they show whatever was on the stack.
  - `bandwidth` fails: it measures GPU speed and drawing takes no time here.
  - amidog `psxtest_cpu`: all tests green in the Exception, Flag and Value
    columns, Timing N/A everywhere. **I read this off a screenshot by eye**;
    nothing checks it mechanically. VRAM hash of that run:
    `91bcaf744e265cac` (300M cycles), usable as a regression marker.
  - amidog `psxtest_cpx`: draws its menu and waits for START. Needs a
    controller; moved to Phase 5's acceptance.
  - BIOS shell: OpenBIOS's rotating cube renders at 640x480 interlaced.
    The roadmap's "memory card / CD player menu" is Sony's shell, which I
    cannot run.
- Broke / surprised us:
  - **A stray file was committed and pushed.** `lines_main.c` (ps1-tests
    source I had downloaded to read) ended up in the repository root: a
    `cd` into a scratch directory that did not exist yet failed, `curl -o`
    wrote into the current directory instead, and I then committed with
    `git add -A` without reading the staged list that I had printed in the
    same command. Removed by a follow-up commit (`1ae6cea`); the history
    was not rewritten, so the file is still visible in commit `ee9d7ef`.
  - The interlaced BIOS picture was wrong twice. First the cube was
    striped: fills cleared every line while polygons skipped the displayed
    field. After making fills skip too, one field was entirely black. A
    temporary log of each fill showed the shell draws twice per two
    frames, both times while my field flag had the same value: I switched
    fields at vsync, but the field being scanned out has to switch when
    vblank starts. psx-spx says only that interlace "DOES affect GP0 draw
    commands"; the rule in the code is inferred from this one program.
  - The CLUT cache had one case wrong: going from 8-bit to 4-bit textures
    with the same CLUT must keep the cached palette (I reloaded it).
    Found by `gpu/clut-cache`, fixed, and then a unit test I added for it
    had a wrong expectation of my own (a transparent texel leaves the
    old pixel, not zero).
  - `gpu/gp0-e1` showed GP0(E1h) bit 11 is only stored after GP1(09h)
    allows it. psx-spx hints at it; the exact behaviour comes from the log.
  - The BIOS shell never reached the GPU: it waited for a DMA transfer to
    the SPU, which Phase 2 left pending forever "as if DREQ never came".
  - Correction to the Phase 2 entry: amidog's programs do print on the TTY
    once they get past the GPU ("Running add test / Done", and a final
    "Result: 00000101" whose meaning I do not know). The per-test verdicts
    are still only on screen.
  - The Phase 2 unit tests of `video.rs` were replaced, not kept: they
    tested the placeholder blanking positions, which no longer exist now
    that blanking follows the GPU registers.
  - It is slow. The window ran 120 frames in 6.4 s (about 19 per second,
    a third of real time). Headless, 68M cycles of BIOS boot take 8.0 s
    here against 5.9 s for the Phase 2 code on the same machine, so most
    of the cost predates the GPU: every device is ticked after every
    instruction. No profiler is installed and I did not optimise (rule 8).
  - 38 of 38 new GPU unit tests and 9 of 14 GPU test programs passed on
    the first run. That is not evidence the unit tests are strong: they
    check rules stated by psx-spx, while the pixel-exact results depend on
    fixed-point stepping that psx-spx does not specify and that I
    reproduced from memory of how other software renderers do it
    (`raster.rs` says so). The reference images are the real check.
- How it was found and fixed: reference images and logs for everything in
  the GPU; looking at dumped PNGs for the interlace bugs; a throwaway
  debug print (removed) for the field timing.
- Human stepped in for: asking that work go through pull requests instead
  of straight to `main`. The GitHub CLI is not installed on this machine,
  so I could push the branch but not open the pull request myself.
- Known gaps and deviations (commented in the code):
  - No command FIFO, no drawing time: GPUSTAT busy bits only reflect a
    command waiting for parameters; DMA2 does not check GP1(04h).
  - Texture cache not emulated (CLUT cache is).
  - Texture page Y base 2 / 2 MB VRAM ignored apart from the status bit.
  - Hblank/vblank as seen by timers are derived from the display range
    registers; psx-spx does not define them. The timers sync-mode tables
    still differ from the console log.
  - The frontend applies no 4:3 correction, has no input and no speed
    regulation.
  - SPU: transfers only; the busy flag never reads busy.
- Next: Phase 4, the GTE (the OpenBIOS cube already executes 64 COP2
  instructions that are ignored, and ps1-tests has `gte/test-all`).

## 2026-11-09 — Phase 2: PS-EXE sideload, interrupts, timers, DMA

Model: claude-fable-5-2 · Session time: ~1.5h (estimate, not measured) · Human interventions: 0

- Done:
  - New core modules: `irq` (I_STAT/I_MASK), `timers` (clock sources, sync
    modes, one-shot/repeat/toggle IRQs), `dma` (registers, DPCR, DICR flag
    logic), `video` (hblank/vblank/dot clock generator standing in for the
    GPU), `exe` (PS-EXE parser). The bus routes the registers and runs the
    transfers: OTC, GPU block/slice, GPU linked list one node per tick.
  - `Psx::run_to_shell` + `Psx::sideload`: boot the real BIOS until it jumps
    to 0x80030000, then start the PS-EXE instead of the shell.
  - Harness: `exe <bios> <file.exe> [--expect psx.log]`, `suite <bios> <dir>`
    and `--trace-from CYCLE`.
  - Test programs downloaded by me into gitignored folders: the ps1-tests
    release (build-158) into `tests/ps1-tests/`, amidog's `psxtest_cpu` and
    `psxtest_cpx` into `tests/amidog/`. Not committed.
  - 109 unit tests (106 core, 3 harness).
- Results (`harness suite bios/openbios.bin tests/ps1-tests`): 3 of 21 pass.
  - PASS `cpu/cop` 17/17, `cpu/code-in-io` 7/7, `dma/otc-test` 13/13 (its 2
    newer cases, missing from the 2021 log, also print pass).
  - `timers`: for 1000/5000-cycle delays the "System clock" rows (1011,
    5011) and the "Hblank" rows equal the console log, apart from one
    6737 outlier in the log; the "/8" rows show the same 126/127 and
    626/627 values in a different order; frame delays are within 0.4%.
    Dot clock rows and the sync-mode tables differ (see below). Compared
    as text it is a FAIL.
  - `dma/chain-looping`: the finished/irq flags match the console for all
    four cases; the tick counts do not (7008 vs 16040).
  - Everything else needs GPU, GTE, CD-ROM, SPU or MDEC.
- Not done / still open:
  - **amidog's CPU tests do not report.** They print "args: 0", then poll
    GPUSTAT forever and would show their results on screen only. This is
    the criterion carried over from Phase 1; it now moves to Phase 3. So no
    independent CPU test suite beyond ps1-tests' 24 assertions has judged
    the interpreter yet.
  - `cpu/access-time` prints `1.%  .d` instead of numbers: its printf
    format is not understood by OpenBIOS's printf. Even with a Sony BIOS it
    would not match, since every instruction costs one cycle here.
- Broke / surprised us:
  - **psx-spx is wrong, or incomplete, about coprocessors.** My Phase 1 code
    followed it and failed several `cpu/cop` tests on the first run. On a
    real console COP1/COP3 instructions and LWCn/SWCn raise "coprocessor
    unusable" only while the CU bit is clear (for COP0 even in kernel mode)
    and are otherwise ignored, and an unknown COP0 command raises nothing.
    The Phase 1 unit tests had encoded the psx-spx reading, so they passed
    while the behaviour was wrong: tests written from the same document as
    the code cannot catch a mistake in the document.
  - I had also misjudged Phase 1's "next" note: the SPUSTAT wait is in the
    BIOS *shell*, which sideloading skips, so it did not block Phase 2.
  - Three of my own tests were wrong, not the code: an off-by-one in the
    expected dot count for a frame, and two that assumed 178 CPU cycles are
    280 video cycles (it is 279.7). Fixed by using exact multiples.
  - The first `--expect` compared whole logs and stopped once the output was
    as long as the reference, which cut programs off mid-line and failed on
    harmless differences ("% " prefixes, library `ResetGraph:` lines, tests
    added after the log was captured). Replaced by per-test judging for
    pass/fail logs and text comparison for the rest.
  - `code-in-io` executes code written into SPU registers. The I/O stub
    read zeros there, so the CPU ran off through I/O space; it needed
    registers that remember their value.
  - The timers log shows 198 dot-clock ticks per 1011 cycles (a /8 dot
    clock, 320 pixels); I get 159 (/10). The released binary writes
    GP1(08h)=0 (256 pixels) before measuring — I logged its GP1 writes —
    so the binary and the log seem to be from different versions. Not
    resolved; I did not change the emulator to match the log.
  - A 20M-line trace piped through sort took minutes and told me nothing;
    `--trace-from` exists because of that.
- How it was found and fixed: all of the above came from running the
  console-verified programs and reading their sources and logs, not from
  unit tests. Each hardware-derived deviation from psx-spx is commented at
  the code with the test that shows it.
- Human stepped in for: nothing this session.
- Known gaps and deviations (commented in the code):
  - Timing is still one cycle per instruction. DMA charges 1 cycle/word.
  - Timer details from psx-spx "Reset and Wrap" (1-2 cycle holds) and the
    INTF pulse are not modelled; DMA chopping and DICR per-block interrupts
    are ignored.
  - Hblank/vblank positions are derived from the default display range;
    only the dot clock follows GP1(08h). This is why the timer sync-mode
    tables differ. The GPU (Phase 3) must own this.
  - Stubs that return plausible values so software proceeds, all still
    listed as unhandled by the harness: GPUSTAT reads 0x14802000, SPU
    registers are plain memory with STATX mirroring ATTR bits 0-5,
    expansion 2/3 read all ones.
  - Narrow stores to the middle of a 32-bit I/O register: psx-spx is
    silent; only the addressed bytes are written.
  - DMA to devices that do not exist yet stays pending forever.
- Next: Phase 3, the GPU. First milestone inside it: enough GPUSTAT and GP0
  for amidog's two programs to draw their result screens, then read those.

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
