//! Instruction disassembler for traces. Pure function, no state.
//!
//! psx-spx: "CPU Opcode Encoding". Mnemonics and operand order follow the
//! conventional MIPS assembler syntax rather than the nocash syntax used in
//! parts of psx-spx.

/// psx-spx: "CPU Registers" — conventional register names.
const REG: [&str; 32] = [
    "zero", "at", "v0", "v1", "a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3", "t4", "t5", "t6",
    "t7", "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "t8", "t9", "k0", "k1", "gp", "sp", "fp",
    "ra",
];

/// Disassembles the instruction `word` located at address `pc`.
pub fn disassemble(pc: u32, word: u32) -> String {
    let op = word >> 26;
    let rs = REG[((word >> 21) & 0x1F) as usize];
    let rt = REG[((word >> 16) & 0x1F) as usize];
    let rd = REG[((word >> 11) & 0x1F) as usize];
    let shamt = (word >> 6) & 0x1F;
    let imm = word & 0xFFFF;
    let simm = imm as u16 as i16;
    let branch = pc.wrapping_add(4).wrapping_add((simm as i32 as u32) << 2);
    let jump = (pc.wrapping_add(4) & 0xF000_0000) | ((word & 0x03FF_FFFF) << 2);
    let simm = signed_hex(simm);
    let mem = || format!("{simm}({rs})");

    if word == 0 {
        return "nop".into();
    }
    match op {
        0x00 => match word & 0x3F {
            0x00 => format!("sll {rd}, {rt}, {shamt}"),
            0x02 => format!("srl {rd}, {rt}, {shamt}"),
            0x03 => format!("sra {rd}, {rt}, {shamt}"),
            0x04 => format!("sllv {rd}, {rt}, {rs}"),
            0x06 => format!("srlv {rd}, {rt}, {rs}"),
            0x07 => format!("srav {rd}, {rt}, {rs}"),
            0x08 => format!("jr {rs}"),
            0x09 => format!("jalr {rd}, {rs}"),
            0x0C => format!("syscall {:#x}", (word >> 6) & 0xF_FFFF),
            0x0D => format!("break {:#x}", (word >> 6) & 0xF_FFFF),
            0x10 => format!("mfhi {rd}"),
            0x11 => format!("mthi {rs}"),
            0x12 => format!("mflo {rd}"),
            0x13 => format!("mtlo {rs}"),
            0x18 => format!("mult {rs}, {rt}"),
            0x19 => format!("multu {rs}, {rt}"),
            0x1A => format!("div {rs}, {rt}"),
            0x1B => format!("divu {rs}, {rt}"),
            0x20 => format!("add {rd}, {rs}, {rt}"),
            0x21 => format!("addu {rd}, {rs}, {rt}"),
            0x22 => format!("sub {rd}, {rs}, {rt}"),
            0x23 => format!("subu {rd}, {rs}, {rt}"),
            0x24 => format!("and {rd}, {rs}, {rt}"),
            0x25 => format!("or {rd}, {rs}, {rt}"),
            0x26 => format!("xor {rd}, {rs}, {rt}"),
            0x27 => format!("nor {rd}, {rs}, {rt}"),
            0x2A => format!("slt {rd}, {rs}, {rt}"),
            0x2B => format!("sltu {rd}, {rs}, {rt}"),
            _ => format!("illegal {word:#010x}"),
        },
        0x01 => {
            let name = match (word >> 16) & 0x1F {
                0x00 => "bltz",
                0x01 => "bgez",
                0x10 => "bltzal",
                0x11 => "bgezal",
                // Undocumented encodings, see the BcondZ note in cpu.rs.
                n if n & 1 == 0 => "bltz?",
                _ => "bgez?",
            };
            format!("{name} {rs}, {branch:#010x}")
        }
        0x02 => format!("j {jump:#010x}"),
        0x03 => format!("jal {jump:#010x}"),
        0x04 => format!("beq {rs}, {rt}, {branch:#010x}"),
        0x05 => format!("bne {rs}, {rt}, {branch:#010x}"),
        0x06 => format!("blez {rs}, {branch:#010x}"),
        0x07 => format!("bgtz {rs}, {branch:#010x}"),
        0x08 => format!("addi {rt}, {rs}, {simm}"),
        0x09 => format!("addiu {rt}, {rs}, {simm}"),
        0x0A => format!("slti {rt}, {rs}, {simm}"),
        0x0B => format!("sltiu {rt}, {rs}, {simm}"),
        0x0C => format!("andi {rt}, {rs}, {imm:#x}"),
        0x0D => format!("ori {rt}, {rs}, {imm:#x}"),
        0x0E => format!("xori {rt}, {rs}, {imm:#x}"),
        0x0F => format!("lui {rt}, {imm:#x}"),
        0x10..=0x13 => {
            let n = op & 3;
            let reg = (word >> 11) & 0x1F;
            match (word >> 21) & 0x1F {
                0x00 => format!("mfc{n} {rt}, ${reg}"),
                0x02 => format!("cfc{n} {rt}, ${reg}"),
                0x04 => format!("mtc{n} {rt}, ${reg}"),
                0x06 => format!("ctc{n} {rt}, ${reg}"),
                0x08 if word & (1 << 16) == 0 => format!("bc{n}f {branch:#010x}"),
                0x08 => format!("bc{n}t {branch:#010x}"),
                0x10..=0x1F if n == 0 && word & 0x3F == 0x10 => "rfe".into(),
                0x10..=0x1F => format!("cop{n} {:#x}", word & 0x01FF_FFFF),
                _ => format!("illegal {word:#010x}"),
            }
        }
        0x20 => format!("lb {rt}, {}", mem()),
        0x21 => format!("lh {rt}, {}", mem()),
        0x22 => format!("lwl {rt}, {}", mem()),
        0x23 => format!("lw {rt}, {}", mem()),
        0x24 => format!("lbu {rt}, {}", mem()),
        0x25 => format!("lhu {rt}, {}", mem()),
        0x26 => format!("lwr {rt}, {}", mem()),
        0x28 => format!("sb {rt}, {}", mem()),
        0x29 => format!("sh {rt}, {}", mem()),
        0x2A => format!("swl {rt}, {}", mem()),
        0x2B => format!("sw {rt}, {}", mem()),
        0x2E => format!("swr {rt}, {}", mem()),
        0x30..=0x33 => format!("lwc{} ${}, {}", op & 3, (word >> 16) & 0x1F, mem()),
        0x38..=0x3B => format!("swc{} ${}, {}", op & 3, (word >> 16) & 0x1F, mem()),
        _ => format!("illegal {word:#010x}"),
    }
}

/// Formats a signed immediate as `0x18` / `-0x18`.
fn signed_hex(value: i16) -> String {
    if value < 0 {
        format!("-{:#x}", value.unsigned_abs())
    } else {
        format!("{value:#x}")
    }
}

#[cfg(test)]
mod tests {
    use super::disassemble;

    #[test]
    fn common_instructions() {
        let d = |w| disassemble(0xBFC0_0000, w);
        assert_eq!(d(0x0000_0000), "nop");
        assert_eq!(d(0x3C08_0013), "lui t0, 0x13");
        assert_eq!(d(0x3508_243F), "ori t0, t0, 0x243f");
        assert_eq!(d(0x27BD_FFE8), "addiu sp, sp, -0x18");
        assert_eq!(d(0xAFBF_0014), "sw ra, 0x14(sp)");
        assert_eq!(d(0x8C22_FFFC), "lw v0, -0x4(at)");
        assert_eq!(d(0x03E0_0008), "jr ra");
        assert_eq!(d(0x0100_F809), "jalr ra, t0");
        assert_eq!(d(0x0000_000C), "syscall 0x0");
        assert_eq!(d(0x4080_6000), "mtc0 zero, $12");
        assert_eq!(d(0x4200_0010), "rfe");
        assert_eq!(d(0x4A18_0001), "cop2 0x180001");
        assert_eq!(d(0xFC00_0000), "illegal 0xfc000000");
    }

    #[test]
    fn branch_and_jump_targets_are_absolute() {
        // Branches are relative to the delay slot.
        assert_eq!(disassemble(0x8001_0000, 0x1000_FFFF), "beq zero, zero, 0x80010000");
        assert_eq!(disassemble(0x8001_0000, 0x1420_0002), "bne at, zero, 0x8001000c");
        assert_eq!(disassemble(0xBFC0_0000, 0x0BF0_0054), "j 0xbfc00150");
        assert_eq!(disassemble(0x8001_0000, 0x0411_0001), "bgezal zero, 0x80010008");
    }
}
