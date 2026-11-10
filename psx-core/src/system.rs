//! The whole console: CPU + bus, plus kernel-level hooks.
//!
//! The core does no I/O (see CLAUDE.md), so TTY output is collected here as
//! bytes and the frontend/harness decides what to do with it.

use crate::bus::{BiosError, Bus};
use crate::cpu::Cpu;
use crate::exe::Exe;
use crate::map::mask_region;

// psx-spx: "Kernel Memory" — function vectors in the first page of RAM, and
// "BIOS Function Summary": "A-Functions (Call 00A0h with function number in
// R9 Register)", likewise 00B0h for B-functions.
const A_VECTOR: u32 = 0x0000_00A0;
const B_VECTOR: u32 = 0x0000_00B0;
/// psx-spx: "A(3Ch) or B(3Dh) - putchar(char) - Write character to TTY".
const A_PUTCHAR: u32 = 0x3C;
const B_PUTCHAR: u32 = 0x3D;

/// Where the kernel starts the shell once it has finished setting itself
/// up. psx-spx: "Kernel (BIOS)" / "Cheat Devices" — Action Replay breaks
/// "when starting the GUI code at 80030000h", after the kernel "will then
/// initialize important things like A0h/B0h/C0h tables".
pub const SHELL_ENTRY: u32 = 0x8003_0000;

const REG_A0: usize = 4;
const REG_A1: usize = 5;
const REG_T1: usize = 9;
const REG_GP: usize = 28;
const REG_SP: usize = 29;
const REG_FP: usize = 30;

pub struct Psx {
    pub cpu: Cpu,
    pub bus: Bus,
    tty: Vec<u8>,
    cycles: u64,
}

impl Psx {
    /// Creates a console in its reset state with the given BIOS image.
    pub fn new(bios: Vec<u8>) -> Result<Self, BiosError> {
        Ok(Self { cpu: Cpu::new(), bus: Bus::new(bios)?, tty: Vec::new(), cycles: 0 })
    }

    /// Executes one CPU instruction and advances the rest of the hardware.
    pub fn step(&mut self) {
        self.hook_putchar();
        // psx-spx: "Interrupts" — cop0r13.bit10 follows I_STAT & I_MASK.
        self.cpu.set_irq(self.bus.irq.pending());
        self.cpu.step(&mut self.bus);
        // One cycle per instruction (see cpu.rs), plus the time the CPU was
        // held off the bus by DMA.
        let cycles = 1 + self.bus.take_stall();
        self.cycles += cycles as u64;
        self.bus.tick(cycles);
    }

    /// System clock cycles elapsed (instructions plus DMA stalls).
    pub fn cycles(&self) -> u64 {
        self.cycles
    }

    /// Runs the BIOS until it is about to start its shell, i.e. until the
    /// kernel is fully initialised. Returns false if that does not happen
    /// within `max_cycles`.
    pub fn run_to_shell(&mut self, max_cycles: u64) -> bool {
        while self.cpu.pc() != SHELL_ENTRY {
            if self.cycles >= max_cycles {
                return false;
            }
            self.step();
        }
        true
    }

    /// Places an executable in RAM and points the CPU at it, the way the
    /// kernel's Exec does. psx-spx: "PSX.EXE (Boot-Executable)".
    ///
    /// Call this once the kernel is set up (see [`Psx::run_to_shell`]) so
    /// the program can use BIOS functions; it then runs instead of the shell.
    pub fn sideload(&mut self, exe: &Exe) {
        self.bus.write_ram(mask_region(exe.dest), &exe.data);
        // "The memfill region is zerofilled".
        if exe.bss_size != 0 {
            let zeros = vec![0; exe.bss_size as usize];
            self.bus.write_ram(mask_region(exe.bss_start), &zeros);
        }
        self.cpu.set_pc(exe.pc);
        self.cpu.set_reg(REG_GP, exe.gp);
        // "When SP is 0, the unmodified caller's stack is used."
        if let Some(sp) = exe.sp {
            self.cpu.set_reg(REG_SP, sp);
            self.cpu.set_reg(REG_FP, sp);
        }
        // "two parameter values are passed to the executable (in R4 and R5
        // registers) (however, usually that values are simply R4=1 and R5=0)".
        self.cpu.set_reg(REG_A0, 1);
        self.cpu.set_reg(REG_A1, 0);
    }

    /// Takes the TTY bytes printed since the last call.
    pub fn take_tty(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.tty)
    }

    /// Captures characters passed to the kernel's `putchar`.
    ///
    /// This is a high-level hook, not hardware: on a retail console the
    /// kernel would send the character to a (usually absent) DUART, see
    /// psx-spx "BIOS TTY Console (std_io)". Watching the function vectors
    /// works for any BIOS and any program that calls putchar/printf through
    /// them, and does not change what the CPU executes. The character is
    /// passed in R4 (psx-spx: "Parameters, Registers, Stack") and is recorded
    /// raw, without the kernel's TAB/LF expansion.
    fn hook_putchar(&mut self) {
        let function = self.cpu.reg(REG_T1);
        let is_putchar = match mask_region(self.cpu.pc()) {
            A_VECTOR => function == A_PUTCHAR,
            B_VECTOR => function == B_PUTCHAR,
            _ => false,
        };
        if is_putchar {
            self.tty.push(self.cpu.reg(REG_A0) as u8);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map;

    /// Console whose "kernel" is just `jr ra; nop` at both vectors.
    fn psx() -> Psx {
        let mut psx = Psx::new(vec![0; map::BIOS_SIZE]).unwrap();
        let ret: Vec<u8> = [0x03E0_0008u32, 0].iter().flat_map(|w| w.to_le_bytes()).collect();
        psx.bus.write_ram(A_VECTOR, &ret);
        psx.bus.write_ram(B_VECTOR, &ret);
        psx
    }

    fn call(psx: &mut Psx, vector: u32, function: u32, arg: u32) {
        psx.cpu.set_pc(vector);
        psx.cpu.set_reg(REG_T1, function);
        psx.cpu.set_reg(REG_A0, arg);
        psx.cpu.set_reg(31, 0x8001_0000);
        psx.step();
        psx.step();
    }

    #[test]
    fn putchar_through_both_vectors_is_captured() {
        let mut psx = psx();
        call(&mut psx, 0xB0, 0x3D, b'H' as u32);
        call(&mut psx, 0xA0, 0x3C, b'i' as u32);
        call(&mut psx, 0x8000_00B0, 0x3D, b'!' as u32); // via KSEG0
        assert_eq!(psx.take_tty(), b"Hi!");
        assert!(psx.take_tty().is_empty(), "buffer drained");
    }

    #[test]
    fn other_functions_are_not_captured() {
        let mut psx = psx();
        call(&mut psx, 0xB0, 0x3C, b'x' as u32); // B(3Ch) is getchar
        call(&mut psx, 0xA0, 0x3D, b'x' as u32); // A(3Dh) is gets
        call(&mut psx, 0xC0, 0x3D, b'x' as u32);
        assert!(psx.take_tty().is_empty());
    }

    /// BIOS image made of the given instructions at the reset vector.
    fn bios(code: &[u32]) -> Vec<u8> {
        let mut bios = vec![0; map::BIOS_SIZE];
        for (i, word) in code.iter().enumerate() {
            bios[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        bios
    }

    #[test]
    fn run_to_shell_stops_at_the_shell_entry() {
        // lui t0, 0x8003 ; jr t0 ; nop
        let mut psx = Psx::new(bios(&[0x3C08_8003, 0x0100_0008, 0])).unwrap();
        assert!(psx.run_to_shell(100));
        assert_eq!(psx.cpu.pc(), SHELL_ENTRY);
        assert_eq!(psx.cycles(), 3);

        // A BIOS that never gets there: j self.
        let mut psx = Psx::new(bios(&[0x0BF0_0000, 0])).unwrap();
        assert!(!psx.run_to_shell(100));
    }

    #[test]
    fn sideload_places_program_and_registers() {
        use crate::exe::tests::build;
        // addiu v0, zero, 0x42 at 0x80010004 (entry is the second word).
        let file = build(0x8001_0004, 0x8001_0000, 0x801F_FF00, &[0xDEAD_BEEF, 0x2402_0042]);
        let mut exe = Exe::parse(&file).unwrap();
        exe.bss_start = 0x8002_0000;
        exe.bss_size = 8;

        let mut psx = Psx::new(vec![0; map::BIOS_SIZE]).unwrap();
        psx.bus.store32(0x8002_0004, 0xFFFF_FFFF).unwrap();
        psx.sideload(&exe);
        assert_eq!(psx.bus.load32(0x8001_0000).unwrap(), 0xDEAD_BEEF);
        assert_eq!(psx.bus.load32(0x8002_0004).unwrap(), 0, "bss cleared");
        assert_eq!(psx.cpu.reg(REG_GP), 0x1234);
        assert_eq!(psx.cpu.reg(REG_SP), 0x801F_FF10);
        assert_eq!(psx.cpu.reg(REG_FP), 0x801F_FF10);
        assert_eq!(psx.cpu.reg(REG_A0), 1);
        psx.step();
        assert_eq!(psx.cpu.reg(2), 0x42);
    }

    #[test]
    fn interrupt_controller_reaches_the_cpu() {
        // Timer 2 IRQ, unmasked in I_MASK and SR, vectors to BFC00180h
        // (BEV is set after reset).
        let mut psx = Psx::new(vec![0; map::BIOS_SIZE]).unwrap();
        psx.bus.store32(0x1F80_1074, 1 << 6).unwrap();
        psx.bus.store32(0x1F80_1128, 10).unwrap();
        psx.bus.store32(0x1F80_1124, 0x58).unwrap();
        let sr = psx.cpu.sr();
        // mtc0 needs code; poke SR through a tiny program instead:
        // lui t0, 0x0040 ; ori t0, t0, 0x0401 ; mtc0 t0, $12
        let code: Vec<u8> = [0x3C08_0040u32, 0x3508_0401, 0x4088_6000]
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect();
        psx.bus.write_ram(0x1_0000, &code);
        psx.cpu.set_pc(0x8001_0000);
        assert_eq!(sr & 1, 0);
        for _ in 0..20 {
            psx.step();
            if psx.cpu.pc() == 0xBFC0_0180 {
                break;
            }
        }
        assert_eq!(psx.cpu.pc(), 0xBFC0_0180, "interrupt taken");
        assert_eq!((psx.cpu.cause() >> 2) & 0x1F, 0, "ExcCode INT");
    }
}
