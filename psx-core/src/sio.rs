//! SIO0: the serial port that controllers and memory cards hang off, and the
//! devices themselves (digital pad, memory card).
//!
//! psx-spx: "Serial Interfaces (SIO)", "Controllers and Memory Cards".
//!
//! A byte written to the data register is shifted out at the configured
//! baud rate while the selected device shifts its reply in; the device then
//! pulses /ACK if it wants another byte, which raises IRQ7.
//!
//! Not emulated: SIO1 (the serial port at 1F801050h), the TX interrupt, the
//! baud rate timer readable in the status register, multitaps, analog pads
//! and every other controller type.

use crate::irq::{Interrupt, Irq};
use std::collections::VecDeque;

// psx-spx: "0x1f801044: SR (status register, read-only)".
const SR_TX_READY: u32 = 1 << 0;
const SR_RX_READY: u32 = 1 << 1;
const SR_TX_IDLE: u32 = 1 << 2;
const SR_DSR: u32 = 1 << 7;
const SR_IRQ: u32 = 1 << 9;

// psx-spx: "0x1f80104a: CR (control register)".
const CR_TX_ENABLE: u16 = 1 << 0;
/// "DTR is wired to the /CS pin ...; bit 1 will pull (assert) /CS low".
const CR_SELECT: u16 = 1 << 1;
/// SIO0: "1=force receiving single byte".
const CR_RX_FORCE: u16 = 1 << 2;
const CR_ACKNOWLEDGE: u16 = 1 << 4;
const CR_RESET: u16 = 1 << 6;
const CR_RX_IRQ_ENABLE: u16 = 1 << 11;
const CR_DSR_IRQ_ENABLE: u16 = 1 << 12;
/// "(SIO0) Port Select (0=port 1, 1=port 2)".
const CR_PORT: u16 = 1 << 13;
/// Bits that read back: 0-3, 5, 8-13 (4 and 6 are write-only strobes).
const CR_STORED: u16 = 0x3F2F;
/// psx-spx: "MR (mode register)": bits 0-5 and 8 exist on SIO0.
const MR_STORED: u16 = 0x013F;

/// "The hardware can hold 8 bytes in the RX direction".
const RX_FIFO_SIZE: usize = 8;

/// Delay from the end of a byte to the device pulling /ACK low.
/// psx-spx: "Memory Card ... Timings": "IRQ7 is usually triggered circa 1500
/// cycles after sending a byte (counted from the begin of the first bit)";
/// a byte takes 8 * 88h = 1088 cycles at the standard rate. It must not be
/// immediate: "Emulation Note" — the kernel acknowledges any old IRQ7 about
/// 100 cycles after sending, then waits for the new one.
const ACK_DELAY: u32 = 1500 - 1088;
/// "note: the LOW duration is circa 100 clock cycles".
const ACK_DURATION: u32 = 100;

/// Button bits of a digital pad. psx-spx: "Standard Controllers" —
/// halfword 1 (Digital Switches).
pub mod button {
    pub const SELECT: u16 = 1 << 0;
    pub const L3: u16 = 1 << 1;
    pub const R3: u16 = 1 << 2;
    pub const START: u16 = 1 << 3;
    pub const UP: u16 = 1 << 4;
    pub const RIGHT: u16 = 1 << 5;
    pub const DOWN: u16 = 1 << 6;
    pub const LEFT: u16 = 1 << 7;
    pub const L2: u16 = 1 << 8;
    pub const R2: u16 = 1 << 9;
    pub const L1: u16 = 1 << 10;
    pub const R1: u16 = 1 << 11;
    pub const TRIANGLE: u16 = 1 << 12;
    pub const CIRCLE: u16 = 1 << 13;
    pub const CROSS: u16 = 1 << 14;
    pub const SQUARE: u16 = 1 << 15;
}

/// A standard digital controller (SCPH-1080).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DigitalPad {
    /// Buttons held down, as a mask of [`button`] bits (1 = pressed; the
    /// wire format is inverted).
    pub pressed: u16,
}

/// A 128 KB memory card.
/// psx-spx: "Memory Card Read/Write Commands", "Memory Card Data Format".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryCard {
    data: Vec<u8>,
    /// "FLAG Byte": 08h after power-up/insertion, bit 3 cleared by a write.
    flag: u8,
    /// Set by every successful write, for frontends that save to disk.
    pub dirty: bool,
}

/// "Total Memory 128KB", "1 Frame 128 bytes".
pub const CARD_SIZE: usize = 128 * 1024;
const SECTOR_SIZE: usize = 128;
const SECTOR_COUNT: u16 = (CARD_SIZE / SECTOR_SIZE) as u16;

impl MemoryCard {
    /// A card with the given raw image (no header, as in .mcd/.mcr files).
    pub fn new(data: Vec<u8>) -> Option<Self> {
        (data.len() == CARD_SIZE).then_some(Self { data, flag: 0x08, dirty: false })
    }

    /// A freshly formatted, empty card.
    /// psx-spx: "Memory Card Data Format" — header frame, 15 free directory
    /// frames, the broken sector list, and the write test frame. The file
    /// blocks are not described for an empty card; they are zero here.
    pub fn formatted() -> Self {
        let mut data = vec![0u8; CARD_SIZE];
        let mut frame = |index: usize, bytes: &[u8], fill: u8| {
            let frame = &mut data[index * SECTOR_SIZE..][..SECTOR_SIZE];
            frame.fill(fill);
            frame[..bytes.len()].copy_from_slice(bytes);
        };
        let checksum = |data: &mut [u8], index: usize| {
            let frame = &mut data[index * SECTOR_SIZE..][..SECTOR_SIZE];
            frame[127] = frame[..127].iter().fold(0, |x, b| x ^ b);
        };
        // "Header Frame": "MC", zeros, checksum (0Eh). Frame 63, the "Write
        // Test Frame", is "usually same as Block 0".
        frame(0, b"MC", 0);
        frame(63, b"MC", 0);
        // "Directory Frames": state A0h (free, freshly formatted), no next
        // block (FFFFh).
        for index in 1..=15 {
            frame(index, &[0xA0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF], 0);
        }
        // "Broken Sector List": FFFFFFFFh = none.
        for index in 16..=35 {
            frame(index, &[0xFF; 4], 0);
        }
        // Replacement data and unused frames: "usually FFh-filled".
        for index in 36..=62 {
            frame(index, &[], 0xFF);
        }
        for index in (0..=35).chain([63]) {
            checksum(&mut data, index);
        }
        Self { data, flag: 0x08, dirty: false }
    }

    /// The raw 128 KB image.
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

/// Progress of a memory card command. `step` counts the bytes exchanged
/// after the command byte.
#[derive(Debug, Clone)]
struct CardSequence {
    command: u8,
    step: usize,
    sector: u16,
    previous: u8,
    checksum: u8,
    buffer: [u8; SECTOR_SIZE],
}

/// What the device on a port is in the middle of.
#[derive(Debug, Clone)]
enum Active {
    /// Waiting for the address byte.
    Idle,
    /// Addressed someone else, or the sequence is over: no reply until /CS
    /// is released.
    Ignoring,
    Pad { step: u8 },
    Card(CardSequence),
}

/// One controller port with its memory card slot ("wired in parallel").
#[derive(Debug, Clone)]
pub struct Port {
    pub pad: Option<DigitalPad>,
    pub card: Option<MemoryCard>,
    active: Active,
}

impl Default for Port {
    fn default() -> Self {
        Self { pad: None, card: None, active: Active::Idle }
    }
}

impl Port {
    /// /CS released: every device forgets where it was.
    fn deselect(&mut self) {
        self.active = Active::Idle;
    }

    /// Exchanges one byte. Returns the reply and whether the device pulses
    /// /ACK (it does not after the last byte of a sequence).
    /// With nothing driving the data line it reads as FFh ("High-Z").
    fn exchange(&mut self, byte: u8) -> (u8, bool) {
        const NONE: (u8, bool) = (0xFF, false);
        match std::mem::replace(&mut self.active, Active::Ignoring) {
            Active::Ignoring => NONE,
            // psx-spx: "Device addressing" — 01h controller, 81h memory card.
            Active::Idle => match byte {
                0x01 if self.pad.is_some() => {
                    self.active = Active::Pad { step: 0 };
                    (0xFF, true)
                }
                0x81 if self.card.is_some() => {
                    let fresh = CardSequence { command: 0, step: 0, sector: 0, previous: 0, checksum: 0, buffer: [0; SECTOR_SIZE] };
                    self.active = Active::Card(fresh);
                    (0xFF, true)
                }
                _ => NONE,
            },
            Active::Pad { step } => {
                let Some(pad) = self.pad else { return NONE };
                // psx-spx: "Controller Communication Sequence": 42h -> idlo
                // (41h for a digital pad), TAP -> idhi (5Ah), then the two
                // switch bytes, "0=Pressed, 1=Released".
                let switches = !pad.pressed;
                let reply = match (step, byte) {
                    (0, 0x42) => 0x41,
                    // Other commands (43h configuration and up) belong to
                    // analog pads; a digital pad does not answer them.
                    (0, _) => return NONE,
                    (1, _) => 0x5A,
                    (2, _) => switches as u8,
                    _ => (switches >> 8) as u8,
                };
                if step < 3 {
                    self.active = Active::Pad { step: step + 1 };
                }
                (reply, step < 3)
            }
            Active::Card(sequence) => {
                let Some(card) = self.card.as_mut() else { return NONE };
                let (reply, next) = card.exchange(sequence, byte);
                let more = next.is_some();
                if let Some(next) = next {
                    self.active = Active::Card(next);
                }
                (reply, more)
            }
        }
    }
}

impl MemoryCard {
    /// One byte of a card command. Returns the reply and the state for the
    /// next byte, or `None` when this was the last byte.
    fn exchange(&mut self, mut s: CardSequence, byte: u8) -> (u8, Option<CardSequence>) {
        let step = s.step;
        s.step += 1;
        let previous = std::mem::replace(&mut s.previous, byte);
        let more = |reply: u8, s: CardSequence| (reply, Some(s));

        // The command byte: "52h FLAG Send Read Command", "57h" write,
        // "53h" get ID. "Invalid Commands": "Transfer aborts immediately
        // after the faulty command byte".
        if s.command == 0 {
            s.command = byte;
            s.step = 1;
            let known = matches!(byte, b'R' | b'W' | b'S');
            return (self.flag, known.then_some(s));
        }
        let valid = s.sector < SECTOR_COUNT;
        let start = s.sector as usize * SECTOR_SIZE;
        match (s.command, step) {
            // All three commands: "5Ah Receive Memory Card ID1", "5Dh ID2".
            (_, 1) => more(0x5A, s),
            (_, 2) => more(0x5D, s),

            // "Get Memory Card ID Command".
            (b'S', 3..=7) => more([0x5C, 0x5D, 0x04, 0x00, 0x00][step - 3], s),
            (b'S', _) => (0x80, None),

            // Read and write: "MSB (00h) Send Address MSB", "LSB (pre)".
            (_, 3) => {
                s.sector = (byte as u16) << 8;
                s.checksum = byte;
                more(0x00, s)
            }
            (_, 4) => {
                s.sector |= byte as u16;
                s.checksum ^= byte;
                more(previous, s)
            }

            // "Reading Data from Memory Card".
            (b'R', 5) => more(0x5C, s),
            (b'R', 6) => more(0x5D, s),
            // "When sending an invalid sector number, original Sony memory
            // cards respond with FFFFh as Confirmed Address (and do then
            // abort the transfer".
            (b'R', 7) => more(if valid { (s.sector >> 8) as u8 } else { 0xFF }, s),
            (b'R', 8) if !valid => (0xFF, None),
            (b'R', 8) => more(s.sector as u8, s),
            (b'R', 9..=136) => {
                let data = self.data[start + step - 9];
                s.checksum ^= data;
                more(data, s)
            }
            // "CHK Receive Checksum (MSB xor LSB xor Data bytes)", then
            // "47h Receive Memory End Byte".
            (b'R', 137) => more(s.checksum, s),
            (b'R', _) => (0x47, None),

            // "Writing Data to Memory Card": 128 data bytes and the
            // checksum, each answered with the previous byte.
            (_, 5..=132) => {
                s.buffer[step - 5] = byte;
                s.checksum ^= byte;
                more(previous, s)
            }
            (_, 133) => {
                // The received checksum must equal the computed one.
                s.checksum ^= byte;
                more(previous, s)
            }
            (_, 134) => more(0x5C, s),
            (_, 135) => more(0x5D, s),
            // "Receive Memory End Byte (47h=Good, 4Eh=BadChecksum,
            // FFh=BadSector)".
            _ => {
                let end = if !valid {
                    0xFF
                } else if s.checksum != 0 {
                    0x4E
                } else {
                    self.data[start..start + SECTOR_SIZE].copy_from_slice(&s.buffer);
                    self.dirty = true;
                    // "bit3 is NOT reset when reading from the card, but
                    // rather when writing to it".
                    self.flag &= !0x08;
                    0x47
                };
                (end, None)
            }
        }
    }
}

/// A byte being shifted out, or the /ACK pulse that follows it.
#[derive(Debug, Clone, Copy)]
enum Phase {
    Idle,
    /// Shifting `byte`; `left` cycles to go.
    Transfer { byte: u8, left: u32 },
    /// Transfer done; the device pulls /ACK low after `left` cycles.
    AckPending { left: u32 },
    /// /ACK is low for another `left` cycles.
    AckLow { left: u32 },
}

pub struct Sio0 {
    pub ports: [Port; 2],
    mode: u16,
    control: u16,
    baud: u16,
    /// A byte written to DR while TX was disabled or busy.
    pending: Option<u8>,
    phase: Phase,
    rx: VecDeque<u8>,
    last_rx: u8,
    /// SR.9, "(sticky)".
    irq: bool,
}

impl Default for Sio0 {
    fn default() -> Self {
        Self {
            ports: [Port::default(), Port::default()],
            mode: 0,
            control: 0,
            baud: 0,
            pending: None,
            phase: Phase::Idle,
            rx: VecDeque::with_capacity(RX_FIFO_SIZE),
            last_rx: 0,
            irq: false,
        }
    }
}

impl Sio0 {
    fn selected_port(&self) -> usize {
        (self.control & CR_PORT != 0) as usize
    }

    /// CPU cycles one byte takes on the wire.
    /// psx-spx: "BR (baud rate register)": "SIO0: BitsPerSecond = 33868800 /
    /// MAX(((Reload*Factor) AND NOT 1),1)", with the factor from MR.0-1
    /// "(1=MUL1, 2=MUL16, 3=MUL64) (or 0=MUL1 on SIO0)".
    fn byte_cycles(&self) -> u32 {
        let factor = [1, 1, 16, 64][(self.mode & 3) as usize];
        8 * ((self.baud as u32 * factor) & !1).max(1)
    }

    fn status(&self) -> u32 {
        let transferring = matches!(self.phase, Phase::Transfer { .. });
        let mut status = 0;
        // "As soon as the transfer starts, SR.0 becomes set (indicating
        // that one can write a new byte to DR ...). As soon as the transfer
        // of the most recently written byte ends, SR.2 becomes set."
        if self.pending.is_none() {
            status |= SR_TX_READY;
            if !transferring {
                status |= SR_TX_IDLE;
            }
        }
        if !self.rx.is_empty() {
            status |= SR_RX_READY;
        }
        // "bit 7 is thus set when /ACK is low (asserted)".
        if matches!(self.phase, Phase::AckLow { .. }) {
            status |= SR_DSR;
        }
        if self.irq {
            status |= SR_IRQ;
        }
        status
    }

    /// Reads a register; `offset` is relative to 1F801040h, `bytes` the
    /// access width.
    pub fn read(&mut self, offset: u32, bytes: usize) -> u32 {
        match offset & !1 {
            // "DR": "0-7 Received Data (1st RX FIFO entry)", "8-31 Preview".
            // "A 16bit read ... removes only ONE entry from the FIFO. On
            // the contrary, a 32bit read DOES remove FOUR entries".
            0x0 | 0x2 => {
                // "Reading from Empty RX FIFO returns either the most
                // recently received byte or zero".
                let mut value = self.last_rx as u32;
                for (i, byte) in self.rx.iter().take(4).enumerate() {
                    value = (value & !(0xFF << (8 * i))) | ((*byte as u32) << (8 * i));
                }
                for _ in 0..if bytes == 4 { 4 } else { 1 } {
                    self.rx.pop_front();
                }
                value >> (8 * (offset & 3))
            }
            0x4 | 0x6 => self.status() >> (8 * (offset & 3)),
            0x8 => self.mode as u32,
            0xA => self.control as u32,
            0xE => self.baud as u32,
            _ => 0,
        }
    }

    /// Writes a register; `offset` is relative to 1F801040h.
    pub fn write(&mut self, offset: u32, value: u32) {
        match offset & !1 {
            // "DR": "Writing to this register starts a transfer (if, or as
            // soon as, TXEN=1 ...)". "Writing ... while SR.0=Busy causes the
            // old value to be overwritten."
            0x0 => {
                self.pending = Some(value as u8);
                self.start_transfer();
            }
            0x8 => self.mode = value as u16 & MR_STORED,
            0xA => self.write_control(value as u16),
            0xE => self.baud = value as u16,
            // SR is read-only.
            _ => {}
        }
    }

    fn write_control(&mut self, value: u16) {
        // "INTRST Reset (0=No change, 1=Reset most registers to zero)".
        if value & CR_RESET != 0 {
            let ports = std::mem::take(&mut self.ports);
            *self = Self { ports, ..Self::default() };
            self.ports.iter_mut().for_each(Port::deselect);
            return;
        }
        let was_selected = self.control & CR_SELECT != 0;
        let old_port = self.selected_port();
        self.control = value & CR_STORED;
        // /CS released, or moved to the other port: the device that was
        // selected starts over.
        if was_selected && (value & CR_SELECT == 0 || self.selected_port() != old_port) {
            self.ports[old_port].deselect();
        }
        // "ERRRST Acknowledge (... 1=Reset SR.Bits 3,4,5,9)". psx-spx:
        // "SIO0_STAT.9 ... CANNOT be acknowledged while the external /IRQ
        // input is still low".
        if value & CR_ACKNOWLEDGE != 0 {
            self.irq = matches!(self.phase, Phase::AckLow { .. }) && self.control & CR_DSR_IRQ_ENABLE != 0;
        }
        self.start_transfer();
    }

    fn start_transfer(&mut self) {
        // Deviation: psx-spx describes a latched TXEN (a transfer can start
        // if TXEN was set when DR was written, even if cleared since); only
        // the current TXEN is looked at.
        if self.control & CR_TX_ENABLE == 0 || matches!(self.phase, Phase::Transfer { .. }) {
            return;
        }
        if let Some(byte) = self.pending.take() {
            self.phase = Phase::Transfer { byte, left: self.byte_cycles() };
        }
    }

    /// Advances the port by `cycles` CPU cycles.
    pub fn tick(&mut self, mut cycles: u32, irq: &mut Irq) {
        while cycles > 0 {
            match &mut self.phase {
                Phase::Idle => return,
                Phase::Transfer { left, .. } | Phase::AckPending { left } | Phase::AckLow { left } => {
                    let step = cycles.min(*left);
                    *left -= step;
                    cycles -= step;
                    if *left > 0 {
                        return;
                    }
                }
            }
            match self.phase {
                Phase::Transfer { byte, .. } => self.finish_transfer(byte, irq),
                Phase::AckPending { .. } => {
                    self.phase = Phase::AckLow { left: ACK_DURATION };
                    // psx-spx: "DSR (/ACK) Controller and Memory Card - Byte
                    // Received Interrupt"; CR.12 "DSR Interrupt Enable".
                    if self.control & CR_DSR_IRQ_ENABLE != 0 {
                        self.raise(irq);
                    }
                }
                Phase::AckLow { .. } => {
                    self.phase = Phase::Idle;
                    self.start_transfer();
                }
                Phase::Idle => {}
            }
        }
    }

    fn finish_transfer(&mut self, byte: u8, irq: &mut Irq) {
        let selected = self.control & CR_SELECT != 0;
        let (reply, ack) = if selected { self.ports[self.selected_port()].exchange(byte) } else { (0xFF, false) };
        // CR.2 on SIO0: "when not set, incoming data will be ignored unless
        // bit 1 is also set. When set, data will be received regardless ...
        // however bit 2 will be automatically cleared after a byte is
        // received."
        if selected || self.control & CR_RX_FORCE != 0 {
            self.control &= !CR_RX_FORCE;
            if self.rx.len() == RX_FIFO_SIZE {
                // "the last FIFO entry will by overwritten by the new byte".
                self.rx.pop_back();
            }
            self.rx.push_back(reply);
            self.last_rx = reply;
            // CR.8-9 "RX Interrupt Mode (0..3 = IRQ when RX FIFO contains
            // 1,2,4,8 bytes)", CR.11 "RX Interrupt Enable".
            let threshold = 1usize << ((self.control >> 8) & 3);
            if self.control & CR_RX_IRQ_ENABLE != 0 && self.rx.len() >= threshold {
                self.raise(irq);
            }
        }
        self.phase = if ack { Phase::AckPending { left: ACK_DELAY } } else { Phase::Idle };
        if !ack {
            self.start_transfer();
        }
    }

    /// Sets SR.9 and, on its rising edge, requests IRQ7.
    fn raise(&mut self, irq: &mut Irq) {
        if !self.irq {
            irq.request(Interrupt::Controller);
        }
        self.irq = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DATA: u32 = 0x0;
    const STAT: u32 = 0x4;
    const MODE: u32 = 0x8;
    const CTRL: u32 = 0xA;
    const BAUD: u32 = 0xE;

    /// Port set up the way the kernel does: 250 kHz, 8 bits, port 1
    /// selected, TX enabled, /ACK interrupt enabled.
    fn sio() -> (Sio0, Irq) {
        let mut sio = Sio0::default();
        sio.write(MODE, 0x000D);
        sio.write(BAUD, 0x0088);
        sio.write(CTRL, 0x1003);
        (sio, Irq::default())
    }

    /// Sends a byte and runs until the reply is in; returns the reply and
    /// whether /ACK was pulsed.
    fn send(sio: &mut Sio0, irq: &mut Irq, byte: u8) -> (u8, bool) {
        sio.write(DATA, byte as u32);
        sio.tick(8 * 0x88, irq);
        assert_ne!(sio.read(STAT, 4) & SR_RX_READY, 0, "reply received");
        let reply = sio.read(DATA, 1) as u8;
        sio.tick(ACK_DELAY, irq);
        let ack = sio.read(STAT, 4) & SR_DSR != 0;
        sio.tick(ACK_DURATION, irq);
        (reply, ack)
    }

    fn exchange(sio: &mut Sio0, irq: &mut Irq, bytes: &[u8]) -> Vec<u8> {
        bytes.iter().map(|b| send(sio, irq, *b).0).collect()
    }

    // ---- port --------------------------------------------------------------

    #[test]
    fn transfer_takes_eight_bit_times_and_sets_status_bits() {
        let (mut sio, mut irq) = sio();
        assert_eq!(sio.read(STAT, 4), SR_TX_READY | SR_TX_IDLE);
        sio.write(DATA, 0x01);
        assert_eq!(sio.read(STAT, 4), SR_TX_READY, "busy: not idle");
        sio.tick(8 * 0x88 - 1, &mut irq);
        assert_eq!(sio.read(STAT, 4) & SR_RX_READY, 0);
        sio.tick(1, &mut irq);
        assert_eq!(sio.read(STAT, 4), SR_TX_READY | SR_TX_IDLE | SR_RX_READY);
        assert_eq!(sio.read(DATA, 1), 0xFF, "nothing connected: high-Z");
        assert_eq!(sio.read(STAT, 4) & SR_RX_READY, 0, "FIFO emptied by the read");
    }

    #[test]
    fn no_device_means_no_ack_and_no_interrupt() {
        // psx-spx: "there will be no IRQ if ... there's no peripheral
        // connected at all".
        let (mut sio, mut irq) = sio();
        assert_eq!(send(&mut sio, &mut irq, 0x01), (0xFF, false));
        assert_eq!(irq.stat(), 0);
    }

    #[test]
    fn ack_pulse_raises_irq7_after_a_delay() {
        let (mut sio, mut irq) = sio();
        sio.ports[0].pad = Some(DigitalPad::default());
        sio.write(DATA, 0x01);
        sio.tick(8 * 0x88, &mut irq);
        assert_eq!(irq.stat(), 0, "not immediately (psx-spx 'Emulation Note')");
        sio.tick(ACK_DELAY - 1, &mut irq);
        assert_eq!(irq.stat(), 0);
        sio.tick(1, &mut irq);
        assert_eq!(irq.stat(), 1 << 7);
        let status = sio.read(STAT, 4);
        assert_ne!(status & SR_DSR, 0);
        assert_ne!(status & SR_IRQ, 0);
        // SR.9 cannot be acknowledged while /ACK is still low...
        sio.write(CTRL, 0x1013);
        assert_ne!(sio.read(STAT, 4) & SR_IRQ, 0);
        // ...but can afterwards.
        sio.tick(ACK_DURATION, &mut irq);
        assert_eq!(sio.read(STAT, 4) & SR_DSR, 0);
        sio.write(CTRL, 0x1013);
        assert_eq!(sio.read(STAT, 4) & SR_IRQ, 0);
        assert_eq!(sio.read(CTRL, 2), 0x1003, "the acknowledge bit is not stored");
    }

    #[test]
    fn ack_interrupt_needs_its_enable_bit() {
        let (mut sio, mut irq) = sio();
        sio.ports[0].pad = Some(DigitalPad::default());
        sio.write(CTRL, 0x0003);
        assert_eq!(send(&mut sio, &mut irq, 0x01), (0xFF, true));
        assert_eq!(irq.stat(), 0);
    }

    #[test]
    fn register_masks_and_reset() {
        // Values from the console log of ps1-tests cpu/io-access-bitwidth:
        // JOY_MODE 5678h reads 38h, JOY_CTRL 5678h (reset bit set) reads 0.
        let mut sio = Sio0::default();
        sio.write(MODE, 0x5678);
        assert_eq!(sio.read(MODE, 2), 0x0038);
        sio.write(BAUD, 0x1234);
        sio.write(CTRL, 0x5678);
        assert_eq!(sio.read(CTRL, 2), 0);
        assert_eq!(sio.read(MODE, 2), 0, "reset clears the other registers");
        assert_eq!(sio.read(BAUD, 2), 0);
    }

    #[test]
    fn byte_written_while_tx_is_disabled_waits() {
        let (mut sio, mut irq) = sio();
        sio.write(CTRL, 0x1002);
        sio.write(DATA, 0x01);
        assert_eq!(sio.read(STAT, 4) & (SR_TX_READY | SR_TX_IDLE), 0);
        sio.tick(5000, &mut irq);
        assert_eq!(sio.read(STAT, 4) & SR_RX_READY, 0);
        sio.write(CTRL, 0x1003);
        sio.tick(8 * 0x88, &mut irq);
        assert_ne!(sio.read(STAT, 4) & SR_RX_READY, 0);
    }

    #[test]
    fn baud_rate_factor() {
        let mut sio = Sio0::default();
        sio.write(BAUD, 0x0089);
        assert_eq!(sio.byte_cycles(), 8 * 0x88, "factor 0 = MUL1, low bit dropped");
        sio.write(MODE, 2);
        assert_eq!(sio.byte_cycles(), 8 * 0x890);
        sio.write(BAUD, 0);
        assert_eq!(sio.byte_cycles(), 8);
    }

    // ---- digital pad -------------------------------------------------------

    #[test]
    fn digital_pad_read_sequence() {
        // psx-spx: "Controller Communication Sequence".
        let (mut sio, mut irq) = sio();
        sio.ports[0].pad = Some(DigitalPad { pressed: button::START | button::CROSS });
        assert_eq!(send(&mut sio, &mut irq, 0x01), (0xFF, true));
        assert_eq!(send(&mut sio, &mut irq, 0x42), (0x41, true));
        assert_eq!(send(&mut sio, &mut irq, 0x00), (0x5A, true));
        assert_eq!(send(&mut sio, &mut irq, 0x00), (!0x08, true), "start = bit 3, 0 = pressed");
        assert_eq!(send(&mut sio, &mut irq, 0x00), (!0x40, false), "cross = bit 14; no /ACK after the last byte");
        // Further bytes get no answer until /CS is released.
        assert_eq!(send(&mut sio, &mut irq, 0x01), (0xFF, false));
        sio.write(CTRL, 0x1001);
        sio.write(CTRL, 0x1003);
        assert_eq!(send(&mut sio, &mut irq, 0x01), (0xFF, true));
    }

    #[test]
    fn pad_answers_only_on_its_port_and_address() {
        let (mut sio, mut irq) = sio();
        sio.ports[1].pad = Some(DigitalPad::default());
        assert_eq!(send(&mut sio, &mut irq, 0x01), (0xFF, false), "port 1 is empty");
        sio.write(CTRL, 0x3003); // port 2
        assert_eq!(send(&mut sio, &mut irq, 0x81), (0xFF, false), "no memory card there");
        sio.write(CTRL, 0x3001);
        sio.write(CTRL, 0x3003);
        assert_eq!(exchange(&mut sio, &mut irq, &[0x01, 0x42, 0, 0, 0]), [0xFF, 0x41, 0x5A, 0xFF, 0xFF]);
    }

    #[test]
    fn digital_pad_ignores_configuration_commands() {
        let (mut sio, mut irq) = sio();
        sio.ports[0].pad = Some(DigitalPad::default());
        send(&mut sio, &mut irq, 0x01);
        assert_eq!(send(&mut sio, &mut irq, 0x43), (0xFF, false));
    }

    // ---- memory card -------------------------------------------------------

    fn with_card() -> (Sio0, Irq) {
        let (mut sio, irq) = sio();
        sio.ports[0].card = Some(MemoryCard::formatted());
        (sio, irq)
    }

    fn reselect(sio: &mut Sio0) {
        sio.write(CTRL, 0x1001);
        sio.write(CTRL, 0x1003);
    }

    #[test]
    fn formatted_card_layout() {
        // psx-spx: "Memory Card Data Format".
        let card = MemoryCard::formatted();
        let frame = |n: usize| &card.data()[n * 128..(n + 1) * 128];
        assert_eq!(&frame(0)[..2], b"MC");
        assert_eq!(frame(0)[127], 0x0E, "header checksum");
        assert_eq!(&frame(1)[..10], &[0xA0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF]);
        assert_eq!(frame(15)[127], 0xA0);
        assert_eq!(&frame(16)[..4], &[0xFF; 4]);
        assert_eq!(frame(16)[127], 0);
        assert!(frame(40).iter().all(|b| *b == 0xFF));
        assert_eq!(frame(63), frame(0));
        assert_eq!(card.data().len(), CARD_SIZE);
        assert!(MemoryCard::new(vec![0; 1000]).is_none());
    }

    #[test]
    fn card_read_sequence() {
        // psx-spx: "Reading Data from Memory Card". Sector 1 is the first
        // directory frame of a formatted card.
        let (mut sio, mut irq) = with_card();
        let head = exchange(&mut sio, &mut irq, &[0x81, 0x52, 0, 0, 0x00, 0x01, 0, 0, 0, 0]);
        assert_eq!(head, [0xFF, 0x08, 0x5A, 0x5D, 0x00, 0x00, 0x5C, 0x5D, 0x00, 0x01]);
        let data = exchange(&mut sio, &mut irq, &[0; 128]);
        assert_eq!(&data[..10], &[0xA0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF]);
        // Checksum = MSB xor LSB xor data = 00 ^ 01 ^ (A0 ^ FF ^ FF ^ A0).
        assert_eq!(send(&mut sio, &mut irq, 0), (0x01, true));
        assert_eq!(send(&mut sio, &mut irq, 0), (0x47, false), "end byte, last");
    }

    #[test]
    fn card_read_of_an_invalid_sector_aborts() {
        let (mut sio, mut irq) = with_card();
        let reply = exchange(&mut sio, &mut irq, &[0x81, 0x52, 0, 0, 0x04, 0x00, 0, 0, 0]);
        assert_eq!(reply[8], 0xFF);
        assert_eq!(send(&mut sio, &mut irq, 0), (0xFF, false));
    }

    /// Runs a complete write command; returns the end byte.
    fn write_sector(sio: &mut Sio0, irq: &mut Irq, sector: u16, data: &[u8; 128], bad_checksum: bool) -> u8 {
        reselect(sio);
        let (msb, lsb) = ((sector >> 8) as u8, sector as u8);
        let head = exchange(sio, irq, &[0x81, 0x57, 0, 0, msb, lsb]);
        assert_eq!(&head[2..], &[0x5A, 0x5D, 0x00, msb]);
        let echoed = exchange(sio, irq, data);
        assert_eq!(echoed[0], lsb, "each reply is the previous byte");
        assert_eq!(echoed[1], data[0]);
        let checksum = data.iter().fold(msb ^ lsb, |x, b| x ^ b) ^ bad_checksum as u8;
        let tail = exchange(sio, irq, &[checksum, 0, 0]);
        assert_eq!(&tail[1..], &[0x5C, 0x5D]);
        let (end, ack) = send(sio, irq, 0);
        assert!(!ack);
        end
    }

    #[test]
    fn card_write_stores_data_and_clears_the_new_card_flag() {
        // psx-spx: "Writing Data to Memory Card", "FLAG Byte".
        let (mut sio, mut irq) = with_card();
        let mut data = [0u8; 128];
        data.iter_mut().enumerate().for_each(|(i, b)| *b = i as u8);
        assert_eq!(write_sector(&mut sio, &mut irq, 0x3F, &data, false), 0x47);
        let card = sio.ports[0].card.as_ref().unwrap();
        assert_eq!(&card.data()[0x3F * 128..0x40 * 128], &data);
        assert!(card.dirty);
        // The flag byte no longer has bit 3.
        reselect(&mut sio);
        assert_eq!(exchange(&mut sio, &mut irq, &[0x81, 0x52])[1], 0x00);
    }

    #[test]
    fn card_write_errors() {
        let (mut sio, mut irq) = with_card();
        let data = [0x55u8; 128];
        assert_eq!(write_sector(&mut sio, &mut irq, 5, &data, true), 0x4E, "bad checksum");
        assert_eq!(write_sector(&mut sio, &mut irq, 0x400, &data, false), 0xFF, "bad sector");
        let card = sio.ports[0].card.as_ref().unwrap();
        assert_ne!(&card.data()[5 * 128..6 * 128], &data, "nothing written");
        assert!(!card.dirty);
        reselect(&mut sio);
        assert_eq!(exchange(&mut sio, &mut irq, &[0x81, 0x52])[1], 0x08, "flag untouched");
    }

    #[test]
    fn card_id_and_invalid_commands() {
        // psx-spx: "Get Memory Card ID Command", "Invalid Commands".
        let (mut sio, mut irq) = with_card();
        let reply = exchange(&mut sio, &mut irq, &[0x81, 0x53, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(reply, [0xFF, 0x08, 0x5A, 0x5D, 0x5C, 0x5D, 0x04, 0x00, 0x00, 0x80]);
        reselect(&mut sio);
        send(&mut sio, &mut irq, 0x81);
        assert_eq!(send(&mut sio, &mut irq, 0x99), (0x08, false), "flag, then the transfer aborts");
    }

    #[test]
    fn pad_and_card_share_a_port() {
        let (mut sio, mut irq) = with_card();
        sio.ports[0].pad = Some(DigitalPad::default());
        assert_eq!(exchange(&mut sio, &mut irq, &[0x01, 0x42])[1], 0x41);
        reselect(&mut sio);
        assert_eq!(exchange(&mut sio, &mut irq, &[0x81, 0x53, 0])[2], 0x5A);
    }
}
