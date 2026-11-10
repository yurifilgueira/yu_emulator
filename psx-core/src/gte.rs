//! Geometry Transformation Engine (coprocessor 2).
//!
//! psx-spx: "Geometry Transformation Engine (GTE)".
//!
//! Fixed-point vector and matrix arithmetic with saturation flags. psx-spx
//! gives the formulas; a few bit-level details that decide flags and
//! results in overflow cases are not spelled out there (when the 44-bit
//! overflow of MAC1-3 is checked inside a sum, how the bugged far-colour
//! MVMVA sets flags). Those follow what other emulators do, from memory,
//! and are marked where they occur; ps1-tests `gte/test-all` and the
//! `gte-fuzz` console log are the judge.
//!
//! Timing is not modelled: psx-spx lists 5-44 cycles per command, here
//! every command takes the single cycle of its instruction.

/// psx-spx: "cop2r63 (cnt31) - FLAG".
mod flag {
    /// "MAC1/2/3 Result larger than 43 bits and positive": bits 30, 29, 28.
    pub const MAC_POSITIVE: [u32; 4] = [1 << 16, 1 << 30, 1 << 29, 1 << 28];
    /// "... and negative": bits 27, 26, 25 (MAC0: bit 15, "larger than 31
    /// bits").
    pub const MAC_NEGATIVE: [u32; 4] = [1 << 15, 1 << 27, 1 << 26, 1 << 25];
    /// "IR0 saturated" (bit 12), "IR1/2/3 saturated" (bits 24, 23, 22).
    pub const IR: [u32; 4] = [1 << 12, 1 << 24, 1 << 23, 1 << 22];
    /// "Color-FIFO-R/G/B saturated": bits 21, 20, 19.
    pub const COLOR: [u32; 3] = [1 << 21, 1 << 20, 1 << 19];
    /// "SZ3 or OTZ saturated to +0000h..+FFFFh".
    pub const SZ_OTZ: u32 = 1 << 18;
    /// "Divide overflow. RTPS/RTPT division result saturated to max=1FFFFh".
    pub const DIVIDE: u32 = 1 << 17;
    pub const SX: u32 = 1 << 14;
    pub const SY: u32 = 1 << 13;
    /// "Error Flag (Bit30..23, and 18..13 ORed together)".
    pub const ERROR_SOURCES: u32 = 0x7F87_E000;
    /// "Bit30-12 are read/write-able".
    pub const WRITABLE: u32 = 0x7FFF_F000;
}

/// psx-spx: "GTE Division Inaccuracy" — "unr_table[i] = max(0,
/// (40000h/(i+100h)+1)/2-101h)".
const UNR_TABLE: [u8; 0x101] = {
    let mut table = [0u8; 0x101];
    let mut i = 0;
    while i < 0x101 {
        let value = (0x40000 / (i as i32 + 0x100) + 1) / 2 - 0x101;
        table[i] = if value < 0 { 0 } else { value as u8 };
        i += 1;
    }
    table
};

type Matrix = [[i16; 3]; 3];

#[derive(Debug, Clone, Default)]
pub struct Gte {
    // ---- data registers (cop2r0-31) ----
    /// V0, V1, V2.
    v: [[i16; 3]; 3],
    /// RGBC: R, G, B, CODE as the bytes of a word.
    rgbc: u32,
    otz: u16,
    /// IR0..IR3.
    ir: [i16; 4],
    /// Screen XY FIFO: SXY0, SXY1, SXY2.
    sxy: [[i16; 2]; 3],
    /// Screen Z FIFO: SZ0..SZ3.
    sz: [u16; 4],
    /// Colour FIFO: RGB0, RGB1, RGB2.
    rgb: [u32; 3],
    res1: u32,
    /// MAC0..MAC3.
    mac: [i32; 4],
    lzcs: u32,

    // ---- control registers (cop2r32-63) ----
    rt: Matrix,
    tr: [i32; 3],
    llm: Matrix,
    bk: [i32; 3],
    lcm: Matrix,
    fc: [i32; 3],
    ofx: i32,
    ofy: i32,
    h: u16,
    dqa: i16,
    dqb: i32,
    zsf3: i16,
    zsf4: i16,
    flag: u32,

    // ---- fields of the command being executed ----
    /// "sf - Shift Fraction in IR registers": 0 or 12.
    shift: u32,
    /// "lm - Saturate IR1,IR2,IR3 result (0=To -8000h..+7FFFh, 1=To 0..+7FFFh)".
    lm: bool,
}

fn pack(low: i16, high: i16) -> u32 {
    (low as u16 as u32) | ((high as u16 as u32) << 16)
}

impl Gte {
    // ---- register access ---------------------------------------------------

    /// Reads register 0-63 (0-31 data, 32-63 control).
    /// psx-spx: "GTE Data Register Summary", "GTE Control Register Summary".
    pub fn read(&self, reg: usize) -> u32 {
        // "Matrix Registers": two elements per register, and the last one
        // "sign-expanded to 32bit".
        let matrix = |m: &Matrix, n: usize| match n {
            0 => pack(m[0][0], m[0][1]),
            1 => pack(m[0][2], m[1][0]),
            2 => pack(m[1][1], m[1][2]),
            3 => pack(m[2][0], m[2][1]),
            _ => m[2][2] as u32,
        };
        match reg {
            0 | 2 | 4 => pack(self.v[reg / 2][0], self.v[reg / 2][1]),
            // "The IRn and VZn elements occupy a whole 32bit register,
            // reading these registers returns the 16bit value sign-expanded".
            1 | 3 | 5 => self.v[reg / 2][2] as u32,
            6 => self.rgbc,
            7 => self.otz as u32,
            8..=11 => self.ir[reg - 8] as u32,
            12..=14 => pack(self.sxy[reg - 12][0], self.sxy[reg - 12][1]),
            // "SXYP is a mirror of SXY2".
            15 => pack(self.sxy[2][0], self.sxy[2][1]),
            16..=19 => self.sz[reg - 16] as u32,
            20..=22 => self.rgb[reg - 20],
            23 => self.res1,
            24..=27 => self.mac[reg - 24] as u32,
            // "cop2r29 - ORGB": IR / 80h saturated to 0..1Fh; "ORGB is
            // simply a read-only mirror of IRGB".
            28 | 29 => {
                let five = |ir: i16| (ir >> 7).clamp(0, 0x1F) as u32;
                five(self.ir[1]) | (five(self.ir[2]) << 5) | (five(self.ir[3]) << 10)
            }
            30 => self.lzcs,
            // "cop2r31 - LZCR": "leading 0 count of LZCS if LZCS is positive
            // and the leading 1 count of LZCS if LZCS is negative".
            31 => {
                let value = self.lzcs as i32;
                (value ^ (value >> 31)).leading_zeros()
            }
            32..=36 => matrix(&self.rt, reg - 32),
            37..=39 => self.tr[reg - 37] as u32,
            40..=44 => matrix(&self.llm, reg - 40),
            45..=47 => self.bk[reg - 45] as u32,
            48..=52 => matrix(&self.lcm, reg - 48),
            53..=55 => self.fc[reg - 53] as u32,
            56 => self.ofx as u32,
            57 => self.ofy as u32,
            // "BUG: When reading the H register, the hardware does
            // accidently <sign-expand> the <unsigned> 16bit value".
            58 => self.h as i16 as u32,
            59 => self.dqa as u32,
            60 => self.dqb as u32,
            61 => self.zsf3 as u32,
            62 => self.zsf4 as u32,
            _ => self.flag,
        }
    }

    /// Writes register 0-63. psx-spx: "Writing 32bit values to 16bit GTE
    /// registers by software does not trigger any overflow/saturation
    /// flags (and does not do any saturation)".
    pub fn write(&mut self, reg: usize, value: u32) {
        let (low, high) = (value as i16, (value >> 16) as i16);
        let matrix = |m: &mut Matrix, n: usize| match n {
            0 => (m[0][0], m[0][1]) = (low, high),
            1 => (m[0][2], m[1][0]) = (low, high),
            2 => (m[1][1], m[1][2]) = (low, high),
            3 => (m[2][0], m[2][1]) = (low, high),
            _ => m[2][2] = low,
        };
        match reg {
            0 | 2 | 4 => (self.v[reg / 2][0], self.v[reg / 2][1]) = (low, high),
            1 | 3 | 5 => self.v[reg / 2][2] = low,
            6 => self.rgbc = value,
            7 => self.otz = value as u16,
            8..=11 => self.ir[reg - 8] = low,
            12..=14 => self.sxy[reg - 12] = [low, high],
            // "writing to SXYP moves SXY2/SXY1 to SXY1/SXY0".
            15 => self.sxy = [self.sxy[1], self.sxy[2], [low, high]],
            16..=19 => self.sz[reg - 16] = value as u16,
            20..=22 => self.rgb[reg - 20] = value,
            23 => self.res1 = value,
            24..=27 => self.mac[reg - 24] = value as i32,
            // "cop2r28 - IRGB": each 5-bit colour "multiplied by 80h, and
            // written to" IR1, IR2, IR3.
            28 => {
                for k in 0..3 {
                    self.ir[k + 1] = (((value >> (5 * k)) & 0x1F) * 0x80) as i16;
                }
            }
            // ORGB and LZCR are read-only.
            29 | 31 => {}
            30 => self.lzcs = value,
            32..=36 => matrix(&mut self.rt, reg - 32),
            37..=39 => self.tr[reg - 37] = value as i32,
            40..=44 => matrix(&mut self.llm, reg - 40),
            45..=47 => self.bk[reg - 45] = value as i32,
            48..=52 => matrix(&mut self.lcm, reg - 48),
            53..=55 => self.fc[reg - 53] = value as i32,
            56 => self.ofx = value as i32,
            57 => self.ofy = value as i32,
            58 => self.h = value as u16,
            59 => self.dqa = low,
            60 => self.dqb = value as i32,
            61 => self.zsf3 = low,
            62 => self.zsf4 = low,
            _ => {
                self.flag = value & flag::WRITABLE;
                self.update_error_flag();
            }
        }
    }

    /// "31 Error Flag (Bit30..23, and 18..13 ORed together) (Read only)".
    fn update_error_flag(&mut self) {
        if self.flag & flag::ERROR_SOURCES != 0 {
            self.flag |= 1 << 31;
        }
    }

    // ---- result registers and their flags ----------------------------------

    /// Checks a MAC1-3 intermediate against 44 bits and wraps it to 44 bits.
    /// psx-spx: FLAG bits 30-25, "larger than 43 bits".
    fn check_mac(&mut self, i: usize, value: i64) -> i64 {
        if value >= 1 << 43 {
            self.flag |= flag::MAC_POSITIVE[i];
        }
        if value < -(1 << 43) {
            self.flag |= flag::MAC_NEGATIVE[i];
        }
        (value << 20) >> 20
    }

    /// MACi = value SAR (sf*12).
    fn set_mac(&mut self, i: usize, value: i64) {
        self.check_mac(i, value);
        self.mac[i] = (value >> self.shift) as i32;
    }

    /// IRi = value saturated "to -8000h..+7FFFh" or, with lm, "to 0..+7FFFh".
    fn set_ir(&mut self, i: usize, value: i32, lm: bool) {
        let min = if lm { 0 } else { -0x8000 };
        let clamped = value.clamp(min, 0x7FFF);
        if clamped != value {
            self.flag |= flag::IR[i];
        }
        self.ir[i] = clamped as i16;
    }

    /// The common "[IRi] = [MACi] = value SAR (sf*12)".
    fn set_mac_ir(&mut self, i: usize, value: i64, lm: bool) {
        self.set_mac(i, value);
        self.set_ir(i, self.mac[i], lm);
    }

    /// MAC0 = value, flagged when "larger than 31 bits".
    fn set_mac0(&mut self, value: i64) {
        if value > i32::MAX as i64 {
            self.flag |= flag::MAC_POSITIVE[0];
        }
        if value < i32::MIN as i64 {
            self.flag |= flag::MAC_NEGATIVE[0];
        }
        self.mac[0] = value as i32;
    }

    /// "IR0 saturated to +0000h..+1000h".
    fn set_ir0(&mut self, value: i64) {
        let clamped = value.clamp(0, 0x1000);
        if clamped != value {
            self.flag |= flag::IR[0];
        }
        self.ir[0] = clamped as i16;
    }

    /// Value "saturated to +0000h..+FFFFh" for SZ3 and OTZ.
    fn saturate_z(&mut self, value: i64) -> u16 {
        let clamped = value.clamp(0, 0xFFFF);
        if clamped != value {
            self.flag |= flag::SZ_OTZ;
        }
        clamped as u16
    }

    /// Pushes onto the screen XY FIFO, "saturated to -0400h..+03FFh".
    fn push_sxy(&mut self, x: i64, y: i64) {
        let cx = x.clamp(-0x400, 0x3FF);
        let cy = y.clamp(-0x400, 0x3FF);
        if cx != x {
            self.flag |= flag::SX;
        }
        if cy != y {
            self.flag |= flag::SY;
        }
        self.sxy = [self.sxy[1], self.sxy[2], [cx as i16, cy as i16]];
    }

    /// psx-spx: "Color FIFO = [MAC1/16,MAC2/16,MAC3/16,CODE]", each
    /// "saturated to +00h..+FFh", older entries "moved downwards".
    fn push_color(&mut self) {
        let mut color = self.rgbc & 0xFF00_0000;
        for k in 0..3 {
            let value = self.mac[k + 1] >> 4;
            let clamped = value.clamp(0, 0xFF);
            if clamped != value {
                self.flag |= flag::COLOR[k];
            }
            color |= (clamped as u32) << (8 * k);
        }
        self.rgb = [self.rgb[1], self.rgb[2], color];
    }

    // ---- building blocks ---------------------------------------------------

    /// "[IR1,IR2,IR3] = [MAC1,MAC2,MAC3] = (T*1000h + M*V) SAR (sf*12)".
    ///
    /// Not in psx-spx: the 44-bit overflow is checked after each of the
    /// three additions, and the running sum wraps to 44 bits in between.
    fn multiply(&mut self, m: &Matrix, v: [i16; 3], t: [i32; 3]) {
        for i in 0..3 {
            let mut sum = (t[i] as i64) << 12;
            for k in 0..3 {
                if k > 0 {
                    sum = self.check_mac(i + 1, sum);
                }
                sum += m[i][k] as i64 * v[k] as i64;
            }
            self.set_mac_ir(i + 1, sum, self.lm);
        }
    }

    /// psx-spx: 'Details on "MAC+(FC-MAC)*IR0"':
    /// IR = ((FC SHL 12) - MAC) SAR (sf*12), "saturated to -8000h..+7FFFh
    /// (ie. as if lm=0)", then MAC = (IR * IR0 + MAC) SAR (sf*12).
    fn interpolate(&mut self, input: [i64; 3]) {
        for (i, value) in input.iter().enumerate() {
            self.set_mac_ir(i + 1, ((self.fc[i] as i64) << 12) - value, false);
        }
        for (i, value) in input.iter().enumerate() {
            let product = self.ir[i + 1] as i64 * self.ir[0] as i64;
            self.set_mac_ir(i + 1, product + value, self.lm);
        }
    }

    /// "[MAC1,MAC2,MAC3] = [R*IR1,G*IR2,B*IR3] SHL 4".
    fn color_times_ir(&self) -> [i64; 3] {
        [0, 1, 2].map(|k| (((self.rgbc >> (8 * k)) & 0xFF) as i64 * self.ir[k + 1] as i64) << 4)
    }

    fn ir_vector(&self) -> [i16; 3] {
        [self.ir[1], self.ir[2], self.ir[3]]
    }

    /// psx-spx: "GTE Division Inaccuracy" — the Unsigned Newton-Raphson
    /// division used by RTPS/RTPT.
    fn divide(&mut self, h: u16, sz3: u16) -> u32 {
        if (h as u32) < sz3 as u32 * 2 {
            let z = sz3.leading_zeros();
            let n = (h as u64) << z;
            let d = (sz3 as u64) << z;
            let u = UNR_TABLE[((d - 0x7FC0) >> 7) as usize] as u64 + 0x101;
            let d = (0x200_0080 - d * u) >> 8;
            let d = (0x80 + d * u) >> 8;
            (((n * d) + 0x8000) >> 16).min(0x1_FFFF) as u32
        } else {
            self.flag |= flag::DIVIDE;
            0x1_FFFF
        }
    }

    // ---- commands ----------------------------------------------------------

    /// Executes a "COP2 imm25" command.
    /// psx-spx: "GTE Command Encoding", "GTE Opcode Summary".
    pub fn execute(&mut self, command: u32) {
        self.shift = if command & (1 << 19) != 0 { 12 } else { 0 };
        self.lm = command & (1 << 10) != 0;
        // "all bits are automatically reset at the begin of a new GTE
        // command".
        self.flag = 0;
        match command & 0x3F {
            0x01 => self.rtp(0, true),
            0x06 => self.nclip(),
            0x0C => self.op(),
            0x10 => self.dpcs(self.rgbc),
            0x11 => self.intpl(),
            0x12 => self.mvmva(command),
            0x13 => self.ncd(0),
            0x14 => self.cdp(),
            0x16 => (0..3).for_each(|n| self.ncd(n)),
            0x1B => self.ncc(0),
            0x1C => self.cc(),
            0x1E => self.nc(0),
            0x20 => (0..3).for_each(|n| self.nc(n)),
            0x28 => self.sqr(),
            0x29 => self.dcpl(),
            // "DPCT executes thrice, and reads the R,G,B values from RGB0".
            0x2A => (0..3).for_each(|_| self.dpcs(self.rgb[0])),
            0x2D => self.avsz(self.zsf3, 1),
            0x2E => self.avsz(self.zsf4, 0),
            0x30 => (0..3).for_each(|n| self.rtp(n, n == 2)),
            0x3D => self.gpf(),
            0x3E => self.gpl(),
            0x3F => (0..3).for_each(|n| self.ncc(n)),
            // "Unknown if/what happens when using the N/A opcodes?" —
            // nothing here, apart from the flags having been reset.
            _ => {}
        }
        self.update_error_flag();
    }

    /// RTPS / one vertex of RTPT. psx-spx: "Perspective Transformation".
    fn rtp(&mut self, vertex: usize, last: bool) {
        let v = self.v[vertex];
        let mut sums = [0i64; 3];
        for (i, total) in sums.iter_mut().enumerate() {
            let mut sum = (self.tr[i] as i64) << 12;
            for (k, component) in v.iter().enumerate() {
                if k > 0 {
                    sum = self.check_mac(i + 1, sum);
                }
                sum += self.rt[i][k] as i64 * *component as i64;
            }
            self.set_mac(i + 1, sum);
            *total = sum;
        }
        self.set_ir(1, self.mac[1], self.lm);
        self.set_ir(2, self.mac[2], self.lm);
        // "The IR3 saturation flag (FLAG.22) is always checked as if lm=0"
        // on "MAC3 SAR 12", "while the stored IR3 is clamped from MAC3 per
        // the actual lm bit".
        // Not in psx-spx: this value is 32 bits wide, so a sum that
        // overflowed 44 bits can change sign here (ps1-tests gte/test-all
        // expects SZ3 = FFFFh in such a case, not 0).
        let depth = (sums[2] >> 12) as i32;
        self.set_ir(3, depth, false);
        let min = if self.lm { 0 } else { -0x8000 };
        self.ir[3] = self.mac[3].clamp(min, 0x7FFF) as i16;

        // "SZ3 = MAC3 SAR ((1-sf)*12)", i.e. always the sum SAR 12.
        let sz3 = self.saturate_z(depth as i64);
        self.sz = [self.sz[1], self.sz[2], self.sz[3], sz3];

        let quotient = self.divide(self.h, sz3) as i64;
        // "MAC0=(((H*20000h/SZ3)+1)/2)*IR1+OFX, SX2=MAC0/10000h".
        let x = quotient * self.ir[1] as i64 + self.ofx as i64;
        self.set_mac0(x);
        let y = quotient * self.ir[2] as i64 + self.ofy as i64;
        self.set_mac0(y);
        self.push_sxy(x >> 16, y >> 16);
        // "MAC0=(((H*20000h/SZ3)+1)/2)*DQA+DQB, IR0=MAC0/1000h". Not in
        // psx-spx: for RTPT this is done for the last vertex only.
        if last {
            let depth_cue = quotient * self.dqa as i64 + self.dqb as i64;
            self.set_mac0(depth_cue);
            self.set_ir0(depth_cue >> 12);
        }
    }

    /// psx-spx: "NCLIP - Normal clipping".
    fn nclip(&mut self) {
        let [[x0, y0], [x1, y1], [x2, y2]] = self.sxy.map(|p| [p[0] as i64, p[1] as i64]);
        self.set_mac0(x0 * y1 + x1 * y2 + x2 * y0 - x0 * y2 - x1 * y0 - x2 * y1);
    }

    /// psx-spx: "AVSZ3" / "AVSZ4": MAC0 = ZSF * sum, OTZ = MAC0/1000h.
    fn avsz(&mut self, factor: i16, first: usize) {
        let sum: i64 = self.sz[first..].iter().map(|z| *z as i64).sum();
        let product = factor as i64 * sum;
        self.set_mac0(product);
        self.otz = self.saturate_z(product >> 12);
    }

    /// psx-spx: "MVMVA(sf,mx,v,cv,lm)".
    fn mvmva(&mut self, command: u32) {
        let matrix = match (command >> 17) & 3 {
            0 => self.rt,
            1 => self.llm,
            2 => self.lcm,
            // "Setting Mx=3 selects a garbage matrix (with elements -R*10h,
            // +R*10h, IR0, RT13, RT13, RT13, RT22, RT22, RT22)".
            _ => {
                let r = ((self.rgbc & 0xFF) << 4) as i16;
                [[-r, r, self.ir[0]], [self.rt[0][2]; 3], [self.rt[1][1]; 3]]
            }
        };
        let vector = match (command >> 15) & 3 {
            n @ 0..=2 => self.v[n as usize],
            _ => self.ir_vector(),
        };
        match (command >> 13) & 3 {
            0 => self.multiply(&matrix, vector, self.tr),
            1 => self.multiply(&matrix, vector, self.bk),
            3 => self.multiply(&matrix, vector, [0; 3]),
            // Far colour: "not added correctly by the hardware: The return
            // values are reduced to the last two portions of the formula
            // ... nevertheless, some bits in the FLAG register seem to be
            // adjusted as if the full operation would have been executed".
            // Not in psx-spx (which flags, exactly): the first portion
            // (FC*1000h + Mx1*Vx1) sets the MAC overflow and IR saturation
            // flags as if lm=0, then the result is the other two portions.
            _ => {
                for (i, row) in matrix.iter().enumerate() {
                    let first = ((self.fc[i] as i64) << 12) + row[0] as i64 * vector[0] as i64;
                    let first = self.check_mac(i + 1, first);
                    self.set_ir(i + 1, (first >> self.shift) as i32, false);
                    let rest = self.check_mac(i + 1, row[1] as i64 * vector[1] as i64);
                    self.set_mac_ir(i + 1, rest + row[2] as i64 * vector[2] as i64, self.lm);
                }
            }
        }
    }

    /// psx-spx: "SQR(sf) - Square vector".
    fn sqr(&mut self) {
        for i in 1..=3 {
            self.set_mac_ir(i, self.ir[i] as i64 * self.ir[i] as i64, self.lm);
        }
    }

    /// psx-spx: "OP(sf,lm) - Cross product of 2 vectors", with "D1,D2,D3
    /// ... the RT11,RT22,RT33 elements of the RT matrix".
    fn op(&mut self) {
        let [ir1, ir2, ir3] = self.ir_vector().map(i64::from);
        let [d1, d2, d3] = [self.rt[0][0], self.rt[1][1], self.rt[2][2]].map(i64::from);
        self.set_mac_ir(1, ir3 * d2 - ir2 * d3, self.lm);
        self.set_mac_ir(2, ir1 * d3 - ir3 * d1, self.lm);
        self.set_mac_ir(3, ir2 * d1 - ir1 * d2, self.lm);
    }

    /// The light steps shared by the "Normal color" commands:
    /// "(LLM*V0) SAR (sf*12)" then "(BK*1000h + LCM*IR) SAR (sf*12)".
    fn light(&mut self, vertex: usize) {
        self.multiply(&self.llm.clone(), self.v[vertex], [0; 3]);
        self.light_color();
    }

    fn light_color(&mut self) {
        self.multiply(&self.lcm.clone(), self.ir_vector(), self.bk);
    }

    /// NCS / NCT: "Normal color".
    fn nc(&mut self, vertex: usize) {
        self.light(vertex);
        self.push_color();
    }

    /// NCCS / NCCT: "Normal Color Color".
    fn ncc(&mut self, vertex: usize) {
        self.light(vertex);
        self.color_step();
    }

    /// NCDS / NCDT: "Normal color depth cue".
    fn ncd(&mut self, vertex: usize) {
        self.light(vertex);
        self.depth_cue_color_step();
    }

    /// "CC(lm=1) - Color Color".
    fn cc(&mut self) {
        self.light_color();
        self.color_step();
    }

    /// "CDP(...) - Color Depth Que".
    fn cdp(&mut self) {
        self.light_color();
        self.depth_cue_color_step();
    }

    /// "[MAC] = [R*IR1,G*IR2,B*IR3] SHL 4", "SAR (sf*12)", colour FIFO.
    fn color_step(&mut self) {
        for (i, product) in self.color_times_ir().into_iter().enumerate() {
            self.set_mac_ir(i + 1, product, self.lm);
        }
        self.push_color();
    }

    /// The same with "MAC+(FC-MAC)*IR0" in between.
    fn depth_cue_color_step(&mut self) {
        self.interpolate(self.color_times_ir());
        self.push_color();
    }

    /// "DCPL - Depth Cue Color light".
    fn dcpl(&mut self) {
        self.depth_cue_color_step();
    }

    /// "DPCS - Depth Cueing": "[MAC1,MAC2,MAC3] = [R,G,B] SHL 16".
    fn dpcs(&mut self, color: u32) {
        self.interpolate([0, 1, 2].map(|k| (((color >> (8 * k)) & 0xFF) as i64) << 16));
        self.push_color();
    }

    /// "INTPL": "[MAC1,MAC2,MAC3] = [IR1,IR2,IR3] SHL 12".
    fn intpl(&mut self) {
        self.interpolate(self.ir_vector().map(|ir| (ir as i64) << 12));
        self.push_color();
    }

    /// "GPF(sf,lm) - General purpose Interpolation".
    fn gpf(&mut self) {
        for i in 1..=3 {
            self.set_mac_ir(i, self.ir[i] as i64 * self.ir[0] as i64, self.lm);
        }
        self.push_color();
    }

    /// "GPL(sf,lm) - General Interpolation with base": "44bit overflows
    /// can occur internally when sf=1".
    fn gpl(&mut self) {
        for i in 1..=3 {
            let base = (self.mac[i] as i64) << self.shift;
            self.set_mac_ir(i, self.ir[i] as i64 * self.ir[0] as i64 + base, self.lm);
        }
        self.push_color();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Register numbers.
    const VXY0: usize = 0;
    const VZ0: usize = 1;
    const RGBC: usize = 6;
    const OTZ: usize = 7;
    const IR0: usize = 8;
    const IR1: usize = 9;
    const IR2: usize = 10;
    const IR3: usize = 11;
    const SXY0: usize = 12;
    const SXY2: usize = 14;
    const SXYP: usize = 15;
    const SZ0: usize = 16;
    const SZ3: usize = 19;
    const RGB0: usize = 20;
    const RGB2: usize = 22;
    const MAC0: usize = 24;
    const MAC1: usize = 25;
    const MAC2: usize = 26;
    const MAC3: usize = 27;
    const IRGB: usize = 28;
    const ORGB: usize = 29;
    const LZCS: usize = 30;
    const LZCR: usize = 31;
    const RT: usize = 32;
    const TRX: usize = 37;
    const LLM: usize = 40;
    const RBK: usize = 45;
    const LCM: usize = 48;
    const RFC: usize = 53;
    const OFX: usize = 56;
    const OFY: usize = 57;
    const H: usize = 58;
    const DQA: usize = 59;
    const DQB: usize = 60;
    const ZSF3: usize = 61;
    const ZSF4: usize = 62;
    const FLAG: usize = 63;

    // Commands with their usual sf/lm bits (psx-spx command headings).
    const RTPS: u32 = 0x018_0001;
    const RTPT: u32 = 0x028_0030;
    const NCLIP: u32 = 0x140_0006;
    const AVSZ3: u32 = 0x158_002D;
    const AVSZ4: u32 = 0x168_002E;
    const SQR: u32 = 0x0A0_0428;
    const OP: u32 = 0x170_000C;
    const NCS: u32 = 0x0C8_041E;
    const NCCS: u32 = 0x108_041B;
    const DPCS: u32 = 0x078_0010;
    const DPCT: u32 = 0x0F8_002A;
    const INTPL: u32 = 0x098_0011;
    const GPF: u32 = 0x190_003D;
    const GPL: u32 = 0x1A0_003E;
    const SF: u32 = 1 << 19;
    const LM: u32 = 1 << 10;

    fn xy(x: i16, y: i16) -> u32 {
        pack(x, y)
    }

    /// Writes a matrix whose diagonal is `d` (1000h = 1.0) at `base`.
    fn diagonal(gte: &mut Gte, base: usize, d: [i16; 3]) {
        gte.write(base, d[0] as u16 as u32);
        gte.write(base + 1, 0);
        gte.write(base + 2, d[1] as u16 as u32);
        gte.write(base + 3, 0);
        gte.write(base + 4, d[2] as u16 as u32);
    }

    fn set_ir(gte: &mut Gte, ir: [i32; 4]) {
        for (k, value) in ir.iter().enumerate() {
            gte.write(IR0 + k, *value as u32);
        }
    }

    fn mac(gte: &Gte) -> [i32; 3] {
        [gte.read(MAC1) as i32, gte.read(MAC2) as i32, gte.read(MAC3) as i32]
    }

    fn ir(gte: &Gte) -> [i32; 3] {
        [gte.read(IR1) as i32, gte.read(IR2) as i32, gte.read(IR3) as i32]
    }

    // ---- registers ---------------------------------------------------------

    #[test]
    fn sixteen_bit_registers_sign_extend_on_read() {
        // psx-spx: "writing 12008900h (positive 32bit) to a signed 16bit
        // register sets that register to FFFF8900h".
        let mut gte = Gte::default();
        for reg in [VZ0, IR0, IR1, IR3, RT + 4, LLM + 4, LCM + 4, DQA, ZSF3, ZSF4] {
            gte.write(reg, 0x1200_8900);
            assert_eq!(gte.read(reg), 0xFFFF_8900, "register {reg}");
        }
        // Unsigned 16-bit registers read zero-extended.
        for reg in [OTZ, SZ0, SZ3] {
            gte.write(reg, 0x1200_8900);
            assert_eq!(gte.read(reg), 0x0000_8900, "register {reg}");
        }
        // 32-bit registers keep everything.
        for reg in [VXY0, RGBC, SXY0, RGB0, 23, MAC0, MAC3, LZCS, RT, TRX, RBK, RFC, OFX, DQB] {
            gte.write(reg, 0x1200_8900);
            assert_eq!(gte.read(reg), 0x1200_8900, "register {reg}");
        }
    }

    #[test]
    fn h_register_reads_sign_extended_by_mistake() {
        // psx-spx: "Screen Offset and Distance" — the H read bug.
        let mut gte = Gte::default();
        gte.write(H, 0x0000_8000);
        assert_eq!(gte.read(H), 0xFFFF_8000);
        gte.write(H, 0x0000_7FFF);
        assert_eq!(gte.read(H), 0x0000_7FFF);
    }

    #[test]
    fn matrix_registers_pack_two_elements() {
        // psx-spx: "Matrix Registers".
        let mut gte = Gte::default();
        gte.write(RT, 0x0002_0001); // RT11=1, RT12=2
        gte.write(RT + 1, 0x0004_0003); // RT13=3, RT21=4
        gte.write(RT + 2, 0x0006_0005);
        gte.write(RT + 3, 0x0008_0007);
        gte.write(RT + 4, 0xFFFF_0009); // RT33=9
        assert_eq!(gte.rt, [[1, 2, 3], [4, 5, 6], [7, 8, 9]]);
        assert_eq!(gte.read(RT + 1), 0x0004_0003);
        assert_eq!(gte.read(RT + 4), 9);
    }

    #[test]
    fn sxyp_mirrors_sxy2_and_pushes_on_write() {
        // psx-spx: "Screen XYZ Coordinate FIFOs".
        let mut gte = Gte::default();
        gte.write(SXY0, 0x0001_0001);
        gte.write(SXY0 + 1, 0x0002_0002);
        gte.write(SXY2, 0x0003_0003);
        assert_eq!(gte.read(SXYP), 0x0003_0003);
        gte.write(SXYP, 0x0004_0004);
        assert_eq!([gte.read(SXY0), gte.read(SXY0 + 1), gte.read(SXY2)], [0x0002_0002, 0x0003_0003, 0x0004_0004]);
        // Writing SXY2 does not move anything.
        gte.write(SXY2, 0x0005_0005);
        assert_eq!(gte.read(SXY0), 0x0002_0002);
    }

    #[test]
    fn irgb_and_orgb_convert_colours() {
        // psx-spx: "cop2r28 - IRGB", "cop2r29 - ORGB".
        let mut gte = Gte::default();
        gte.write(IRGB, (0x1F << 10) | (0x10 << 5) | 0x01);
        assert_eq!(ir(&gte), [0x0080, 0x0800, 0x0F80]);
        assert_eq!(gte.read(ORGB), (0x1F << 10) | (0x10 << 5) | 0x01);
        assert_eq!(gte.read(IRGB), gte.read(ORGB));
        // "Negative values ... are saturated to 00h, large positive values
        // ... to 1Fh".
        set_ir(&mut gte, [0, -1, 0x1000, 0x7FFF]);
        assert_eq!(gte.read(ORGB), (0x1F << 10) | (0x1F << 5));
        assert_eq!(gte.read(FLAG), 0, "no flags for this conversion");
        gte.write(ORGB, 0);
        assert_eq!(gte.read(IR3) as i32, 0x7FFF, "ORGB is read-only");
    }

    #[test]
    fn leading_bit_count() {
        // psx-spx: "LZCR": "The results are in range 1..32."
        let mut gte = Gte::default();
        for (value, count) in [(0u32, 32), (1, 31), (0x7FFF_FFFF, 1), (0x8000_0000, 1), (0xFFFF_FFFF, 32), (0xFFF0_0000, 12)] {
            gte.write(LZCS, value);
            assert_eq!(gte.read(LZCR), count, "LZCS = {value:#x}");
        }
        gte.write(LZCR, 5);
        assert_eq!(gte.read(LZCR), 12, "LZCR is read-only");
    }

    #[test]
    fn flag_register_write_mask_and_error_bit() {
        // psx-spx: "Bit30-12 are read/write-able"; bit 31 = OR of bits
        // 30-23 and 18-13.
        let mut gte = Gte::default();
        gte.write(FLAG, 0xFFFF_FFFF);
        assert_eq!(gte.read(FLAG), 0xFFFF_F000);
        gte.write(FLAG, 0x8000_0FFF);
        assert_eq!(gte.read(FLAG), 0);
        gte.write(FLAG, (1 << 22) | (1 << 21) | (1 << 20) | (1 << 19) | (1 << 12));
        assert_eq!(gte.read(FLAG) >> 31, 0, "IR3, colour and IR0 flags do not set bit 31");
        gte.write(FLAG, 1 << 13);
        assert_eq!(gte.read(FLAG) >> 31, 1);
    }

    #[test]
    fn unr_table_matches_the_documented_values() {
        // psx-spx: "GTE Division Inaccuracy" table, first and last rows.
        assert_eq!(&UNR_TABLE[..8], &[0xFF, 0xFD, 0xFB, 0xF9, 0xF7, 0xF5, 0xF3, 0xF1]);
        assert_eq!(&UNR_TABLE[0x40..0x44], &[0x99, 0x97, 0x96, 0x95]);
        assert_eq!(&UNR_TABLE[0xF8..], &[0x03, 0x03, 0x02, 0x02, 0x01, 0x01, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn division_overflow_and_special_cases() {
        let mut gte = Gte::default();
        // "if (H < SZ3*2) ... else n = 1FFFFh, FLAG.Bit17=1".
        assert_eq!(gte.divide(100, 0), 0x1_FFFF);
        assert_ne!(gte.flag & flag::DIVIDE, 0);
        gte.flag = 0;
        assert_eq!(gte.divide(200, 100), 0x1_FFFF);
        assert_ne!(gte.flag & flag::DIVIDE, 0);
        // "FE3Fh/7F20h, F015h/780Bh ... do produce UNR result 20000h, and
        // are saturated to 1FFFFh, but without setting overflow FLAG bits".
        gte.flag = 0;
        assert_eq!(gte.divide(0xFE3F, 0x7F20), 0x1_FFFF);
        assert_eq!(gte.divide(0xF015, 0x780B), 0x1_FFFF);
        assert_eq!(gte.flag, 0);
        // "0000h/0001h and 0001h/0001h".
        assert_eq!(gte.divide(0, 1), 0);
        assert_eq!(gte.divide(1, 1), 0x1_0000);
        // An ordinary quotient is within one unit of H*10000h/SZ3.
        let exact = (0x1234u32 * 0x1_0000 + 0x5678 / 2) / 0x5678;
        assert!(gte.divide(0x1234, 0x5678).abs_diff(exact) <= 1);
    }

    // ---- coordinate commands ----------------------------------------------

    /// GTE set up with an identity rotation matrix.
    fn identity() -> Gte {
        let mut gte = Gte::default();
        diagonal(&mut gte, RT, [0x1000; 3]);
        gte
    }

    #[test]
    fn rtps_rotates_translates_and_projects() {
        // psx-spx: "RTPS". Identity rotation, translation (10, 20, 1000),
        // H = SZ3 so the perspective factor is exactly 1.0 (10000h).
        let mut gte = identity();
        gte.write(TRX, 10);
        gte.write(TRX + 1, 20);
        gte.write(TRX + 2, 1000);
        gte.write(VXY0, xy(100, -50));
        gte.write(VZ0, 24);
        gte.write(H, 1024);
        gte.write(OFX, 160 << 16);
        gte.write(OFY, 120 << 16);
        gte.write(SZ3, 7);
        gte.execute(RTPS);
        assert_eq!(mac(&gte), [110, -30, 1024]);
        assert_eq!(ir(&gte), [110, -30, 1024]);
        assert_eq!(gte.read(SZ3), 1024);
        assert_eq!(gte.read(SZ3 - 1), 7, "the Z FIFO moved down");
        assert_eq!(gte.divide(1024, 1024), 0x1_0000);
        assert_eq!(gte.read(SXY2), xy(160 + 110, 120 - 30));
        assert_eq!(gte.read(FLAG), 0);
    }

    #[test]
    fn rtps_depth_cue() {
        // "MAC0=(n*DQA+DQB), IR0=MAC0/1000h" with n = 10000h.
        let mut gte = identity();
        gte.write(VZ0, 512);
        gte.write(H, 512);
        gte.write(DQA, 0x0100);
        gte.write(DQB, 0x0020_0000);
        gte.execute(RTPS);
        assert_eq!(gte.read(MAC0), 0x1_0000 * 0x100 + 0x0020_0000);
        // 1200h exceeds the "+0000h..+1000h" range of IR0.
        assert_eq!(gte.read(IR0), 0x1000);
        assert_eq!(gte.read(FLAG), flag::IR[0]);

        gte.write(DQB, (-0x0080_0000i32) as u32);
        gte.execute(RTPS);
        assert_eq!(gte.read(MAC0), 0x0080_0000);
        assert_eq!(gte.read(IR0), 0x0800);
        assert_eq!(gte.read(FLAG), 0);
    }

    #[test]
    fn rtps_saturation_flags() {
        let mut gte = identity();
        // Behind the camera: SZ3 saturates to 0, the division overflows,
        // and the screen coordinates saturate.
        gte.write(VXY0, xy(1000, -1000));
        gte.write(VZ0, (-5i32) as u32);
        gte.write(H, 100);
        gte.execute(RTPS);
        assert_eq!(gte.read(SZ3), 0);
        let f = gte.read(FLAG);
        assert_ne!(f & flag::SZ_OTZ, 0);
        assert_ne!(f & flag::DIVIDE, 0);
        assert_ne!(f & flag::SX, 0);
        assert_ne!(f & flag::SY, 0);
        assert_eq!(f >> 31, 1);
        assert_eq!(gte.read(SXY2), xy(0x3FF, -0x400));
        // lm=1 clamps the stored IR3 at 0, but FLAG.22 is checked as if
        // lm=0, so -5 does not set it.
        gte.execute(RTPS | LM);
        assert_eq!(gte.read(IR3), 0);
        assert_eq!(gte.read(FLAG) & flag::IR[3], 0);
        assert_ne!(gte.read(FLAG) & flag::IR[2], 0, "IR2 = -1000 does saturate with lm=1");
    }

    #[test]
    fn rtps_without_fraction_shift() {
        // sf=0: MAC = unshifted sum, but "SZ3 = MAC3 SAR ((1-sf)*12)".
        let mut gte = identity();
        gte.write(VXY0, xy(1, 2));
        gte.write(VZ0, 3);
        gte.write(H, 2);
        gte.execute(RTPS & !SF);
        assert_eq!(mac(&gte), [0x1000, 0x2000, 0x3000]);
        assert_eq!(ir(&gte), [0x1000, 0x2000, 0x3000]);
        assert_eq!(gte.read(SZ3), 3);
    }

    #[test]
    fn mac_overflow_beyond_44_bits_is_flagged() {
        // TRX*1000h is just below 2^43; adding a positive product crosses it.
        let mut gte = Gte::default();
        gte.write(TRX, 0x7FFF_FFFF);
        gte.write(RT, 0x7FFF);
        gte.write(VXY0, xy(0x7FFF, 0));
        gte.write(VZ0, 1);
        gte.write(RT + 4, 0x1000);
        gte.write(H, 1);
        gte.execute(RTPS);
        assert_ne!(gte.read(FLAG) & flag::MAC_POSITIVE[1], 0);
        assert_eq!(gte.read(FLAG) & (flag::MAC_POSITIVE[2] | flag::MAC_NEGATIVE[1]), 0);
        // And the negative direction.
        gte.write(TRX, 0x8000_0000);
        gte.write(VXY0, xy(-0x8000, 0));
        gte.execute(RTPS);
        assert_ne!(gte.read(FLAG) & flag::MAC_NEGATIVE[1], 0);
    }

    #[test]
    fn rtpt_transforms_three_vertices() {
        let mut gte = identity();
        gte.write(H, 256);
        for (n, x) in [10i16, 20, 30].iter().enumerate() {
            gte.write(n * 2, xy(*x, 0));
            gte.write(n * 2 + 1, 256);
        }
        gte.execute(RTPT);
        assert_eq!([gte.read(SXY0), gte.read(SXY0 + 1), gte.read(SXY2)], [xy(10, 0), xy(20, 0), xy(30, 0)]);
        assert_eq!([gte.read(SZ0 + 1), gte.read(SZ0 + 2), gte.read(SZ3)], [256, 256, 256]);
        assert_eq!(gte.read(IR1), 30, "IR holds the last vertex");
    }

    #[test]
    fn nclip_sign_gives_the_winding() {
        // psx-spx: "NCLIP".
        let mut gte = Gte::default();
        gte.write(SXY0, xy(0, 0));
        gte.write(SXY0 + 1, xy(10, 0));
        gte.write(SXY2, xy(0, 10));
        gte.execute(NCLIP);
        assert_eq!(gte.read(MAC0) as i32, 100);
        gte.write(SXY0 + 1, xy(0, 10));
        gte.write(SXY2, xy(10, 0));
        gte.execute(NCLIP);
        assert_eq!(gte.read(MAC0) as i32, -100);
        assert_eq!(gte.read(FLAG), 0);
    }

    #[test]
    fn average_z() {
        // psx-spx: "AVSZ3" / "AVSZ4": ZSF3 = 1/3, ZSF4 = 1/4 in 1.3.12.
        let mut gte = Gte::default();
        for (n, z) in [100u32, 1000, 2000, 3000].iter().enumerate() {
            gte.write(SZ0 + n, *z);
        }
        gte.write(ZSF3, 0x555);
        gte.write(ZSF4, 0x400);
        gte.execute(AVSZ3);
        assert_eq!(gte.read(MAC0), 0x555 * 6000);
        assert_eq!(gte.read(OTZ), (0x555 * 6000) >> 12);
        gte.execute(AVSZ4);
        assert_eq!(gte.read(OTZ), 6100 / 4);
        // "negative values would ... saturate OTZ to zero".
        gte.write(ZSF3, (-0x555i32) as u32);
        gte.execute(AVSZ3);
        assert_eq!(gte.read(OTZ), 0);
        assert_ne!(gte.read(FLAG) & flag::SZ_OTZ, 0);
    }

    // ---- general purpose commands -----------------------------------------

    /// MVMVA command word. psx-spx: "GTE Command Encoding".
    fn mvmva(mx: u32, v: u32, cv: u32, bits: u32) -> u32 {
        0x040_0012 | (mx << 17) | (v << 15) | (cv << 13) | bits
    }

    #[test]
    fn mvmva_selects_matrix_vector_and_translation() {
        let mut gte = Gte::default();
        diagonal(&mut gte, RT, [0x1000, 0x2000, 0x0800]);
        diagonal(&mut gte, LLM, [0x1000; 3]);
        diagonal(&mut gte, LCM, [0x0800; 3]);
        gte.write(VXY0, xy(100, 200));
        gte.write(VZ0, 300);
        gte.write(2, xy(1, 2)); // V1
        gte.write(3, 3);
        gte.write(TRX, 5);
        gte.write(RBK, 7);
        set_ir(&mut gte, [0, 40, 50, 60]);

        gte.execute(mvmva(0, 0, 0, SF)); // RT * V0 + TR
        assert_eq!(mac(&gte), [105, 400, 150]);
        assert_eq!(ir(&gte), [105, 400, 150]);
        gte.execute(mvmva(1, 1, 3, SF)); // LLM * V1, no translation
        assert_eq!(mac(&gte), [1, 2, 3]);
        set_ir(&mut gte, [0, 40, 50, 60]);
        gte.execute(mvmva(2, 3, 1, SF)); // LCM * IR + BK
        assert_eq!(mac(&gte), [27, 25, 30]);
        gte.execute(mvmva(0, 0, 3, 0)); // sf=0
        assert_eq!(mac(&gte), [100 << 12, 200 << 13, 300 << 11]);
        assert_eq!(ir(&gte), [0x7FFF; 3], "saturated");
        assert_eq!(gte.read(FLAG) & (flag::IR[1] | flag::IR[2] | flag::IR[3]), flag::IR[1] | flag::IR[2] | flag::IR[3]);
    }

    #[test]
    fn mvmva_lm_clamps_negative_results_to_zero() {
        let mut gte = identity();
        gte.write(VXY0, xy(-3, 4));
        gte.write(VZ0, (-5i32) as u32);
        gte.execute(mvmva(0, 0, 3, SF | LM));
        assert_eq!(mac(&gte), [-3, 4, -5]);
        assert_eq!(ir(&gte), [0, 4, 0]);
        assert_eq!(gte.read(FLAG), flag::IR[1] | flag::IR[3] | (1 << 31));
    }

    #[test]
    fn mvmva_far_colour_keeps_only_the_last_two_products() {
        // psx-spx: "MAC1=(Mx12*Vx2+Mx13*Vx3) SAR (sf*12)".
        let mut gte = Gte::default();
        gte.write(RT, pack(0x1000, 0x1000));
        gte.write(RT + 1, pack(0x1000, 0));
        gte.write(VXY0, xy(1000, 20));
        gte.write(VZ0, 3);
        gte.write(RFC, 12345);
        gte.execute(mvmva(0, 0, 2, SF));
        assert_eq!(gte.read(MAC1), 23);
    }

    #[test]
    fn mvmva_garbage_matrix() {
        // psx-spx: Mx=3 elements "-R*10h, +R*10h, IR0, RT13, RT13, RT13,
        // RT22, RT22, RT22".
        let mut gte = Gte::default();
        gte.write(RGBC, 0x0000_0010); // R = 10h -> 100h
        gte.write(IR0, 0x1000);
        gte.write(RT + 1, 0x0800); // RT13
        gte.write(RT + 2, 0x0400); // RT22
        gte.write(VXY0, xy(0x10, 0x30));
        gte.write(VZ0, 5);
        gte.execute(mvmva(3, 0, 3, SF));
        // Row 1: (-100h*10h + 100h*30h + 1000h*5) >> 12 = 2 + 5.
        // Row 2: 800h*(10h+30h+5) >> 12; row 3: 400h*(...) >> 12.
        assert_eq!(mac(&gte), [7, (0x800 * 0x45) >> 12, (0x400 * 0x45) >> 12]);
    }

    #[test]
    fn square_and_cross_product() {
        // psx-spx: "SQR", "OP".
        let mut gte = Gte::default();
        set_ir(&mut gte, [0, 3, -4, 0x100]);
        gte.execute(SQR & !SF);
        assert_eq!(mac(&gte), [9, 16, 0x1_0000]);
        assert_eq!(ir(&gte), [9, 16, 0x7FFF]);
        set_ir(&mut gte, [0, 0x1000, -0x2000, 0x3000]);
        gte.execute(SQR | SF);
        assert_eq!(mac(&gte), [0x1000, 0x4000, 0x9000]);

        // (1,2,3) x (4,5,6) = (-3, 6, -3) with D = diagonal of RT.
        let mut gte = Gte::default();
        diagonal(&mut gte, RT, [4, 5, 6]);
        set_ir(&mut gte, [0, 1, 2, 3]);
        gte.execute(OP);
        // psx-spx formula: [IR3*D2-IR2*D3, IR1*D3-IR3*D1, IR2*D1-IR1*D2].
        assert_eq!(mac(&gte), [3 * 5 - 2 * 6, 6 - 3 * 4, 2 * 4 - 5]);
        assert_eq!(ir(&gte), [3, -6, 3]);
    }

    // ---- colour commands ---------------------------------------------------

    #[test]
    fn gpf_and_gpl_scale_and_accumulate() {
        // psx-spx: "GPF", "GPL".
        let mut gte = Gte::default();
        gte.write(RGBC, 0xAB00_0000);
        set_ir(&mut gte, [0x0800, 0x100, 0x200, 0x2000]);
        gte.execute(GPF | SF);
        assert_eq!(mac(&gte), [0x80, 0x100, 0x1000]);
        // Colour FIFO = MAC / 16, saturated to FFh, with the CODE of RGBC.
        assert_eq!(gte.read(RGB2), 0xAB00_0000 | 0x08 | (0x10 << 8) | (0xFF << 16));
        assert_eq!(gte.read(FLAG), flag::COLOR[2]);

        set_ir(&mut gte, [0x1000, 1, 2, 3]);
        gte.write(MAC1, 10);
        gte.write(MAC2, 20);
        gte.write(MAC3, 30);
        gte.execute(GPL | SF);
        assert_eq!(mac(&gte), [11, 22, 33]);
        assert_eq!(gte.read(RGB2 - 1), 0xAB00_0000 | 0x08 | (0x10 << 8) | (0xFF << 16), "FIFO moved");
    }

    #[test]
    fn depth_cueing_interpolates_towards_the_far_colour() {
        // psx-spx: "DPCS": MAC = [R,G,B] SHL 16, then MAC+(FC-MAC)*IR0.
        let mut gte = Gte::default();
        gte.write(RGBC, 0x2C40_8000 | 0x20); // R=20h G=80h B=40h
        for k in 0..3 {
            gte.write(RFC + k, 0xFF0); // far colour FFh in 4-bit fraction
        }
        // IR0 = 0: the colour is unchanged.
        gte.execute(DPCS);
        assert_eq!(gte.read(RGB2), 0x2C40_8020);
        // IR0 = 1.0: the far colour.
        gte.write(IR0, 0x1000);
        gte.execute(DPCS);
        assert_eq!(gte.read(RGB2), 0x2CFF_FFFF);
        // IR0 = 0.5: half way.
        gte.write(IR0, 0x0800);
        gte.execute(DPCS);
        let half = |c: u32| (c * 16 + (0xFF0 - c * 16) / 2) / 16;
        assert_eq!(gte.read(RGB2), 0x2C00_0000 | half(0x20) | (half(0x80) << 8) | (half(0x40) << 16));
        assert_eq!(gte.read(FLAG), 0);
    }

    #[test]
    fn dpct_reads_from_the_bottom_of_the_colour_fifo() {
        // psx-spx: "DPCT executes thrice, and reads the R,G,B values from
        // RGB0 ... (the CODE value is kept read from RGBC)".
        let mut gte = Gte::default();
        gte.write(RGBC, 0x7700_0000);
        gte.write(RGB0, 0x0011_2233);
        gte.write(RGB0 + 1, 0x0044_5566);
        gte.write(RGB2, 0x0077_8899);
        gte.execute(DPCT); // IR0 = 0: colours pass through
        assert_eq!([gte.read(RGB0), gte.read(RGB0 + 1), gte.read(RGB2)], [0x7711_2233, 0x7744_5566, 0x7777_8899]);
    }

    #[test]
    fn intpl_interpolates_the_ir_vector() {
        // psx-spx: "INTPL": MAC = IR SHL 12.
        let mut gte = Gte::default();
        set_ir(&mut gte, [0x0800, 0x100, 0x200, 0x300]);
        gte.write(RFC, 0x300);
        gte.write(RFC + 1, 0x200);
        gte.write(RFC + 2, 0x100);
        gte.execute(INTPL);
        assert_eq!(mac(&gte), [0x200, 0x200, 0x200]);
        assert_eq!(ir(&gte), [0x200, 0x200, 0x200]);
    }

    #[test]
    fn normal_colour_lights_a_vertex() {
        // psx-spx: "NCS": IR = LLM*V0, then IR = BK + LCM*IR, colour FIFO.
        let mut gte = Gte::default();
        diagonal(&mut gte, LLM, [0x1000; 3]);
        diagonal(&mut gte, LCM, [0x1000, 0x0800, 0x0400]);
        gte.write(VXY0, xy(0x800, 0x800));
        gte.write(VZ0, 0x800);
        gte.write(RBK, 0x100);
        gte.write(RGBC, 0x3400_0000);
        gte.execute(NCS);
        assert_eq!(mac(&gte), [0x900, 0x400, 0x200]);
        assert_eq!(gte.read(RGB2), 0x3400_0000 | 0x90 | (0x40 << 8) | (0x20 << 16));

        // NCCS multiplies by the primary colour: (R*IR) SHL 4 SAR 12.
        gte.write(RGBC, 0x3480_8080);
        gte.write(RBK, 0);
        gte.execute(NCCS);
        assert_eq!(mac(&gte), [0x400, 0x200, 0x100]);
        assert_eq!(gte.read(RGB2), 0x3400_0000 | 0x40 | (0x20 << 8) | (0x10 << 16));
    }

    #[test]
    fn flags_are_reset_by_every_command() {
        let mut gte = Gte::default();
        gte.write(FLAG, 0x7FFF_F000);
        gte.execute(NCLIP);
        assert_eq!(gte.read(FLAG), 0);
        gte.write(FLAG, 0x7FFF_F000);
        gte.execute(0x02); // an unused opcode
        assert_eq!(gte.read(FLAG), 0);
    }
}
