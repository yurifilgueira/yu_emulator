//! MIPS R3000A interpreter (the PS1 CPU, without the GTE).
//!
//! psx-spx: "CPU Specifications"
//!
//! One call to [`Cpu::step`] executes exactly one instruction. Timing is not
//! modelled yet: every instruction counts as one cycle (psx-spx documents
//! longer mul/div and memory access times; that is deferred until the timers
//! of Phase 2 need it).
//!
//! Two pipeline effects are modelled because software depends on them:
//!
//! * **Branch delay** — psx-spx: "CPU Jump Opcodes": "the instruction
//!   following the branch will always be executed". Implemented with a
//!   `pc` / `next_pc` pair: a branch only changes `next_pc`.
//! * **Load delay** — psx-spx: "CPU Load/Store Opcodes" / "Caution - Load
//!   Delay": a loaded value is not visible to the next instruction.
//!   Implemented with an in-flight `(register, value)` pair that is committed
//!   after the following instruction has read its operands.

use crate::bus::Bus;
use crate::map;

/// psx-spx: "COP0 - Exception Handling" — ExcCode values of CAUSE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exception {
    /// INT: external (or software) interrupt.
    Interrupt = 0x00,
    /// AdEL: address error on data load or instruction fetch.
    AddressLoad = 0x04,
    /// AdES: address error on data store.
    AddressStore = 0x05,
    /// IBE: bus error on instruction fetch.
    BusFetch = 0x06,
    /// DBE: bus error on data load/store.
    BusData = 0x07,
    /// Sys: SYSCALL instruction.
    Syscall = 0x08,
    /// Bp: BREAK instruction.
    Break = 0x09,
    /// RI: reserved instruction.
    Reserved = 0x0A,
    /// CpU: coprocessor unusable.
    CoprocessorUnusable = 0x0B,
    /// Ovf: arithmetic overflow.
    Overflow = 0x0C,
}

// psx-spx: "cop0r12 - SR - System status register".
const SR_IEC: u32 = 1 << 0;
const SR_KUC: u32 = 1 << 1;
const SR_ISC: u32 = 1 << 16;
const SR_BEV: u32 = 1 << 22;
const SR_CU0: u32 = 1 << 28;
const SR_CU2: u32 = 1 << 30;
/// Bits 6-7, 23-24 and 26-27 are "Not used (zero)".
const SR_WRITE_MASK: u32 = 0xF27F_FF3F;

// psx-spx: "cop0r13 - CAUSE".
const CAUSE_SW: u32 = 0x0000_0300;
const CAUSE_IP: u32 = 0x0000_FF00;
const CAUSE_IP2: u32 = 1 << 10;
const CAUSE_BT: u32 = 1 << 30;
const CAUSE_BD: u32 = 1 << 31;

/// psx-spx: "cop0r15 - PRID": 00000002h on a CXD8606CQ.
const PRID: u32 = 0x0000_0002;

/// psx-spx: "Exception Vectors (depending on BEV bit in SR register)".
const VECTOR_GENERAL_RAM: u32 = 0x8000_0080;
const VECTOR_GENERAL_ROM: u32 = 0xBFC0_0180;

/// System control coprocessor state.
/// psx-spx: "COP0 - Register Summary".
#[derive(Debug, Clone, Default)]
struct Cop0 {
    bpc: u32,      // r3
    bda: u32,      // r5
    tar: u32,      // r6
    dcic: u32,     // r7
    badvaddr: u32, // r8
    bdam: u32,     // r9
    bpcm: u32,     // r11
    sr: u32,       // r12
    cause: u32,    // r13
    epc: u32,      // r14
}

/// A fetched instruction word with field accessors.
/// psx-spx: "CPU Opcode Encoding" / "Opcode/Parameter Encoding".
#[derive(Clone, Copy)]
struct Instr(u32);

impl Instr {
    fn op(self) -> u32 {
        self.0 >> 26
    }
    fn rs(self) -> usize {
        ((self.0 >> 21) & 0x1F) as usize
    }
    fn rt(self) -> usize {
        ((self.0 >> 16) & 0x1F) as usize
    }
    fn rd(self) -> usize {
        ((self.0 >> 11) & 0x1F) as usize
    }
    fn shamt(self) -> u32 {
        (self.0 >> 6) & 0x1F
    }
    fn funct(self) -> u32 {
        self.0 & 0x3F
    }
    /// Zero-extended 16-bit immediate.
    fn imm(self) -> u32 {
        self.0 & 0xFFFF
    }
    /// Sign-extended 16-bit immediate.
    fn simm(self) -> u32 {
        self.0 as u16 as i16 as u32
    }
    fn target(self) -> u32 {
        self.0 & 0x03FF_FFFF
    }
}

pub struct Cpu {
    regs: [u32; 32],
    hi: u32,
    lo: u32,
    /// Address of the next instruction to fetch.
    pc: u32,
    /// Address of the instruction after that (changed by branches).
    next_pc: u32,
    /// Address of the instruction being executed (for EPC).
    current_pc: u32,
    cop0: Cop0,

    /// Load started by the instruction being executed.
    load: Option<(usize, u32)>,
    /// Load started by the previous instruction, not yet visible in `regs`.
    delayed: Option<(usize, u32)>,
    /// Register write of the instruction being executed. Every instruction
    /// writes at most one general register.
    writeback: Option<(usize, u32)>,

    /// The instruction being executed is a branch/jump (taken or not).
    branch: bool,
    branch_taken: bool,
    branch_target: u32,
    /// The instruction being executed sits in a branch delay slot.
    delay_slot: bool,
    delay_taken: bool,
    /// Coprocessor number for the CE field of a CpU exception.
    cop_number: u32,

    last_instr: u32,
    cycles: u64,
    gte_unimplemented: u64,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    /// Creates a CPU in its reset state.
    pub fn new() -> Self {
        let mut cpu = Self {
            regs: [0; 32],
            hi: 0,
            lo: 0,
            pc: 0,
            next_pc: 0,
            current_pc: 0,
            cop0: Cop0::default(),
            load: None,
            delayed: None,
            writeback: None,
            branch: false,
            branch_taken: false,
            branch_target: 0,
            delay_slot: false,
            delay_taken: false,
            cop_number: 0,
            last_instr: 0,
            cycles: 0,
            gte_unimplemented: 0,
        };
        cpu.reset();
        cpu
    }

    /// psx-spx: "Exception Vectors" — reset jumps to BFC00000h.
    ///
    /// psx-spx does not list the reset value of SR. Standard R3000 behaviour
    /// is kernel mode, interrupts off and BEV=1 (boot vectors in ROM), which
    /// is what is used here; the BIOS rewrites SR early on anyway.
    pub fn reset(&mut self) {
        self.set_pc(map::RESET_VECTOR);
        self.cop0.sr = SR_BEV;
        self.cop0.cause = 0;
        self.load = None;
        self.delayed = None;
        self.writeback = None;
    }

    // ---- inspection (harness, tests, system hooks) -------------------------

    /// Address of the next instruction that will execute.
    pub fn pc(&self) -> u32 {
        self.pc
    }

    /// Redirects execution, discarding any pending branch.
    pub fn set_pc(&mut self, pc: u32) {
        self.pc = pc;
        self.next_pc = pc.wrapping_add(4);
        self.branch = false;
        self.branch_taken = false;
    }

    /// Address of the most recently executed instruction.
    pub fn current_pc(&self) -> u32 {
        self.current_pc
    }

    /// Word of the most recently fetched instruction.
    pub fn last_instr(&self) -> u32 {
        self.last_instr
    }

    pub fn reg(&self, index: usize) -> u32 {
        self.regs[index]
    }

    pub fn set_reg(&mut self, index: usize, value: u32) {
        if index != 0 {
            self.regs[index] = value;
        }
    }

    pub fn hi(&self) -> u32 {
        self.hi
    }

    pub fn lo(&self) -> u32 {
        self.lo
    }

    pub fn sr(&self) -> u32 {
        self.cop0.sr
    }

    pub fn cause(&self) -> u32 {
        self.cop0.cause
    }

    pub fn epc(&self) -> u32 {
        self.cop0.epc
    }

    pub fn badvaddr(&self) -> u32 {
        self.cop0.badvaddr
    }

    /// Instructions executed so far (one cycle each for now).
    pub fn cycles(&self) -> u64 {
        self.cycles
    }

    /// Number of COP2 (GTE) instructions executed as no-ops. The GTE is
    /// Phase 4; this lets the harness report that software needed it.
    pub fn gte_unimplemented(&self) -> u64 {
        self.gte_unimplemented
    }

    /// Drives the CPU's external interrupt line.
    /// psx-spx: "cop0r13 - CAUSE" bits 10-15 (IP). On the PS1 the interrupt
    /// controller (I_STAT & I_MASK, Phase 2) is wired to bit 10.
    pub fn set_irq(&mut self, asserted: bool) {
        if asserted {
            self.cop0.cause |= CAUSE_IP2;
        } else {
            self.cop0.cause &= !CAUSE_IP2;
        }
    }

    // ---- execution ---------------------------------------------------------

    /// Executes one instruction.
    pub fn step(&mut self, bus: &mut Bus) {
        self.current_pc = self.pc;
        self.delay_slot = self.branch;
        self.delay_taken = self.branch_taken;
        self.branch = false;
        self.branch_taken = false;
        self.delayed = self.load.take();
        self.writeback = None;
        self.cycles += 1;

        let pc = self.pc;
        self.pc = self.next_pc;
        self.next_pc = self.next_pc.wrapping_add(4);

        if let Err(e) = self.fetch_and_execute(pc, bus) {
            self.exception(e);
        }

        // psx-spx: "Caution - Load Delay". Commit order matters:
        // 1. the load of the previous instruction lands now, unless this
        //    instruction started another load of the same register (the
        //    older value is then never seen; not spelled out in psx-spx,
        //    verified on hardware by amidog's CPU tests);
        // 2. this instruction's own write wins over the landing load.
        if let Some((reg, value)) = self.delayed.take() {
            if self.load.map(|(r, _)| r) != Some(reg) {
                self.regs[reg] = value;
            }
        }
        if let Some((reg, value)) = self.writeback.take() {
            self.regs[reg] = value;
        }
        self.regs[0] = 0;
    }

    fn fetch_and_execute(&mut self, pc: u32, bus: &mut Bus) -> Result<(), Exception> {
        // psx-spx: "CPU Jump Opcodes" — a jump to an unaligned address raises
        // AdEL on the next fetch; "cop0r14 - EPC": EPC and BadVaddr point to
        // the faulty address, not to the jump.
        if pc & 3 != 0 || self.kseg_violation(pc) {
            self.cop0.badvaddr = pc;
            return Err(Exception::AddressLoad);
        }
        // psx-spx: "Memory Map" / "Scratchpad": "the scratchpad is NOT
        // executable. Attempts to jump to this region will cause a bus error
        // on the first instruction fetch." The bus itself maps it (for data).
        if map::SCRATCHPAD.contains(map::mask_region(pc)).is_some() {
            return Err(Exception::BusFetch);
        }
        // Unused regions bus-error on fetch as well.
        let word = bus.load32(pc).map_err(|_| Exception::BusFetch)?;
        self.last_instr = word;

        // psx-spx: "cop0r13 - CAUSE" / "cop0r12 - SR": an interrupt is taken
        // when IEc is set and any pending bit (IP/Sw) is enabled in Im.
        // Phase 4 note: psx-spx "Interrupts vs GTE Commands" (the GTE command
        // at EPC still executes) must be handled here once the GTE exists.
        if self.cop0.sr & SR_IEC != 0 && self.cop0.sr & self.cop0.cause & CAUSE_IP != 0 {
            return Err(Exception::Interrupt);
        }

        self.execute(Instr(word), bus)
    }

    /// psx-spx: "COP0 - Exception Handling".
    fn exception(&mut self, e: Exception) {
        let sr = self.cop0.sr;
        // Push the KU/IE stack: current -> previous -> old, then enter
        // kernel mode with interrupts disabled (inverse of what RFE pops).
        self.cop0.sr = (sr & !0x3F) | ((sr << 2) & 0x3F);

        // ExcCode in bits 2-6; pending interrupt bits are preserved.
        let mut cause = (self.cop0.cause & CAUSE_IP) | ((e as u32) << 2);
        if e == Exception::CoprocessorUnusable {
            cause |= self.cop_number << 28; // CE
        }
        // psx-spx: "cop0r14 - EPC": with BD set, EPC points at the branch.
        if self.delay_slot {
            cause |= CAUSE_BD;
            self.cop0.epc = self.current_pc.wrapping_sub(4);
            // psx-spx: "cop0r6 - TAR": updated when the branch is taken.
            if self.delay_taken {
                cause |= CAUSE_BT;
                self.cop0.tar = self.branch_target;
            }
        } else {
            self.cop0.epc = self.current_pc;
        }
        self.cop0.cause = cause;

        let vector = if sr & SR_BEV != 0 { VECTOR_GENERAL_ROM } else { VECTOR_GENERAL_RAM };
        self.set_pc(vector);
        // The faulting instruction has no effect on registers.
        self.writeback = None;
        self.load = None;
    }

    // ---- helpers -----------------------------------------------------------

    /// Queues the (single) register write of the current instruction.
    fn write(&mut self, reg: usize, value: u32) {
        self.writeback = Some((reg, value));
    }

    /// Starts a delayed load into `reg`.
    fn load_delayed(&mut self, reg: usize, value: u32) {
        self.load = Some((reg, value));
    }

    /// psx-spx: "CPU Jump Opcodes": target is relative to the delay slot.
    fn branch(&mut self, taken: bool, offset: u32) {
        self.branch = true;
        if taken {
            self.jump(self.pc.wrapping_add(offset << 2));
        }
    }

    fn jump(&mut self, target: u32) {
        self.branch = true;
        self.branch_taken = true;
        self.branch_target = target;
        self.next_pc = target;
    }

    /// psx-spx: "COP0 - Exception Handling" (AdEL/AdES): "address errors
    /// occur when attempting to read outside of KUseg in user mode".
    fn kseg_violation(&self, addr: u32) -> bool {
        self.cop0.sr & SR_KUC != 0 && addr >= 0x8000_0000
    }

    fn check_address(&mut self, addr: u32, align: u32, e: Exception) -> Result<(), Exception> {
        // psx-spx: "Load/Store Alignment".
        if addr & (align - 1) != 0 || self.kseg_violation(addr) {
            // psx-spx: "cop0r8 - BadVaddr": updated only by address errors.
            self.cop0.badvaddr = addr;
            return Err(e);
        }
        Ok(())
    }

    /// psx-spx: "cop0r12 - SR" bit 16 (IsC): while the cache is isolated,
    /// loads and stores target the cache instead of memory. The i-cache is
    /// not emulated, so isolated stores are dropped and isolated loads read
    /// zero. This is what the kernel's FlushCache relies on: it must not
    /// overwrite RAM.
    fn cache_isolated(&self) -> bool {
        self.cop0.sr & SR_ISC != 0
    }

    fn load8(&mut self, bus: &mut Bus, addr: u32) -> Result<u8, Exception> {
        self.check_address(addr, 1, Exception::AddressLoad)?;
        if self.cache_isolated() {
            return Ok(0);
        }
        bus.load8(addr).map_err(|_| Exception::BusData)
    }

    fn load16(&mut self, bus: &mut Bus, addr: u32) -> Result<u16, Exception> {
        self.check_address(addr, 2, Exception::AddressLoad)?;
        if self.cache_isolated() {
            return Ok(0);
        }
        bus.load16(addr).map_err(|_| Exception::BusData)
    }

    fn load32(&mut self, bus: &mut Bus, addr: u32) -> Result<u32, Exception> {
        self.check_address(addr, 4, Exception::AddressLoad)?;
        if self.cache_isolated() {
            return Ok(0);
        }
        bus.load32(addr).map_err(|_| Exception::BusData)
    }

    fn store8(&mut self, bus: &mut Bus, addr: u32, value: u8) -> Result<(), Exception> {
        self.check_address(addr, 1, Exception::AddressStore)?;
        if self.cache_isolated() {
            return Ok(());
        }
        bus.store8(addr, value).map_err(|_| Exception::BusData)
    }

    fn store16(&mut self, bus: &mut Bus, addr: u32, value: u16) -> Result<(), Exception> {
        self.check_address(addr, 2, Exception::AddressStore)?;
        if self.cache_isolated() {
            return Ok(());
        }
        bus.store16(addr, value).map_err(|_| Exception::BusData)
    }

    fn store32(&mut self, bus: &mut Bus, addr: u32, value: u32) -> Result<(), Exception> {
        self.check_address(addr, 4, Exception::AddressStore)?;
        if self.cache_isolated() {
            return Ok(());
        }
        bus.store32(addr, value).map_err(|_| Exception::BusData)
    }

    /// Value of `reg` as seen by LWL/LWR: unlike every other instruction they
    /// see a load that is still in flight. psx-spx: "Unaligned Load/Store" —
    /// "There's no delay required between lwl and lwr".
    fn reg_with_pending_load(&self, reg: usize) -> u32 {
        match self.delayed {
            Some((r, value)) if r == reg => value,
            _ => self.regs[reg],
        }
    }

    fn coprocessor_unusable(&mut self, number: u32) -> Exception {
        self.cop_number = number;
        Exception::CoprocessorUnusable
    }

    // ---- decoder -----------------------------------------------------------

    /// psx-spx: "CPU Opcode Encoding" — primary opcode field.
    fn execute(&mut self, i: Instr, bus: &mut Bus) -> Result<(), Exception> {
        let rs = self.regs[i.rs()];
        let rt = self.regs[i.rt()];
        // Effective address of loads/stores: rs + sign-extended immediate.
        let addr = rs.wrapping_add(i.simm());

        match i.op() {
            0x00 => return self.execute_special(i),
            0x01 => {
                // BcondZ. psx-spx: "CPU Jump Opcodes" lists bltz/bgez/
                // bltzal/bgezal (rt = 00h/01h/10h/11h). The other rt values
                // are not in the psx-spx table; on hardware only bit 16
                // selects the condition and bits 17-20 = 1000b select
                // linking, which is what is implemented here.
                let negative = (rs as i32) < 0;
                let taken = negative != (i.0 & (1 << 16) != 0);
                // "bltzal/bgezal will always place the return address in
                // $ra, whether or not the branch is taken", and the
                // comparison uses rs from before linking (rs was read above).
                if (i.0 >> 17) & 0xF == 0x8 {
                    self.write(31, self.next_pc);
                }
                self.branch(taken, i.simm());
            }
            // psx-spx: "CPU Jump Opcodes".
            0x02 => self.jump((self.pc & 0xF000_0000) | (i.target() << 2)),
            0x03 => {
                self.write(31, self.next_pc); // ra = $+8
                self.jump((self.pc & 0xF000_0000) | (i.target() << 2));
            }
            0x04 => self.branch(rs == rt, i.simm()),
            0x05 => self.branch(rs != rt, i.simm()),
            0x06 => self.branch(rs as i32 <= 0, i.simm()),
            0x07 => self.branch(rs as i32 > 0, i.simm()),

            // psx-spx: "CPU ALU Opcodes" — immediate forms.
            0x08 => {
                // ADDI: overflow trap, rt left unchanged.
                let value = (rs as i32).checked_add(i.simm() as i32).ok_or(Exception::Overflow)?;
                self.write(i.rt(), value as u32);
            }
            0x09 => self.write(i.rt(), rs.wrapping_add(i.simm())),
            0x0A => self.write(i.rt(), ((rs as i32) < (i.simm() as i32)) as u32),
            0x0B => self.write(i.rt(), (rs < i.simm()) as u32),
            0x0C => self.write(i.rt(), rs & i.imm()),
            0x0D => self.write(i.rt(), rs | i.imm()),
            0x0E => self.write(i.rt(), rs ^ i.imm()),
            0x0F => self.write(i.rt(), i.imm() << 16),

            0x10 => return self.execute_cop0(i),
            // psx-spx: "cop0r12 - SR": COP1/COP3 "none in PSX". psx-spx does
            // not say what happens if CU1/CU3 are set and they are used;
            // treated as coprocessor unusable either way.
            0x11 => return Err(self.coprocessor_unusable(1)),
            0x12 => return self.execute_cop2(i),
            0x13 => return Err(self.coprocessor_unusable(3)),

            // psx-spx: "CPU Load/Store Opcodes".
            0x20 => {
                let value = self.load8(bus, addr)? as i8 as u32;
                self.load_delayed(i.rt(), value);
            }
            0x21 => {
                let value = self.load16(bus, addr)? as i16 as u32;
                self.load_delayed(i.rt(), value);
            }
            0x22 => {
                // LWL. psx-spx: "Unaligned Load/Store (Details)".
                let cur = self.reg_with_pending_load(i.rt());
                let word = self.load32(bus, addr & !3)?;
                let value = match addr & 3 {
                    0 => (cur & 0x00FF_FFFF) | (word << 24),
                    1 => (cur & 0x0000_FFFF) | (word << 16),
                    2 => (cur & 0x0000_00FF) | (word << 8),
                    _ => word,
                };
                self.load_delayed(i.rt(), value);
            }
            0x23 => {
                let value = self.load32(bus, addr)?;
                self.load_delayed(i.rt(), value);
            }
            0x24 => {
                let value = self.load8(bus, addr)? as u32;
                self.load_delayed(i.rt(), value);
            }
            0x25 => {
                let value = self.load16(bus, addr)? as u32;
                self.load_delayed(i.rt(), value);
            }
            0x26 => {
                // LWR. psx-spx: "Unaligned Load/Store (Details)".
                let cur = self.reg_with_pending_load(i.rt());
                let word = self.load32(bus, addr & !3)?;
                let value = match addr & 3 {
                    0 => word,
                    1 => (cur & 0xFF00_0000) | (word >> 8),
                    2 => (cur & 0xFFFF_0000) | (word >> 16),
                    _ => (cur & 0xFFFF_FF00) | (word >> 24),
                };
                self.load_delayed(i.rt(), value);
            }
            0x28 => self.store8(bus, addr, rt as u8)?,
            0x29 => self.store16(bus, addr, rt as u16)?,
            0x2A => {
                // SWL. psx-spx: "Unaligned Load/Store (Details)".
                let aligned = addr & !3;
                let mem = self.load32(bus, aligned)?;
                let value = match addr & 3 {
                    0 => (mem & 0xFFFF_FF00) | (rt >> 24),
                    1 => (mem & 0xFFFF_0000) | (rt >> 16),
                    2 => (mem & 0xFF00_0000) | (rt >> 8),
                    _ => rt,
                };
                self.store32(bus, aligned, value)?;
            }
            0x2B => self.store32(bus, addr, rt)?,
            0x2E => {
                // SWR. psx-spx: "Unaligned Load/Store (Details)".
                let aligned = addr & !3;
                let mem = self.load32(bus, aligned)?;
                let value = match addr & 3 {
                    0 => rt,
                    1 => (mem & 0x0000_00FF) | (rt << 8),
                    2 => (mem & 0x0000_FFFF) | (rt << 16),
                    _ => (mem & 0x00FF_FFFF) | (rt << 24),
                };
                self.store32(bus, aligned, value)?;
            }

            // psx-spx: "COP0 - Misc" — "mov [mem],cop0reg / mov cop0reg,[mem]"
            // are not supported and raise coprocessor unusable. COP1/COP3
            // do not exist (see 0x11/0x13 above).
            0x30 | 0x38 => return Err(self.coprocessor_unusable(0)),
            0x31 | 0x39 => return Err(self.coprocessor_unusable(1)),
            0x33 | 0x3B => return Err(self.coprocessor_unusable(3)),
            0x32 => {
                // LWC2: GTE data register load. GTE is Phase 4; the memory
                // access (and its exceptions) still happens.
                self.require_cop2()?;
                self.load32(bus, addr)?;
                self.gte_unimplemented += 1;
            }
            0x3A => {
                // SWC2: stores a GTE data register (reads as zero for now).
                self.require_cop2()?;
                self.store32(bus, addr, 0)?;
                self.gte_unimplemented += 1;
            }

            // psx-spx: "Illegal Opcodes" — all N/A opcodes raise RI.
            _ => return Err(Exception::Reserved),
        }
        Ok(())
    }

    /// psx-spx: "CPU Opcode Encoding" — secondary opcode field.
    fn execute_special(&mut self, i: Instr) -> Result<(), Exception> {
        let rs = self.regs[i.rs()];
        let rt = self.regs[i.rt()];

        match i.funct() {
            // psx-spx: "shifting instructions" — shifts use rt as operand,
            // variable shifts use the low 5 bits of rs.
            0x00 => self.write(i.rd(), rt << i.shamt()),
            0x02 => self.write(i.rd(), rt >> i.shamt()),
            0x03 => self.write(i.rd(), ((rt as i32) >> i.shamt()) as u32),
            0x04 => self.write(i.rd(), rt << (rs & 0x1F)),
            0x06 => self.write(i.rd(), rt >> (rs & 0x1F)),
            0x07 => self.write(i.rd(), ((rt as i32) >> (rs & 0x1F)) as u32),

            // psx-spx: "CPU Jump Opcodes".
            0x08 => self.jump(rs),
            0x09 => {
                // JALR: rd = $+8. rs was read before the link is written, so
                // "jalr r31,r31" jumps to the old value.
                self.write(i.rd(), self.next_pc);
                self.jump(rs);
            }
            // psx-spx: "exception opcodes" — taken immediately.
            0x0C => return Err(Exception::Syscall),
            0x0D => return Err(Exception::Break),

            // psx-spx: "Multiply/divide".
            0x10 => self.write(i.rd(), self.hi),
            0x11 => self.hi = rs,
            0x12 => self.write(i.rd(), self.lo),
            0x13 => self.lo = rs,
            0x18 => {
                let product = (rs as i32 as i64) * (rt as i32 as i64);
                self.hi = (product >> 32) as u32;
                self.lo = product as u32;
            }
            0x19 => {
                let product = (rs as u64) * (rt as u64);
                self.hi = (product >> 32) as u32;
                self.lo = product as u32;
            }
            0x1A => {
                // DIV never traps; psx-spx tabulates the error results.
                let (n, d) = (rs as i32, rt as i32);
                if d == 0 {
                    self.hi = rs;
                    self.lo = if n >= 0 { 0xFFFF_FFFF } else { 1 };
                } else if n == i32::MIN && d == -1 {
                    self.hi = 0;
                    self.lo = 0x8000_0000;
                } else {
                    self.hi = (n % d) as u32;
                    self.lo = (n / d) as u32;
                }
            }
            0x1B => {
                if rt == 0 {
                    self.hi = rs;
                    self.lo = 0xFFFF_FFFF;
                } else {
                    self.hi = rs % rt;
                    self.lo = rs / rt;
                }
            }

            // psx-spx: "arithmetic instructions" — ADD/SUB trap on overflow
            // and leave rd unchanged.
            0x20 => {
                let value = (rs as i32).checked_add(rt as i32).ok_or(Exception::Overflow)?;
                self.write(i.rd(), value as u32);
            }
            0x21 => self.write(i.rd(), rs.wrapping_add(rt)),
            0x22 => {
                let value = (rs as i32).checked_sub(rt as i32).ok_or(Exception::Overflow)?;
                self.write(i.rd(), value as u32);
            }
            0x23 => self.write(i.rd(), rs.wrapping_sub(rt)),
            // psx-spx: "logical instructions" / "comparison instructions".
            0x24 => self.write(i.rd(), rs & rt),
            0x25 => self.write(i.rd(), rs | rt),
            0x26 => self.write(i.rd(), rs ^ rt),
            0x27 => self.write(i.rd(), !(rs | rt)),
            0x2A => self.write(i.rd(), ((rs as i32) < (rt as i32)) as u32),
            0x2B => self.write(i.rd(), (rs < rt) as u32),

            // psx-spx: "Illegal Opcodes".
            _ => return Err(Exception::Reserved),
        }
        Ok(())
    }

    /// psx-spx: "Coprocessor Opcode/Parameter Encoding", "COP0 - Misc".
    fn execute_cop0(&mut self, i: Instr) -> Result<(), Exception> {
        let field = i.rs();
        let reg = i.rd();

        // psx-spx: "cop0r16-r31 - Garbage": readable without exception even
        // in user mode with COP0 disabled; everything else then raises CpU.
        let garbage_read = field == 0x00 && reg >= 16;
        let usable = self.cop0.sr & SR_KUC == 0 || self.cop0.sr & SR_CU0 != 0;
        if !usable && !garbage_read {
            return Err(self.coprocessor_unusable(0));
        }

        match field {
            0x00 => {
                // MFC0. psx-spx: "COP0 - Register Summary".
                let value = match reg {
                    3 => self.cop0.bpc,
                    5 => self.cop0.bda,
                    6 => self.cop0.tar,
                    7 => self.cop0.dcic,
                    8 => self.cop0.badvaddr,
                    9 => self.cop0.bdam,
                    11 => self.cop0.bpcm,
                    12 => self.cop0.sr,
                    13 => self.cop0.cause,
                    14 => self.cop0.epc,
                    15 => PRID,
                    // "Garbage" on hardware (value depends on recent bus
                    // activity); zero here.
                    16..=31 => 0,
                    // r0-r2, r4, r10: reading raises RI.
                    _ => return Err(Exception::Reserved),
                };
                // psx-spx: "CPU Coprocessor Opcodes" / "Caution - Load Delay".
                self.load_delayed(i.rt(), value);
            }
            0x04 => {
                // MTC0. Read-only and missing registers ignore writes.
                // Breakpoints (BPC/BDA/DCIC) are stored but not acted upon.
                let value = self.regs[i.rt()];
                match reg {
                    3 => self.cop0.bpc = value,
                    5 => self.cop0.bda = value,
                    7 => self.cop0.dcic = value,
                    9 => self.cop0.bdam = value,
                    11 => self.cop0.bpcm = value,
                    12 => self.cop0.sr = value & SR_WRITE_MASK,
                    // psx-spx: "cop0r13 - CAUSE": only bits 8-9 are R/W.
                    13 => self.cop0.cause = (self.cop0.cause & !CAUSE_SW) | (value & CAUSE_SW),
                    _ => {}
                }
            }
            // CFC0/CTC0: psx-spx "cop0r32..r63" (control registers) — N/A,
            // reading raises RI. Writing is not documented; RI as well.
            0x02 | 0x06 => return Err(Exception::Reserved),
            // BC0F/BC0T: psx-spx "jf/jt cop0flg,dest" — coprocessor unusable.
            0x08 => return Err(self.coprocessor_unusable(0)),
            // psx-spx: "cop0cmd=10h - RFE": SR bits 2-3 -> 0-1 and
            // bits 4-5 -> 2-3; bits 4-5 are left unchanged.
            0x10..=0x1F if i.funct() == 0x10 => {
                let sr = self.cop0.sr;
                self.cop0.sr = (sr & !0x0F) | ((sr >> 2) & 0x0F);
            }
            // TLBR/TLBWI/TLBWR/TLBP and anything else: RI.
            _ => return Err(Exception::Reserved),
        }
        Ok(())
    }

    fn require_cop2(&mut self) -> Result<(), Exception> {
        // psx-spx: "cop0r12 - SR" bit 30 (CU2).
        if self.cop0.sr & SR_CU2 == 0 {
            return Err(self.coprocessor_unusable(2));
        }
        Ok(())
    }

    /// COP2 is the GTE (Phase 4). Until then commands and register writes do
    /// nothing and register reads return zero, and each use is counted so
    /// the harness can report it.
    fn execute_cop2(&mut self, i: Instr) -> Result<(), Exception> {
        self.require_cop2()?;
        self.gte_unimplemented += 1;
        if matches!(i.rs(), 0x00 | 0x02) {
            // MFC2/CFC2 have a load delay like memory loads.
            self.load_delayed(i.rt(), 0);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u32 = 0x8001_0000;
    const NOP: u32 = 0;

    // ---- tiny assembler ----------------------------------------------------

    fn itype(op: u32, rs: u32, rt: u32, imm: u32) -> u32 {
        (op << 26) | (rs << 21) | (rt << 16) | (imm & 0xFFFF)
    }
    fn rtype(funct: u32, rs: u32, rt: u32, rd: u32, shamt: u32) -> u32 {
        (rs << 21) | (rt << 16) | (rd << 11) | (shamt << 6) | funct
    }
    fn addiu(rt: u32, rs: u32, imm: u32) -> u32 {
        itype(0x09, rs, rt, imm)
    }
    fn addi(rt: u32, rs: u32, imm: u32) -> u32 {
        itype(0x08, rs, rt, imm)
    }
    fn ori(rt: u32, rs: u32, imm: u32) -> u32 {
        itype(0x0D, rs, rt, imm)
    }
    fn lui(rt: u32, imm: u32) -> u32 {
        itype(0x0F, 0, rt, imm)
    }
    /// Two instructions loading any 32-bit constant.
    fn li(rt: u32, value: u32) -> [u32; 2] {
        [lui(rt, value >> 16), ori(rt, rt, value & 0xFFFF)]
    }
    fn lw(rt: u32, rs: u32, imm: u32) -> u32 {
        itype(0x23, rs, rt, imm)
    }
    fn sw(rt: u32, rs: u32, imm: u32) -> u32 {
        itype(0x2B, rs, rt, imm)
    }
    fn beq(rs: u32, rt: u32, off: u32) -> u32 {
        itype(0x04, rs, rt, off)
    }
    fn mfc0(rt: u32, rd: u32) -> u32 {
        (0x10 << 26) | (rt << 16) | (rd << 11)
    }
    fn mtc0(rt: u32, rd: u32) -> u32 {
        (0x10 << 26) | (0x04 << 21) | (rt << 16) | (rd << 11)
    }
    const RFE: u32 = 0x4200_0010;
    const SYSCALL: u32 = 0x0000_000C;
    const BREAK: u32 = 0x0000_000D;

    struct Machine {
        cpu: Cpu,
        bus: Bus,
    }

    impl Machine {
        /// Program at BASE; SR cleared so exceptions vector to RAM (BEV=0).
        fn new(program: &[u32]) -> Self {
            let mut bus = Bus::new(vec![0; map::BIOS_SIZE]).unwrap();
            let bytes: Vec<u8> = program.iter().flat_map(|w| w.to_le_bytes()).collect();
            bus.write_ram(BASE & 0x1F_FFFF, &bytes);
            let mut cpu = Cpu::new();
            cpu.set_pc(BASE);
            cpu.cop0.sr = 0;
            Self { cpu, bus }
        }

        fn run(program: &[u32]) -> Self {
            let mut m = Self::new(program);
            m.steps(program.len());
            m
        }

        fn steps(&mut self, n: usize) {
            for _ in 0..n {
                self.cpu.step(&mut self.bus);
            }
        }

        fn excode(&self) -> u32 {
            (self.cpu.cause() >> 2) & 0x1F
        }

        /// Asserts that an exception of kind `e` was just taken.
        fn assert_exception(&self, e: Exception, epc: u32) {
            assert_eq!(self.excode(), e as u32, "ExcCode");
            assert_eq!(self.cpu.epc(), epc, "EPC");
            assert_eq!(self.cpu.pc(), VECTOR_GENERAL_RAM, "vector");
        }
    }

    /// Runs `setup`, then one instruction, and returns the machine.
    fn with_regs(regs: &[(usize, u32)], instr: u32) -> Machine {
        let mut m = Machine::new(&[instr]);
        for &(r, v) in regs {
            m.cpu.set_reg(r, v);
        }
        m.steps(1);
        m
    }

    // ---- reset -------------------------------------------------------------

    #[test]
    fn reset_state() {
        let cpu = Cpu::new();
        assert_eq!(cpu.pc(), 0xBFC0_0000);
        assert_ne!(cpu.sr() & SR_BEV, 0);
        assert_eq!(cpu.sr() & 0x3, 0, "kernel mode, interrupts off");
    }

    #[test]
    fn r0_is_always_zero() {
        let m = Machine::run(&[addiu(0, 0, 5), ori(1, 0, 0)]);
        assert_eq!(m.cpu.reg(0), 0);
        assert_eq!(m.cpu.reg(1), 0);
    }

    // ---- ALU ---------------------------------------------------------------

    #[test]
    fn immediates_sign_and_zero_extension() {
        // psx-spx: "CPU ALU Opcodes".
        let m = Machine::run(&[
            addiu(1, 0, 0xFFFF),       // sign-extended: -1
            ori(2, 0, 0xFFFF),         // zero-extended
            itype(0x0C, 1, 3, 0xFF00), // andi
            itype(0x0E, 1, 4, 0xFFFF), // xori
            lui(5, 0x8000),
            itype(0x0A, 1, 6, 0),      // slti  -1 < 0
            itype(0x0B, 1, 7, 0),      // sltiu 0xFFFFFFFF < 0
            itype(0x0B, 0, 8, 0xFFFF), // sltiu 0 < 0xFFFFFFFF (sign-extended)
        ]);
        assert_eq!(m.cpu.reg(1), 0xFFFF_FFFF);
        assert_eq!(m.cpu.reg(2), 0x0000_FFFF);
        assert_eq!(m.cpu.reg(3), 0x0000_FF00);
        assert_eq!(m.cpu.reg(4), 0xFFFF_0000);
        assert_eq!(m.cpu.reg(5), 0x8000_0000);
        assert_eq!(m.cpu.reg(6), 1);
        assert_eq!(m.cpu.reg(7), 0);
        assert_eq!(m.cpu.reg(8), 1);
    }

    #[test]
    fn register_alu() {
        let regs = [(1, 0xF0F0_1234), (2, 0x0FF0_00FF)];
        let f = |funct| with_regs(&regs, rtype(funct, 1, 2, 3, 0)).cpu.reg(3);
        assert_eq!(f(0x21), 0xF0F0_1234u32.wrapping_add(0x0FF0_00FF)); // addu
        assert_eq!(f(0x23), 0xF0F0_1234u32.wrapping_sub(0x0FF0_00FF)); // subu
        assert_eq!(f(0x24), 0x00F0_0034); // and
        assert_eq!(f(0x25), 0xFFF0_12FF); // or
        assert_eq!(f(0x26), 0xFF00_12CB); // xor
        assert_eq!(f(0x27), 0x000F_ED00); // nor
        assert_eq!(f(0x2A), 1); // slt: negative < positive
        assert_eq!(f(0x2B), 0); // sltu
    }

    #[test]
    fn shifts() {
        // psx-spx: "shifting instructions" — rt is the shifted operand.
        let regs = [(1, 0x8000_00F0), (2, 0xFFFF_FF24)]; // r2 & 1F = 4
        assert_eq!(with_regs(&regs, rtype(0x00, 0, 1, 3, 4)).cpu.reg(3), 0x0000_0F00);
        assert_eq!(with_regs(&regs, rtype(0x02, 0, 1, 3, 4)).cpu.reg(3), 0x0800_000F);
        assert_eq!(with_regs(&regs, rtype(0x03, 0, 1, 3, 4)).cpu.reg(3), 0xF800_000F);
        assert_eq!(with_regs(&regs, rtype(0x04, 2, 1, 3, 0)).cpu.reg(3), 0x0000_0F00);
        assert_eq!(with_regs(&regs, rtype(0x06, 2, 1, 3, 0)).cpu.reg(3), 0x0800_000F);
        assert_eq!(with_regs(&regs, rtype(0x07, 2, 1, 3, 0)).cpu.reg(3), 0xF800_000F);
    }

    #[test]
    fn add_overflow_traps_and_leaves_destination() {
        // psx-spx: "arithmetic instructions".
        let regs = [(1, 0x7FFF_FFFF), (2, 1), (3, 0xAAAA_AAAA)];
        let m = with_regs(&regs, rtype(0x20, 1, 2, 3, 0));
        m.assert_exception(Exception::Overflow, BASE);
        assert_eq!(m.cpu.reg(3), 0xAAAA_AAAA);

        let m = with_regs(&regs, addi(3, 1, 1));
        m.assert_exception(Exception::Overflow, BASE);
        assert_eq!(m.cpu.reg(3), 0xAAAA_AAAA);

        // sub: MIN - 1 overflows.
        let m = with_regs(&[(1, 0x8000_0000), (2, 1)], rtype(0x22, 1, 2, 3, 0));
        m.assert_exception(Exception::Overflow, BASE);

        // No trap without overflow, and none for the unsigned forms.
        let m = with_regs(&regs, addi(3, 2, 0xFFFF));
        assert_eq!(m.cpu.reg(3), 0);
        assert_eq!(m.cpu.pc(), BASE + 4);
        let m = with_regs(&regs, rtype(0x21, 1, 2, 3, 0));
        assert_eq!(m.cpu.reg(3), 0x8000_0000);
    }

    #[test]
    fn multiply() {
        // psx-spx: "Multiply/divide".
        let regs = [(1, 0xFFFF_FFFE), (2, 3)];
        let m = with_regs(&regs, rtype(0x18, 1, 2, 0, 0)); // mult: -2 * 3
        assert_eq!((m.cpu.hi(), m.cpu.lo()), (0xFFFF_FFFF, 0xFFFF_FFFA));
        let m = with_regs(&regs, rtype(0x19, 1, 2, 0, 0)); // multu
        assert_eq!((m.cpu.hi(), m.cpu.lo()), (2, 0xFFFF_FFFA));
    }

    #[test]
    fn divide_including_error_cases() {
        // psx-spx: "Multiply/divide" — table of divide error results.
        let div = |n: u32, d: u32| {
            let m = with_regs(&[(1, n), (2, d)], rtype(0x1A, 1, 2, 0, 0));
            assert_eq!(m.cpu.pc(), BASE + 4, "div never traps");
            (m.cpu.hi(), m.cpu.lo())
        };
        let divu = |n: u32, d: u32| {
            let m = with_regs(&[(1, n), (2, d)], rtype(0x1B, 1, 2, 0, 0));
            (m.cpu.hi(), m.cpu.lo())
        };
        assert_eq!(div(7, 2), (1, 3));
        assert_eq!(div(-7i32 as u32, 2), (-1i32 as u32, -3i32 as u32));
        assert_eq!(div(5, 0), (5, 0xFFFF_FFFF));
        assert_eq!(div(-5i32 as u32, 0), (-5i32 as u32, 1));
        assert_eq!(div(0x8000_0000, 0xFFFF_FFFF), (0, 0x8000_0000));
        assert_eq!(divu(0xFFFF_FFFF, 0x10), (0xF, 0x0FFF_FFFF));
        assert_eq!(divu(9, 0), (9, 0xFFFF_FFFF));
    }

    #[test]
    fn hi_lo_moves() {
        let mut m = Machine::new(&[
            rtype(0x11, 1, 0, 0, 0), // mthi r1
            rtype(0x13, 2, 0, 0, 0), // mtlo r2
            rtype(0x10, 0, 0, 3, 0), // mfhi r3
            rtype(0x12, 0, 0, 4, 0), // mflo r4
        ]);
        m.cpu.set_reg(1, 11);
        m.cpu.set_reg(2, 22);
        m.steps(4);
        assert_eq!((m.cpu.reg(3), m.cpu.reg(4)), (11, 22));
    }

    // ---- branches and jumps ------------------------------------------------

    #[test]
    fn branch_delay_slot_always_executes() {
        // psx-spx: "CPU Jump Opcodes".
        let m = Machine::run(&[
            beq(0, 0, 2),    // +0: taken, target = +4 + 2*4 = +12
            addiu(1, 0, 1),  // +4: delay slot, executes
            addiu(2, 0, 1),  // +8: skipped
            addiu(3, 0, 1),  // +12: target
        ][..]);
        // 4 steps: beq, slot, target, and one zero word (nop) after it.
        assert_eq!((m.cpu.reg(1), m.cpu.reg(2), m.cpu.reg(3)), (1, 0, 1));
    }

    #[test]
    fn untaken_branch_falls_through() {
        let m = Machine::run(&[itype(0x05, 0, 0, 2), addiu(1, 0, 1), addiu(2, 0, 1)]);
        assert_eq!((m.cpu.reg(1), m.cpu.reg(2)), (1, 1));
    }

    #[test]
    fn backward_branch() {
        // Loop: r1 counts to 3.
        let mut m = Machine::new(&[
            addiu(2, 0, 3),
            addiu(1, 1, 1),         // +4
            itype(0x05, 1, 2, 0xFFFE), // +8: bne r1,r2 -> +4
            NOP,
            addiu(3, 0, 9),         // +16
        ]);
        m.steps(1 + 3 * 3 + 1);
        assert_eq!((m.cpu.reg(1), m.cpu.reg(3)), (3, 9));
    }

    #[test]
    fn conditional_branch_conditions() {
        // (opcode/rt selector, rs value, expected taken)
        let taken = |instr: u32, rs: u32| {
            let mut m = Machine::new(&[instr, NOP, NOP]);
            m.cpu.set_reg(1, rs);
            m.steps(2);
            m.cpu.pc() == BASE + 4 + 8
        };
        let neg = 0xFFFF_FFFF;
        let blez = itype(0x06, 1, 0, 2);
        let bgtz = itype(0x07, 1, 0, 2);
        let bltz = itype(0x01, 1, 0x00, 2);
        let bgez = itype(0x01, 1, 0x01, 2);
        assert_eq!([taken(blez, neg), taken(blez, 0), taken(blez, 1)], [true, true, false]);
        assert_eq!([taken(bgtz, neg), taken(bgtz, 0), taken(bgtz, 1)], [false, false, true]);
        assert_eq!([taken(bltz, neg), taken(bltz, 0), taken(bltz, 1)], [true, false, false]);
        assert_eq!([taken(bgez, neg), taken(bgez, 0), taken(bgez, 1)], [false, true, true]);
    }

    #[test]
    fn bltzal_links_even_when_not_taken() {
        // psx-spx: "CPU Jump Opcodes".
        let mut m = Machine::new(&[itype(0x01, 1, 0x10, 4), NOP, NOP]);
        m.cpu.set_reg(1, 5); // not negative: not taken
        m.steps(2);
        assert_eq!(m.cpu.reg(31), BASE + 8);
        assert_eq!(m.cpu.pc(), BASE + 8);

        let mut m = Machine::new(&[itype(0x01, 1, 0x11, 4), NOP, NOP]); // bgezal
        m.cpu.set_reg(1, 5);
        m.steps(2);
        assert_eq!(m.cpu.reg(31), BASE + 8);
        assert_eq!(m.cpu.pc(), BASE + 4 + 16);
    }

    #[test]
    fn bgezal_compares_ra_before_linking() {
        // psx-spx: "if rs is $ra, the value used for the comparison is $ra's
        // value before linking".
        let mut m = Machine::new(&[itype(0x01, 31, 0x11, 4), NOP]);
        m.cpu.set_reg(31, 0xFFFF_FFFF); // negative: bgezal not taken
        m.steps(2);
        assert_eq!(m.cpu.pc(), BASE + 8);
        assert_eq!(m.cpu.reg(31), BASE + 8);
    }

    #[test]
    fn jumps_and_links() {
        // j: pc = (pc & F0000000) + imm26*4
        let m = Machine::run(&[(0x02 << 26) | 0x0004_1000, NOP]);
        assert_eq!(m.cpu.pc(), 0x8010_4000);
        // jal: ra = $+8
        let m = Machine::run(&[(0x03 << 26) | 0x0004_1000, NOP]);
        assert_eq!(m.cpu.pc(), 0x8010_4000);
        assert_eq!(m.cpu.reg(31), BASE + 8);
        // jr
        let mut m = Machine::new(&[rtype(0x08, 1, 0, 0, 0), NOP]);
        m.cpu.set_reg(1, 0x8002_0000);
        m.steps(2);
        assert_eq!(m.cpu.pc(), 0x8002_0000);
        // jalr rd=5
        let mut m = Machine::new(&[rtype(0x09, 1, 0, 5, 0), NOP]);
        m.cpu.set_reg(1, 0x8002_0000);
        m.steps(2);
        assert_eq!(m.cpu.pc(), 0x8002_0000);
        assert_eq!(m.cpu.reg(5), BASE + 8);
    }

    #[test]
    fn jalr_same_register_jumps_to_old_value() {
        let mut m = Machine::new(&[rtype(0x09, 31, 0, 31, 0), NOP]);
        m.cpu.set_reg(31, 0x8002_0000);
        m.steps(2);
        assert_eq!(m.cpu.pc(), 0x8002_0000);
        assert_eq!(m.cpu.reg(31), BASE + 8);
    }

    #[test]
    fn jump_to_unaligned_address_faults_at_target() {
        // psx-spx: "CPU Jump Opcodes" / "cop0r14 - EPC".
        let mut m = Machine::new(&[rtype(0x08, 1, 0, 0, 0), addiu(2, 0, 7)]);
        m.cpu.set_reg(1, 0x8002_0002);
        m.steps(3);
        m.assert_exception(Exception::AddressLoad, 0x8002_0002);
        assert_eq!(m.cpu.badvaddr(), 0x8002_0002);
        assert_eq!(m.cpu.cause() & CAUSE_BD, 0);
        assert_eq!(m.cpu.reg(2), 7, "delay slot still ran");
    }

    // ---- loads and stores --------------------------------------------------

    const DATA: u32 = 0x8002_0000;

    /// Machine with r1 = DATA and a word stored there.
    fn with_data(word: u32, program: &[u32]) -> Machine {
        let mut m = Machine::new(program);
        m.bus.store32(DATA, word).unwrap();
        m.cpu.set_reg(1, DATA);
        m
    }

    #[test]
    fn load_widths_and_extension() {
        let mut m = with_data(
            0x8182_F384,
            &[
                itype(0x20, 1, 2, 0), // lb
                itype(0x24, 1, 3, 0), // lbu
                itype(0x21, 1, 4, 2), // lh
                itype(0x25, 1, 5, 2), // lhu
                lw(6, 1, 0),
                NOP,
            ],
        );
        m.steps(6);
        assert_eq!(m.cpu.reg(2), 0xFFFF_FF84);
        assert_eq!(m.cpu.reg(3), 0x0000_0084);
        assert_eq!(m.cpu.reg(4), 0xFFFF_8182);
        assert_eq!(m.cpu.reg(5), 0x0000_8182);
        assert_eq!(m.cpu.reg(6), 0x8182_F384);
    }

    #[test]
    fn store_widths() {
        let mut m = with_data(
            0,
            &[
                itype(0x28, 1, 2, 0), // sb
                itype(0x29, 1, 2, 2), // sh
                sw(2, 1, 4),
            ],
        );
        m.cpu.set_reg(2, 0x1122_3344);
        m.steps(3);
        assert_eq!(m.bus.load32(DATA).unwrap(), 0x3344_0044);
        assert_eq!(m.bus.load32(DATA + 4).unwrap(), 0x1122_3344);
    }

    #[test]
    fn negative_load_offset() {
        let mut m = with_data(0xCAFE_BABE, &[lw(2, 3, 0xFFFC), NOP]);
        m.cpu.set_reg(3, DATA + 4);
        m.steps(2);
        assert_eq!(m.cpu.reg(2), 0xCAFE_BABE);
    }

    #[test]
    fn load_delay_next_instruction_sees_old_value() {
        // psx-spx: "Caution - Load Delay".
        let mut m = with_data(
            0x1234_5678,
            &[
                lw(2, 1, 0),
                rtype(0x21, 2, 0, 3, 0), // addu r3 = r2 (old value)
                rtype(0x21, 2, 0, 4, 0), // addu r4 = r2 (new value)
            ],
        );
        m.cpu.set_reg(2, 0xAAAA);
        m.steps(3);
        assert_eq!(m.cpu.reg(3), 0xAAAA);
        assert_eq!(m.cpu.reg(4), 0x1234_5678);
    }

    #[test]
    fn write_in_load_delay_slot_wins() {
        let mut m = with_data(0x1234_5678, &[lw(2, 1, 0), addiu(2, 0, 7), NOP]);
        m.steps(3);
        assert_eq!(m.cpu.reg(2), 7);
    }

    #[test]
    fn second_load_to_same_register_cancels_first() {
        // Not in psx-spx; hardware behaviour checked by amidog's CPU tests.
        let mut m = with_data(
            0x1111_1111,
            &[
                lw(2, 1, 0),             // would load 0x11111111
                lw(2, 1, 4),             // loads 0x22222222
                rtype(0x21, 2, 0, 3, 0), // still sees the original r2
                NOP,
            ],
        );
        m.bus.store32(DATA + 4, 0x2222_2222).unwrap();
        m.cpu.set_reg(2, 0xAAAA);
        m.steps(4);
        assert_eq!(m.cpu.reg(3), 0xAAAA);
        assert_eq!(m.cpu.reg(2), 0x2222_2222);
    }

    #[test]
    fn mfc0_has_a_load_delay() {
        // psx-spx: "CPU Coprocessor Opcodes" / "Caution - Load Delay".
        let mut m = Machine::new(&[mfc0(2, 15), rtype(0x21, 2, 0, 3, 0), NOP]);
        m.cpu.set_reg(2, 0xAAAA);
        m.steps(3);
        assert_eq!(m.cpu.reg(3), 0xAAAA);
        assert_eq!(m.cpu.reg(2), PRID);
    }

    #[test]
    fn lwl_lwr_every_offset() {
        // psx-spx: "Unaligned Load/Store (Details)".
        let lwl = |off| {
            let mut m = with_data(0x1122_3344, &[itype(0x22, 1, 2, off), NOP]);
            m.cpu.set_reg(2, 0xAABB_CCDD);
            m.steps(2);
            m.cpu.reg(2)
        };
        let lwr = |off| {
            let mut m = with_data(0x1122_3344, &[itype(0x26, 1, 2, off), NOP]);
            m.cpu.set_reg(2, 0xAABB_CCDD);
            m.steps(2);
            m.cpu.reg(2)
        };
        assert_eq!([lwl(0), lwl(1), lwl(2), lwl(3)], [0x44BB_CCDD, 0x3344_CCDD, 0x2233_44DD, 0x1122_3344]);
        assert_eq!([lwr(0), lwr(1), lwr(2), lwr(3)], [0x1122_3344, 0xAA11_2233, 0xAABB_1122, 0xAABB_CC11]);
    }

    #[test]
    fn lwl_then_lwr_needs_no_delay() {
        // psx-spx: "Unaligned Load/Store" — loads an unaligned word.
        let mut m = with_data(0x4433_2211, &[itype(0x22, 1, 2, 4), itype(0x26, 1, 2, 1), NOP]);
        m.bus.store32(DATA + 4, 0x8877_6655).unwrap();
        m.steps(3);
        assert_eq!(m.cpu.reg(2), 0x5544_3322);
    }

    #[test]
    fn swl_swr_every_offset() {
        // psx-spx: "Unaligned Load/Store (Details)".
        let store = |op, off| {
            let mut m = with_data(0x1122_3344, &[itype(op, 1, 2, off)]);
            m.cpu.set_reg(2, 0xAABB_CCDD);
            m.steps(1);
            m.bus.load32(DATA).unwrap()
        };
        let swl: Vec<u32> = (0..4).map(|o| store(0x2A, o)).collect();
        let swr: Vec<u32> = (0..4).map(|o| store(0x2E, o)).collect();
        assert_eq!(swl, [0x1122_33AA, 0x1122_AABB, 0x11AA_BBCC, 0xAABB_CCDD]);
        assert_eq!(swr, [0xAABB_CCDD, 0xBBCC_DD44, 0xCCDD_3344, 0xDD22_3344]);
    }

    #[test]
    fn misaligned_load_and_store_raise_address_errors() {
        // psx-spx: "Load/Store Alignment", "cop0r8 - BadVaddr".
        let mut m = with_data(0, &[lw(2, 1, 2), NOP]);
        m.cpu.set_reg(2, 0x55);
        m.steps(1);
        m.assert_exception(Exception::AddressLoad, BASE);
        assert_eq!(m.cpu.badvaddr(), DATA + 2);
        m.steps(1);
        assert_eq!(m.cpu.reg(2), 0x55, "rt untouched by a faulting load");

        let mut m = with_data(0, &[itype(0x29, 1, 2, 1)]); // sh
        m.steps(1);
        m.assert_exception(Exception::AddressStore, BASE);
        assert_eq!(m.cpu.badvaddr(), DATA + 1);

        let mut m = with_data(0, &[itype(0x21, 1, 2, 3)]); // lh
        m.steps(1);
        m.assert_exception(Exception::AddressLoad, BASE);

        let mut m = with_data(0, &[sw(2, 1, 1)]);
        m.steps(1);
        m.assert_exception(Exception::AddressStore, BASE);
    }

    #[test]
    fn unmapped_data_access_raises_bus_error() {
        // psx-spx: "Memory Map" / "Memory Exceptions"; BadVaddr unchanged.
        for instr in [lw(2, 1, 0), sw(2, 1, 0), itype(0x20, 1, 2, 0)] {
            let mut m = Machine::new(&[instr]);
            m.cpu.set_reg(1, 0x4000_0000);
            m.steps(1);
            m.assert_exception(Exception::BusData, BASE);
            assert_eq!(m.cpu.badvaddr(), 0);
        }
    }

    #[test]
    fn unmapped_fetch_raises_bus_error() {
        let mut m = Machine::new(&[]);
        m.cpu.set_pc(0x4000_0000);
        m.steps(1);
        m.assert_exception(Exception::BusFetch, 0x4000_0000);
        // psx-spx: "Scratchpad" — not executable.
        m.cpu.set_pc(0x1F80_0000);
        m.steps(1);
        m.assert_exception(Exception::BusFetch, 0x1F80_0000);
    }

    #[test]
    fn isolated_cache_stores_do_not_reach_ram() {
        // psx-spx: "cop0r12 - SR" bit 16.
        let mut m = with_data(0x1234_5678, &[sw(0, 1, 0)]);
        m.cpu.cop0.sr = SR_ISC;
        m.steps(1);
        assert_eq!(m.bus.load32(DATA).unwrap(), 0x1234_5678);
    }

    // ---- exceptions and COP0 -----------------------------------------------

    #[test]
    fn syscall_and_break() {
        // psx-spx: "exception opcodes", "COP0 - Exception Handling".
        let mut m = Machine::new(&[NOP, SYSCALL, addiu(1, 0, 1)]);
        // Kernel mode (KUc=0, the code is in KSEG0) with IEc, IEp and KUp
        // set: current -> previous, previous -> old, current = 0.
        m.cpu.cop0.sr = 0b00_11_01;
        m.steps(2);
        m.assert_exception(Exception::Syscall, BASE + 4);
        assert_eq!(m.cpu.sr() & 0x3F, 0b11_01_00, "mode stack pushed");
        assert_eq!(m.cpu.cause() & CAUSE_BD, 0);

        let m = Machine::run(&[BREAK]);
        m.assert_exception(Exception::Break, BASE);
    }

    #[test]
    fn exception_vector_depends_on_bev() {
        // psx-spx: "Exception Vectors".
        let mut m = Machine::new(&[SYSCALL]);
        m.cpu.cop0.sr = SR_BEV;
        m.steps(1);
        assert_eq!(m.cpu.pc(), 0xBFC0_0180);
    }

    #[test]
    fn exception_in_delay_slot_sets_bd_and_points_at_branch() {
        // psx-spx: "cop0r14 - EPC", "cop0r6 - TAR".
        let mut m = Machine::new(&[NOP, beq(0, 0, 4), SYSCALL, mfc0(2, 6), NOP]);
        m.steps(3);
        m.assert_exception(Exception::Syscall, BASE + 4);
        assert_ne!(m.cpu.cause() & CAUSE_BD, 0);
        assert_ne!(m.cpu.cause() & CAUSE_BT, 0);
        assert_eq!(m.cpu.cop0.tar, BASE + 8 + 16);

        // Untaken branch: BD set, BT clear.
        let mut m = Machine::new(&[itype(0x05, 0, 0, 4), SYSCALL]);
        m.steps(2);
        m.assert_exception(Exception::Syscall, BASE);
        assert_ne!(m.cpu.cause() & CAUSE_BD, 0);
        assert_eq!(m.cpu.cause() & CAUSE_BT, 0);
    }

    #[test]
    fn exception_after_delay_slot_is_not_bd() {
        let mut m = Machine::new(&[beq(0, 0, 1), NOP, SYSCALL]);
        m.steps(3);
        m.assert_exception(Exception::Syscall, BASE + 8);
        assert_eq!(m.cpu.cause() & CAUSE_BD, 0);
    }

    #[test]
    fn pending_load_completes_when_next_instruction_faults() {
        let mut m = with_data(0x1234_5678, &[lw(2, 1, 0), SYSCALL]);
        m.steps(2);
        assert_eq!(m.cpu.reg(2), 0x1234_5678);
    }

    #[test]
    fn rfe_pops_mode_stack() {
        // psx-spx: "cop0cmd=10h - RFE".
        let mut m = Machine::new(&[RFE]);
        m.cpu.cop0.sr = 0x0040_0000 | 0b10_01_00;
        m.steps(1);
        assert_eq!(m.cpu.sr(), 0x0040_0000 | 0b10_10_01);
    }

    #[test]
    fn mtc0_mfc0_roundtrip() {
        let [a, b] = li(1, 0x1040_0401);
        let mut m = Machine::new(&[a, b, mtc0(1, 12), mfc0(2, 12), mfc0(3, 15), NOP]);
        m.steps(6);
        assert_eq!(m.cpu.sr(), 0x1040_0401);
        assert_eq!(m.cpu.reg(2), 0x1040_0401);
        assert_eq!(m.cpu.reg(3), 2, "PRID");
    }

    #[test]
    fn sr_unused_bits_read_zero() {
        let mut m = Machine::new(&[mtc0(1, 12)]);
        m.cpu.set_reg(1, 0xFFFF_FFFC); // keep kernel mode, IEc off
        m.steps(1);
        assert_eq!(m.cpu.sr(), 0xFFFF_FFFC & SR_WRITE_MASK);
    }

    #[test]
    fn cause_only_software_interrupt_bits_are_writable() {
        // psx-spx: "cop0r13 - CAUSE".
        let mut m = Machine::new(&[mtc0(1, 13), mtc0(1, 14), mtc0(1, 8)]);
        m.cpu.set_reg(1, 0xFFFF_FFFF);
        m.steps(3);
        assert_eq!(m.cpu.cause(), 0x0000_0300);
        assert_eq!(m.cpu.epc(), 0, "EPC is read-only");
        assert_eq!(m.cpu.badvaddr(), 0, "BadVaddr is read-only");
    }

    #[test]
    fn reserved_instructions() {
        // psx-spx: "Illegal Opcodes", "COP0 - Misc".
        for instr in [
            0x14 << 26,              // primary N/A
            0x3F << 26,              // primary N/A
            0x27 << 26,              // primary N/A between loads
            rtype(0x01, 0, 0, 0, 0), // secondary N/A
            rtype(0x3F, 0, 0, 0, 0), // secondary N/A
            mfc0(1, 0),              // cop0r0 N/A
            mfc0(1, 10),             // cop0r10 N/A
            0x4200_0001,             // TLBR
            0x4200_0008,             // TLBP
        ] {
            let m = Machine::run(&[instr]);
            assert_eq!(m.excode(), Exception::Reserved as u32, "instr 0x{instr:08X}");
        }
    }

    #[test]
    fn coprocessor_unusable_sets_ce() {
        // psx-spx: "cop0r13 - CAUSE" bits 28-29, "cop0r12 - SR" CU bits.
        let ce = |m: &Machine| (m.cpu.cause() >> 28) & 3;
        let m = Machine::run(&[0x11 << 26]); // COP1
        assert_eq!((m.excode(), ce(&m)), (0x0B, 1));
        let m = Machine::run(&[0x13 << 26]); // COP3
        assert_eq!((m.excode(), ce(&m)), (0x0B, 3));
        let m = Machine::run(&[0x12 << 26]); // COP2 with CU2 clear
        assert_eq!((m.excode(), ce(&m)), (0x0B, 2));
        let m = Machine::run(&[0x30 << 26]); // LWC0
        assert_eq!((m.excode(), ce(&m)), (0x0B, 0));

        // COP2 enabled: no exception (GTE itself is Phase 4).
        let mut m = Machine::new(&[0x4A00_0001]);
        m.cpu.cop0.sr = SR_CU2;
        m.steps(1);
        assert_eq!(m.cpu.pc(), BASE + 4);
        assert_eq!(m.cpu.gte_unimplemented(), 1);
    }

    #[test]
    fn user_mode_restrictions() {
        // COP0 unusable in user mode without CU0.
        let mut m = Machine::new(&[mfc0(1, 12)]);
        m.cpu.set_pc(0x0001_0000); // same RAM through KUSEG
        m.cpu.cop0.sr = SR_KUC;
        m.steps(1);
        assert_eq!(m.excode(), Exception::CoprocessorUnusable as u32);

        // ...but usable with CU0.
        let mut m = Machine::new(&[mfc0(1, 12)]);
        m.cpu.set_pc(0x0001_0000);
        m.cpu.cop0.sr = SR_KUC | SR_CU0;
        m.steps(1);
        assert_eq!(m.cpu.pc(), 0x0001_0004);

        // Kernel segments are off limits: data and fetch.
        let mut m = with_data(0, &[lw(2, 1, 0)]);
        m.cpu.set_pc(0x0001_0000);
        m.cpu.cop0.sr = SR_KUC;
        m.steps(1);
        assert_eq!(m.excode(), Exception::AddressLoad as u32);
        assert_eq!(m.cpu.badvaddr(), DATA);

        let mut m = Machine::new(&[NOP]);
        m.cpu.cop0.sr = SR_KUC;
        m.steps(1);
        m.assert_exception(Exception::AddressLoad, BASE);
    }

    // ---- interrupts --------------------------------------------------------

    #[test]
    fn interrupt_taken_only_when_enabled_and_unmasked() {
        // psx-spx: "cop0r13 - CAUSE" IP, "cop0r12 - SR" Im/IEc.
        let prog = [addiu(1, 0, 1), addiu(2, 0, 2)];

        let mut m = Machine::new(&prog);
        m.cpu.set_irq(true);
        m.cpu.cop0.sr = 0x0400; // unmasked but IEc off
        m.steps(1);
        assert_eq!(m.cpu.reg(1), 1);

        let mut m = Machine::new(&prog);
        m.cpu.set_irq(true);
        m.cpu.cop0.sr = 0x0001; // enabled but masked
        m.steps(1);
        assert_eq!(m.cpu.reg(1), 1);

        let mut m = Machine::new(&prog);
        m.cpu.set_irq(true);
        m.cpu.cop0.sr = 0x0401;
        m.steps(1);
        m.assert_exception(Exception::Interrupt, BASE);
        assert_eq!(m.cpu.reg(1), 0, "interrupted instruction did not run");
        assert_ne!(m.cpu.cause() & CAUSE_IP2, 0, "IP stays visible");
        assert_eq!(m.cpu.sr() & 1, 0, "interrupts disabled in handler");

        m.cpu.set_irq(false);
        assert_eq!(m.cpu.cause() & CAUSE_IP2, 0);
    }

    #[test]
    fn software_interrupt() {
        let [a, b] = li(1, 0x0000_0101);
        let mut m = Machine::new(&[a, b, mtc0(1, 12), ori(2, 0, 0x100), mtc0(2, 13), NOP]);
        m.steps(6);
        m.assert_exception(Exception::Interrupt, BASE + 20);
    }

    #[test]
    fn cycles_count_instructions() {
        let m = Machine::run(&[NOP, NOP, NOP]);
        assert_eq!(m.cpu.cycles(), 3);
    }
}
