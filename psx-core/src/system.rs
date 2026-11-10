//! The whole console: CPU + bus, plus kernel-level hooks.
//!
//! The core does no I/O (see CLAUDE.md), so TTY output is collected here as
//! bytes and the frontend/harness decides what to do with it.

use crate::bus::{BiosError, Bus};
use crate::cpu::Cpu;
use crate::map::mask_region;

// psx-spx: "Kernel Memory" — function vectors in the first page of RAM, and
// "BIOS Function Summary": "A-Functions (Call 00A0h with function number in
// R9 Register)", likewise 00B0h for B-functions.
const A_VECTOR: u32 = 0x0000_00A0;
const B_VECTOR: u32 = 0x0000_00B0;
/// psx-spx: "A(3Ch) or B(3Dh) - putchar(char) - Write character to TTY".
const A_PUTCHAR: u32 = 0x3C;
const B_PUTCHAR: u32 = 0x3D;

const REG_A0: usize = 4;
const REG_T1: usize = 9;

pub struct Psx {
    pub cpu: Cpu,
    pub bus: Bus,
    tty: Vec<u8>,
}

impl Psx {
    /// Creates a console in its reset state with the given BIOS image.
    pub fn new(bios: Vec<u8>) -> Result<Self, BiosError> {
        Ok(Self { cpu: Cpu::new(), bus: Bus::new(bios)?, tty: Vec::new() })
    }

    /// Executes one CPU instruction.
    pub fn step(&mut self) {
        self.hook_putchar();
        self.cpu.step(&mut self.bus);
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
}
