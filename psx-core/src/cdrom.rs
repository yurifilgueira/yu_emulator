//! CD-ROM drive controller.
//!
//! psx-spx: "CDROM Drive".
//!
//! The CPU talks to a decoder chip with four banked byte registers; behind
//! it a microcontroller executes commands and answers with interrupts:
//! INT3 acknowledges a command, INT2 reports its completion, INT1 announces
//! a sector (or a play report), INT4 the end of a track and INT5 an error.
//!
//! Not emulated: XA-ADPCM and CD-DA audio output (sectors meant for the
//! audio decoder are skipped; playing only moves the position), sound map,
//! the audio volume registers, seek times that depend on distance, drive
//! door, multiple sessions, most test commands and the unlock commands.

use crate::disc::{from_bcd, Disc, Msf, SECTORS_PER_SECOND};
use crate::irq::{Interrupt, Irq};
use std::collections::VecDeque;

// psx-spx: "0x1f801803 (read, banks 1 and 3): HINTSTS".
const INT_DATA_READY: u8 = 1;
const INT_COMPLETE: u8 = 2;
const INT_ACKNOWLEDGE: u8 = 3;
const INT_DATA_END: u8 = 4;
const INT_ERROR: u8 = 5;

// psx-spx: "Status code (stat)".
const STAT_ERROR: u8 = 1 << 0;
const STAT_MOTOR: u8 = 1 << 1;
const STAT_ID_ERROR: u8 = 1 << 3;
const STAT_READ: u8 = 1 << 5;
const STAT_SEEK: u8 = 1 << 6;
const STAT_PLAY: u8 = 1 << 7;

// Error bytes that follow stat in an INT5.
const ERROR_INVALID_PARAMETER: u8 = 0x10;
const ERROR_PARAMETER_COUNT: u8 = 0x20;
const ERROR_INVALID_COMMAND: u8 = 0x40;
const ERROR_NOT_READY: u8 = 0x80;

// psx-spx: "Setmode - Command 0Eh".
const MODE_DOUBLE_SPEED: u8 = 1 << 7;
const MODE_XA_ADPCM: u8 = 1 << 6;
const MODE_WHOLE_SECTOR: u8 = 1 << 5;
const MODE_XA_FILTER: u8 = 1 << 3;
const MODE_REPORT: u8 = 1 << 2;
const MODE_AUTO_PAUSE: u8 = 1 << 1;

// Timings in CPU cycles. psx-spx: "CDROM - Response Timings" (averages
// measured on a PSone).
/// "Nop (normal) 000c4e1h": first response of ordinary commands.
const ACK_DELAY: u32 = 0x00_C4E1;
/// "Init 0013cceh": first response of Init (and ReadTOC, "similar timing").
const ACK_DELAY_INIT: u32 = 0x01_3CCE;
/// "GetID 0004a00h": its second response.
const GETID_DELAY: u32 = 0x4A00;
/// "Pause (when paused) 0001df2h" / "Stop (when stopped) 0001d7bh".
const PAUSE_IDLE_DELAY: u32 = 0x1DF2;
const STOP_IDLE_DELAY: u32 = 0x1D7B;
/// "Stop (single speed) 0d38acah".
const STOP_DELAY: u32 = 0x0D3_8ACA;
/// ReadTOC: "the second response appears after about 1 second delay".
const TOC_DELAY: u32 = 33_868_800;
/// One sector at single speed: "SystemClock*930h/4/44100Hz".
const SECTOR_CYCLES: u32 = 33_868_800 / SECTORS_PER_SECOND;
/// Delay before a queued interrupt follows an acknowledged one. psx-spx:
/// "there seems to be a small delay between the acknowledge and the next
/// interrupt"; the length is a guess.
const NEXT_INT_DELAY: u32 = 2000;
/// Seek time. psx-spx: "The seek timings are still unknown, and they are
/// probably quite complicated". A fixed, short time is used.
const SEEK_SECTORS: u32 = 4;

/// What the drive mechanism is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drive {
    Stopped,
    /// Spinning, head resting.
    Idle,
    /// Moving to the Setloc target; then start `Then`.
    Seeking { then: Then, left: u32 },
    Reading { left: u32 },
    Playing { left: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Then {
    Pause,
    Read,
    Play,
}

/// A second response still to come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Second {
    /// INT2(stat) after the delay.
    Complete,
    GetId,
    Pause,
    Stop,
}

/// An interrupt with the bytes of its result FIFO.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Response {
    int: u8,
    bytes: Vec<u8>,
    /// For INT1 of a read: the sector data that comes with it.
    sector: Option<Vec<u8>>,
}

pub struct Cdrom {
    disc: Option<Disc>,
    /// Register bank (HSTS/ADDRESS bits 0-1).
    bank: u8,
    int_enable: u8,
    /// HINTSTS bits 0-2: the interrupt being presented.
    int_flags: u8,
    parameters: VecDeque<u8>,
    /// Result FIFO: 16 bytes, read position, and bytes of the response.
    result: [u8; 16],
    result_pos: usize,
    result_len: usize,
    /// Responses waiting for the current interrupt to be acknowledged.
    queue: VecDeque<Response>,
    /// Cycles until the next queued response may be presented.
    queue_delay: u32,

    /// Command written but not yet executed, and cycles until it is.
    command: Option<(u8, u32)>,
    second: Option<(Second, u32)>,

    drive: Drive,
    mode: u8,
    filter_file: u8,
    filter_channel: u8,
    /// Setloc target not yet used by a seek.
    target: Option<u32>,
    /// Sector under the head.
    position: u32,
    /// Header and subheader of the last data sector read (GetlocL).
    last_header: Option<[u8; 8]>,
    muted: bool,

    /// Sector that came with the last INT1, not yet requested.
    pending_sector: Option<Vec<u8>>,
    /// Sector buffer being read through RDDATA.
    data: Vec<u8>,
    data_pos: usize,
}

impl Default for Cdrom {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Cdrom {
    pub fn new(disc: Option<Disc>) -> Self {
        // With a disc in the drive the motor is running by the time software
        // looks (the drive spins up and reads the TOC on power-up).
        let drive = if disc.is_some() { Drive::Idle } else { Drive::Stopped };
        Self {
            disc,
            bank: 0,
            int_enable: 0,
            int_flags: 0,
            parameters: VecDeque::new(),
            result: [0; 16],
            result_pos: 0,
            result_len: 0,
            queue: VecDeque::new(),
            queue_delay: 0,
            command: None,
            second: None,
            drive,
            mode: 0,
            filter_file: 0,
            filter_channel: 0,
            target: None,
            position: 0,
            last_header: None,
            muted: false,
            pending_sector: None,
            data: Vec::new(),
            data_pos: 0,
        }
    }

    /// Puts a disc in the drive (or removes it), as if at power-up.
    pub fn insert(&mut self, disc: Option<Disc>) {
        *self = Self::new(disc);
    }

    pub fn has_disc(&self) -> bool {
        self.disc.is_some()
    }

    // ---- registers ---------------------------------------------------------

    /// Reads register 0-3 (1F801800h + n).
    /// psx-spx: "CDROM Controller I/O Ports".
    pub fn read(&mut self, register: u32) -> u8 {
        match register & 3 {
            // HSTS.
            0 => {
                let mut status = self.bank;
                status |= (self.parameters.is_empty() as u8) << 3;
                status |= ((self.parameters.len() < 16) as u8) << 4;
                status |= ((self.result_pos < self.result_len) as u8) << 5;
                status |= ((self.data_pos < self.data.len()) as u8) << 6;
                status |= (self.command.is_some() as u8) << 7;
                status
            }
            // RESULT: "padded with 00h's to the end of the 16-bytes, and does
            // then restart at the first response byte".
            1 => {
                let byte = self.result[self.result_pos % 16];
                self.result_pos += 1;
                if self.result_pos >= 16 + self.result_len.max(1) {
                    self.result_pos -= 16;
                }
                byte
            }
            2 => self.read_data(),
            // HINTMSK in banks 0 and 2, HINTSTS in banks 1 and 3; bits 5-7
            // "always 1".
            _ => {
                if self.bank & 1 == 0 {
                    self.int_enable | 0xE0
                } else {
                    self.int_flags | 0xE0
                }
            }
        }
    }

    /// RDDATA: one byte of the requested sector. psx-spx: "when trying to
    /// read further bytes, then the PSX will repeat the byte at index
    /// [800h-8] or [924h-4] as padding value".
    pub fn read_data(&mut self) -> u8 {
        if let Some(byte) = self.data.get(self.data_pos) {
            self.data_pos += 1;
            return *byte;
        }
        let padding = if self.data.len() == 0x800 { 0x800 - 8 } else { 0x924 - 4 };
        self.data.get(padding).copied().unwrap_or(0)
    }

    /// Whether RDDATA has bytes: DRQSTS, and the DMA3 data request.
    pub fn data_ready(&self) -> bool {
        self.data_pos < self.data.len()
    }

    /// Writes register 0-3.
    pub fn write(&mut self, register: u32, value: u8, irq: &mut Irq) {
        match (register & 3, self.bank) {
            (0, _) => self.bank = value & 3,
            // COMMAND. psx-spx: the controller "will proceed to drain the
            // parameter FIFO, process the command, push any return values
            // into the result FIFO and fire INT3".
            (1, 0) => {
                let delay = if matches!(value, 0x0A | 0x1E) { ACK_DELAY_INIT } else { ACK_DELAY };
                self.command = Some((value, delay));
            }
            // PARAMETER: "The FIFO can hold up to 16 bytes".
            (2, 0) => {
                if self.parameters.len() < 16 {
                    self.parameters.push_back(value);
                }
            }
            // HCHPCTL: bit 7 BFRD "Request sector buffer read".
            (3, 0) => {
                if value & 0x80 != 0 {
                    // "The Data Request accepts the data for the currently
                    // pending interrupt". Asking again while a sector is
                    // still being read changes nothing.
                    if self.data_pos >= self.data.len() {
                        if let Some(sector) = self.pending_sector.take() {
                            self.data = sector;
                            self.data_pos = 0;
                        }
                    }
                } else {
                    self.data.clear();
                    self.data_pos = 0;
                }
            }
            // HINTMSK.
            (2, 1) => {
                self.int_enable = value & 0x1F;
                self.update_irq(irq);
            }
            // HCLRCTL: acknowledge interrupts, clear the parameter FIFO.
            (3, 1) => {
                if value & 0x40 != 0 {
                    self.parameters.clear();
                }
                if value & 7 != 0 && self.int_flags != 0 {
                    self.int_flags &= !(value & 7);
                    if self.int_flags == 0 {
                        // "After acknowledge, the result FIFO is drained".
                        self.result_len = 0;
                        self.result_pos = 0;
                        self.result = [0; 16];
                        self.queue_delay = NEXT_INT_DELAY;
                    }
                }
            }
            // WRDATA, CI, ATV0-3, ADPCTL: sound map and audio volume, not
            // emulated.
            _ => {}
        }
    }

    /// psx-spx: HINTMSK — "The CD-ROM drive fires an interrupt whenever
    /// (HINTMSK & HINTSTS) is non-zero". IRQ2 is edge triggered, so this is
    /// called when a new interrupt is presented.
    fn update_irq(&mut self, irq: &mut Irq) {
        if self.int_flags & self.int_enable != 0 {
            irq.request(Interrupt::Cdrom);
        }
    }

    // ---- time --------------------------------------------------------------

    /// Advances the drive by `cycles` CPU cycles.
    pub fn tick(&mut self, cycles: u32, irq: &mut Irq) {
        // A command executes when its time has come and no interrupt is
        // pending. psx-spx: "Main CPU has sent a command, AND, there is no
        // INT pending".
        if let Some((command, left)) = self.command {
            let left = left.saturating_sub(cycles);
            if left == 0 && self.int_flags == 0 {
                self.command = None;
                self.execute(command);
            } else {
                self.command = Some((command, left));
            }
        }
        if let Some((second, left)) = self.second {
            let left = left.saturating_sub(cycles);
            if left == 0 {
                self.second = None;
                self.second_response(second);
            } else {
                self.second = Some((second, left));
            }
        }
        self.tick_drive(cycles);

        // Present the next response once the previous one is acknowledged.
        self.queue_delay = self.queue_delay.saturating_sub(cycles);
        if self.int_flags == 0 && self.queue_delay == 0 {
            if let Some(response) = self.queue.pop_front() {
                self.int_flags = response.int;
                self.result = [0; 16];
                self.result_len = response.bytes.len().min(16);
                self.result[..self.result_len].copy_from_slice(&response.bytes[..self.result_len]);
                self.result_pos = 0;
                if response.sector.is_some() {
                    self.pending_sector = response.sector;
                }
                self.update_irq(irq);
            }
        }
    }

    fn sector_cycles(&self) -> u32 {
        // "Read commands are continously receiving 75 sectors per second
        // (or 150 sectors at double speed)".
        if self.mode & MODE_DOUBLE_SPEED != 0 {
            SECTOR_CYCLES / 2
        } else {
            SECTOR_CYCLES
        }
    }

    fn tick_drive(&mut self, cycles: u32) {
        match self.drive {
            Drive::Seeking { then, left } if left <= cycles => {
                self.position = self.target.take().unwrap_or(self.position);
                self.drive = match then {
                    Then::Pause => {
                        // SeekL/SeekP: INT2 when the seek is done.
                        self.push(INT_COMPLETE, vec![self.stat_idle()], None);
                        Drive::Idle
                    }
                    Then::Read => Drive::Reading { left: self.sector_cycles() },
                    Then::Play => Drive::Playing { left: self.sector_cycles() },
                };
            }
            Drive::Seeking { then, left } => self.drive = Drive::Seeking { then, left: left - cycles },
            Drive::Reading { left } if left <= cycles => {
                self.drive = Drive::Reading { left: self.sector_cycles() };
                self.read_sector();
            }
            Drive::Reading { left } => self.drive = Drive::Reading { left: left - cycles },
            Drive::Playing { left } if left <= cycles => {
                self.drive = Drive::Playing { left: self.sector_cycles() };
                self.play_sector();
            }
            Drive::Playing { left } => self.drive = Drive::Playing { left: left - cycles },
            Drive::Stopped | Drive::Idle => {}
        }
    }

    // ---- responses ---------------------------------------------------------

    fn push(&mut self, int: u8, bytes: Vec<u8>, sector: Option<Vec<u8>>) {
        // psx-spx: "the SUB-CPU cannot hold more than one undelivered INT1":
        // a new sector replaces one that was not presented yet.
        if int == INT_DATA_READY {
            if let Some(waiting) = self.queue.iter_mut().find(|r| r.int == INT_DATA_READY) {
                *waiting = Response { int, bytes, sector };
                return;
            }
        }
        self.queue.push_back(Response { int, bytes, sector });
    }

    /// psx-spx: "Status code (stat)" — "only ONE of these bits can be set at
    /// a time (ie. Read/Play won't get set until after Seek completion)".
    fn stat(&self) -> u8 {
        match self.drive {
            Drive::Stopped => 0,
            Drive::Idle => STAT_MOTOR,
            Drive::Seeking { .. } => STAT_MOTOR | STAT_SEEK,
            Drive::Reading { .. } => STAT_MOTOR | STAT_READ,
            Drive::Playing { .. } => STAT_MOTOR | STAT_PLAY,
        }
    }

    fn stat_idle(&self) -> u8 {
        STAT_MOTOR
    }

    fn acknowledge(&mut self) {
        self.push(INT_ACKNOWLEDGE, vec![self.stat()], None);
    }

    /// "INT5 occurs, and an error-byte is send as second response byte",
    /// with stat bit 0 set.
    fn error(&mut self, code: u8) {
        self.push(INT_ERROR, vec![self.stat() | STAT_ERROR, code], None);
    }

    // ---- commands ----------------------------------------------------------

    /// psx-spx: "CDROM Controller Command Summary".
    fn execute(&mut self, command: u8) {
        let parameters: Vec<u8> = self.parameters.drain(..).collect();
        // (parameter count, needs a disc). psx-spx: error 80h "appears on
        // some commands (02h..09h, 0Bh..0Dh, 10h..16h, 1Ah, 1Bh?, and 1Dh)
        // when the disk is missing". GetID has its own no-disc answer.
        let (count, needs_disc): (Option<usize>, bool) = match command {
            0x01 | 0x0A | 0x0E | 0x0F | 0x19 | 0x1A | 0x1E => (None, false),
            0x02 => (Some(3), true),
            0x03 => (None, true),
            0x0D => (Some(2), true),
            0x12 | 0x14 => (Some(1), true),
            0x06..=0x09 | 0x0B | 0x0C | 0x10 | 0x11 | 0x13 | 0x15 | 0x16 | 0x1B => (Some(0), true),
            // "Unused: INT5: 0x11, 0x40" and everything not emulated.
            _ => return self.error(ERROR_INVALID_COMMAND),
        };
        let expected = count.or(match command {
            0x0E => Some(1),
            0x01 | 0x0A | 0x0F | 0x1A | 0x1E => Some(0),
            _ => None,
        });
        if expected.is_some_and(|n| n != parameters.len()) {
            return self.error(ERROR_PARAMETER_COUNT);
        }
        if needs_disc && self.disc.is_none() {
            return self.error(ERROR_NOT_READY);
        }

        match command {
            // Nop.
            0x01 => self.acknowledge(),
            // Setloc: "All three parameters must be valid packed BCD, with
            // ass < 60h and asect < 75h; invalid or out-of-range values
            // return INT5(stat,10h)".
            0x02 => {
                let decoded = (from_bcd(parameters[0]), from_bcd(parameters[1]), from_bcd(parameters[2]));
                match decoded {
                    (Some(minute), Some(second), Some(frame)) if second < 60 && frame < 75 => {
                        // Positions inside the lead-in are clamped to the
                        // first sector.
                        self.target = Some(Msf { minute, second, frame }.to_lba().unwrap_or(0));
                        self.acknowledge();
                    }
                    _ => self.error(ERROR_INVALID_PARAMETER),
                }
            }
            // Play, with an optional track number.
            0x03 => {
                if let Some(track) = parameters.first().and_then(|t| from_bcd(*t)).filter(|t| *t != 0) {
                    let disc = self.disc.as_ref().expect("checked above");
                    let start = disc.tracks().iter().find(|t| t.number == track).map(|t| t.start);
                    self.target = Some(start.unwrap_or(disc.tracks()[0].start));
                }
                self.acknowledge();
                self.start(Then::Play);
            }
            // ReadN / ReadS.
            0x06 | 0x1B => {
                self.acknowledge();
                self.start(Then::Read);
            }
            // MotorOn: "works ONLY if the motor was off (otherwise fails
            // with INT5(stat,20h))".
            0x07 => {
                if self.drive == Drive::Stopped {
                    self.acknowledge();
                    self.drive = Drive::Idle;
                    self.second = Some((Second::Complete, ACK_DELAY));
                } else {
                    self.error(ERROR_PARAMETER_COUNT);
                }
            }
            // Stop: "The first response returns the current status (this
            // already with bit5 cleared), the second response returns the
            // new status (with bit1 cleared)".
            0x08 => {
                let was_stopped = self.drive == Drive::Stopped;
                if !was_stopped {
                    self.drive = Drive::Idle;
                }
                self.acknowledge();
                self.second = Some((Second::Stop, if was_stopped { STOP_IDLE_DELAY } else { STOP_DELAY }));
            }
            // Pause: "The first response returns the current status (still
            // with bit5 set if a Read command was active), the second
            // response returns the new status (with bit5 cleared)".
            0x09 => {
                let active = matches!(self.drive, Drive::Reading { .. } | Drive::Playing { .. } | Drive::Seeking { .. });
                self.acknowledge();
                // "Pause (single speed) 021181ch ... time equal to about 5
                // sectors"; half that at double speed.
                let delay = if active { 5 * self.sector_cycles() } else { PAUSE_IDLE_DELAY };
                self.second = Some((Second::Pause, delay));
            }
            // Init: "Sets mode=20h, activates drive motor, Standby, abort
            // all commands."
            0x0A => {
                self.mode = MODE_WHOLE_SECTOR;
                self.second = None;
                self.queue.clear();
                self.target = None;
                if self.disc.is_some() {
                    self.drive = Drive::Idle;
                }
                self.acknowledge();
                self.second = Some((Second::Complete, ACK_DELAY_INIT));
            }
            0x0B | 0x0C => {
                self.muted = command == 0x0B;
                self.acknowledge();
            }
            // Setfilter.
            0x0D => {
                self.filter_file = parameters[0];
                self.filter_channel = parameters[1];
                self.acknowledge();
            }
            // Setmode.
            0x0E => {
                self.mode = parameters[0];
                self.acknowledge();
            }
            // Getparam: "INT3(stat,mode,null,file,channel)".
            0x0F => {
                let bytes = vec![self.stat(), self.mode, 0, self.filter_file, self.filter_channel];
                self.push(INT_ACKNOWLEDGE, bytes, None);
            }
            // GetlocL: "4-byte sector header, plus 4-byte subheader of the
            // current sector"; fails with 80h during a seek or before any
            // data sector was read.
            0x10 => match (self.last_header, self.drive) {
                (Some(header), drive) if !matches!(drive, Drive::Seeking { .. }) => {
                    self.push(INT_ACKNOWLEDGE, header.to_vec(), None);
                }
                _ => self.error(ERROR_NOT_READY),
            },
            // GetlocP: "INT3(track,index,mm,ss,sect,amm,ass,asect)", BCD.
            0x11 => {
                let bytes = self.subchannel_position().to_vec();
                self.push(INT_ACKNOWLEDGE, bytes, None);
            }
            // SetSession: "session = 00h causes error code 10h"; a single
            // session exists, so 1 passes and anything else is a seek error.
            0x12 => match parameters[0] {
                0 => self.error(ERROR_INVALID_PARAMETER),
                1 => {
                    self.acknowledge();
                    self.second = Some((Second::Complete, ACK_DELAY));
                }
                _ => {
                    self.acknowledge();
                    self.push(INT_ERROR, vec![self.stat() | 0x04, ERROR_INVALID_COMMAND], None);
                }
            },
            // GetTN: "INT3(stat,first,last) ;BCD".
            0x13 => {
                let tracks = self.disc.as_ref().expect("checked above").tracks();
                let (first, last) = (tracks[0].number, tracks[tracks.len() - 1].number);
                let bytes = vec![self.stat(), crate::disc::to_bcd(first), crate::disc::to_bcd(last)];
                self.push(INT_ACKNOWLEDGE, bytes, None);
            }
            // GetTD: "INT3(stat,mm,ss) ;BCD"; track 0 is "the end of the
            // last track"; unknown or non-BCD tracks give error 10h.
            0x14 => {
                let disc = self.disc.as_ref().expect("checked above");
                let start = match from_bcd(parameters[0]) {
                    Some(0) => Some(disc.sector_count()),
                    Some(n) => disc.tracks().iter().find(|t| t.number == n).map(|t| t.start),
                    None => None,
                };
                match start {
                    Some(lba) => {
                        let msf = Msf::from_lba(lba).bcd();
                        self.push(INT_ACKNOWLEDGE, vec![self.stat(), msf[0], msf[1]], None);
                    }
                    None => self.error(ERROR_INVALID_PARAMETER),
                }
            }
            // SeekL / SeekP.
            0x15 | 0x16 => {
                self.acknowledge();
                self.start(Then::Pause);
            }
            0x19 => self.test(&parameters),
            // GetID.
            0x1A => {
                self.acknowledge();
                self.second = Some((Second::GetId, GETID_DELAY));
            }
            // ReadTOC.
            0x1E => {
                self.acknowledge();
                self.second = Some((Second::Complete, TOC_DELAY));
            }
            _ => unreachable!("filtered above"),
        }
    }

    /// Starts reading, playing or a plain seek. psx-spx: "Setloc, Read,
    /// Pause" — "If Read is issued with an unprocessed Setloc, then the
    /// drive is automatically seeking the Setloc location"; otherwise it
    /// continues, or resumes, where it is.
    fn start(&mut self, then: Then) {
        if self.target.is_some() || then == Then::Pause {
            self.drive = Drive::Seeking { then, left: SEEK_SECTORS * self.sector_cycles() };
            return;
        }
        let left = self.sector_cycles();
        self.drive = match (then, self.drive) {
            (Then::Read, Drive::Reading { .. }) | (Then::Play, Drive::Playing { .. }) => self.drive,
            (Then::Read, _) => Drive::Reading { left },
            _ => Drive::Playing { left },
        };
    }

    /// Test commands. psx-spx: "CDROM - Test Commands".
    fn test(&mut self, parameters: &[u8]) {
        match parameters {
            // "19h,20h --> INT3(yy,mm,dd,ver)": the controller version.
            // Reported as vC1 (a), "16 May 1995", the first version with
            // ReadTOC; which one an SCPH-1001 really has is not checked.
            [0x20] => self.push(INT_ACKNOWLEDGE, vec![0x95, 0x05, 0x16, 0xC1], None),
            [] => self.error(ERROR_PARAMETER_COUNT),
            // "10h - Invalid Sub_function (for command 19h)".
            _ => self.error(ERROR_INVALID_PARAMETER),
        }
    }

    fn second_response(&mut self, second: Second) {
        match second {
            Second::Complete => self.push(INT_COMPLETE, vec![self.stat()], None),
            Second::Pause => {
                if self.drive != Drive::Stopped {
                    self.drive = Drive::Idle;
                }
                self.push(INT_COMPLETE, vec![self.stat()], None);
            }
            Second::Stop => {
                self.drive = Drive::Stopped;
                self.push(INT_COMPLETE, vec![self.stat()], None);
            }
            // psx-spx: "GetID" response table.
            Second::GetId => match &self.disc {
                // "No Disk: INT5(08h,40h, 00h,00h, 00h,00h,00h,00h)".
                None => self.push(INT_ERROR, vec![STAT_ID_ERROR, 0x40, 0, 0, 0, 0, 0, 0], None),
                Some(disc) => match disc.region() {
                    // "Licensed:Mode2: INT2(02h,00h, 20h,00h, 53h,43h,45h,4xh)".
                    Some(region) => {
                        let bytes = vec![self.stat(), 0x00, 0x20, 0x00, b'S', b'C', b'E', region];
                        self.push(INT_COMPLETE, bytes, None);
                    }
                    // "Audio Disk: INT5(0Ah,90h, ...)" /
                    // "Unlicensed:Mode2: INT5(0Ah,80h, 20h,00h, ...)".
                    None => {
                        let audio = disc.tracks()[0].audio;
                        let flags = if audio { 0x90 } else { 0x80 };
                        let kind = if audio { 0x00 } else { 0x20 };
                        let bytes = vec![self.stat() | STAT_ID_ERROR, flags, kind, 0, 0, 0, 0, 0];
                        self.push(INT_ERROR, bytes, None);
                    }
                },
            },
        }
    }

    // ---- sectors -----------------------------------------------------------

    /// The 8 position bytes of GetlocP for the sector under the head.
    fn subchannel_position(&self) -> [u8; 8] {
        let Some(disc) = &self.disc else { return [0; 8] };
        let track = disc.track_at(self.position);
        let relative = Msf::from_sectors(self.position.saturating_sub(track.start)).bcd();
        let absolute = Msf::from_lba(self.position).bcd();
        let number = crate::disc::to_bcd(track.number);
        [number, 0x01, relative[0], relative[1], relative[2], absolute[0], absolute[1], absolute[2]]
    }

    /// One sector has passed under the head while reading.
    fn read_sector(&mut self) {
        let Some(raw) = self.disc.as_ref().and_then(|d| d.sector(self.position)).map(<[u8]>::to_vec) else {
            // Past the end of the disc: "INT4 DataEnd — Reached end of disc".
            self.drive = Drive::Idle;
            self.push(INT_DATA_END, vec![self.stat()], None);
            return;
        };
        self.position += 1;
        let mut header = [0u8; 8];
        header.copy_from_slice(&raw[12..20]);
        self.last_header = Some(header);

        // psx-spx: "Data/ADPCM Sector Filtering/Delivery". Sectors that go
        // to the XA-ADPCM decoder are not delivered as data; the decoder
        // itself is not emulated, so they are dropped.
        let (file, channel, submode) = (raw[16], raw[17], raw[18]);
        let realtime_audio = raw[15] == 2 && submode & 0x44 == 0x44;
        let filter = self.mode & MODE_XA_FILTER != 0;
        let filter_match = file == self.filter_file && channel == self.filter_channel;
        if realtime_audio && self.mode & MODE_XA_ADPCM != 0 && (!filter || filter_match) {
            return;
        }
        if filter && realtime_audio {
            return;
        }
        // Setmode bit 5: "Sector Size (0=800h=DataOnly, 1=924h=Whole Sector
        // Except Sync Bytes)".
        let data = if self.mode & MODE_WHOLE_SECTOR != 0 { raw[12..12 + 0x924].to_vec() } else { raw[24..24 + 0x800].to_vec() };
        self.push(INT_DATA_READY, vec![self.stat()], Some(data));
    }

    /// One sector has passed under the head while playing audio.
    fn play_sector(&mut self) {
        let Some(disc) = &self.disc else { return };
        let track = *disc.track_at(self.position);
        let next = self.position + 1;
        let end_of_disc = next >= disc.sector_count();
        let end_of_track = disc.track_at(next.min(disc.sector_count() - 1)).number != track.number;
        // psx-spx: "AutoPause --> INT4(stat)" at the end of the track when
        // Setmode bit 1 is set; the end of the disc ends playing anyway.
        if end_of_disc || (end_of_track && self.mode & MODE_AUTO_PAUSE != 0) {
            self.drive = Drive::Idle;
            self.push(INT_DATA_END, vec![self.stat()], None);
            return;
        }
        // "Report --> INT1(stat,track,index,mm/amm,ss+80h/ass,sect/asect,
        // peaklo,peakhi)", sent 7.5 times a second: absolute time when the
        // sector number ends in 0, 20, 40, 60, relative time (seconds with
        // bit 7 set) at 10, 30, 50, 70. The peak is not computed.
        let absolute = Msf::from_lba(self.position);
        if self.mode & MODE_REPORT != 0 && absolute.frame.is_multiple_of(10) {
            let position = self.subchannel_position();
            let bytes = if absolute.frame.is_multiple_of(20) {
                vec![self.stat(), position[0], position[1], position[5], position[6], position[7], 0, 0]
            } else {
                vec![self.stat(), position[0], position[1], position[2], position[3] | 0x80, position[4], 0, 0]
            };
            self.push(INT_DATA_READY, bytes, None);
        }
        self.position = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disc::tests::data_track;
    use crate::disc::SECTOR_SIZE;

    fn licensed_disc() -> Disc {
        let mut data = data_track(400);
        let text = b"          Licensed  by          Sony Computer Entertainment Amer  ica ";
        data[4 * SECTOR_SIZE + 24..][..text.len()].copy_from_slice(text);
        Disc::from_data_track(data).unwrap()
    }

    struct Rig {
        cd: Cdrom,
        irq: Irq,
    }

    impl Rig {
        fn new(disc: Option<Disc>) -> Self {
            let mut rig = Self { cd: Cdrom::new(disc), irq: Irq::default() };
            // Enable all interrupts: bank 1, HINTMSK = 1Fh.
            rig.cd.write(0, 1, &mut rig.irq);
            rig.cd.write(2, 0x1F, &mut rig.irq);
            rig
        }

        fn command(&mut self, command: u8, parameters: &[u8]) {
            self.cd.write(0, 0, &mut self.irq);
            for p in parameters {
                self.cd.write(2, *p, &mut self.irq);
            }
            self.cd.write(1, command, &mut self.irq);
        }

        fn int(&mut self) -> u8 {
            self.cd.write(0, 1, &mut self.irq);
            self.cd.read(3) & 7
        }

        /// Runs until an interrupt is presented (or `limit` cycles pass);
        /// returns its number and result bytes, and acknowledges it.
        fn wait(&mut self, limit: u32) -> Option<(u8, Vec<u8>)> {
            let mut elapsed = 0;
            while self.int() == 0 {
                if elapsed >= limit {
                    return None;
                }
                self.cd.tick(100, &mut self.irq);
                elapsed += 100;
            }
            let int = self.int();
            let mut bytes = Vec::new();
            while self.cd.read(0) & 0x20 != 0 {
                bytes.push(self.cd.read(1));
            }
            self.cd.write(0, 1, &mut self.irq);
            self.cd.write(3, 0x07, &mut self.irq);
            Some((int, bytes))
        }

        fn run(&mut self, command: u8, parameters: &[u8]) -> (u8, Vec<u8>) {
            self.command(command, parameters);
            self.wait(1_000_000).expect("first response")
        }
    }

    // ---- registers ---------------------------------------------------------

    #[test]
    fn status_register_and_banks() {
        // psx-spx: HSTS — bit 3 parameter FIFO empty, bit 4 not full.
        let mut rig = Rig::new(None);
        rig.cd.write(0, 2, &mut rig.irq);
        assert_eq!(rig.cd.read(0), 0x18 | 2);
        rig.cd.write(0, 0, &mut rig.irq);
        rig.cd.write(2, 0x55, &mut rig.irq);
        assert_eq!(rig.cd.read(0), 0x10, "a parameter is waiting");
        // HINTMSK reads back in banks 0/2, HINTSTS in banks 1/3; bits 5-7 set.
        assert_eq!(rig.cd.read(3), 0xE0 | 0x1F);
        rig.cd.write(0, 1, &mut rig.irq);
        assert_eq!(rig.cd.read(3), 0xE0);
        // HCLRCTL bit 6 clears the parameter FIFO.
        rig.cd.write(3, 0x40, &mut rig.irq);
        assert_eq!(rig.cd.read(0) & 0x08, 0x08);
    }

    #[test]
    fn nop_is_acknowledged_after_a_delay_with_an_interrupt() {
        // psx-spx: "Nop - Command 01h --> INT3(stat)"; first response after
        // about C4E1h cycles; busy flag (HSTS bit 7) until then.
        let mut rig = Rig::new(Some(licensed_disc()));
        rig.command(0x01, &[]);
        assert_ne!(rig.cd.read(0) & 0x80, 0, "busy");
        rig.cd.tick(ACK_DELAY - 1, &mut rig.irq);
        assert_eq!(rig.int(), 0);
        assert_eq!(rig.irq.stat(), 0);
        rig.cd.tick(1, &mut rig.irq);
        assert_eq!(rig.int(), 3);
        assert_eq!(rig.irq.stat(), 1 << 2, "IRQ2");
        assert_eq!(rig.cd.read(0) & 0xA0, 0x20, "not busy, result ready");
        assert_eq!(rig.cd.read(1), STAT_MOTOR);
        assert_eq!(rig.cd.read(0) & 0x20, 0, "result FIFO empty");
        // Acknowledge.
        rig.cd.write(3, 0x07, &mut rig.irq);
        assert_eq!(rig.int(), 0);
    }

    #[test]
    fn interrupt_needs_its_mask_bit() {
        let mut rig = Rig::new(None);
        rig.cd.write(0, 1, &mut rig.irq);
        rig.cd.write(2, 0, &mut rig.irq);
        rig.command(0x01, &[]);
        rig.cd.tick(ACK_DELAY, &mut rig.irq);
        assert_eq!(rig.int(), 3);
        assert_eq!(rig.irq.stat(), 0);
    }

    #[test]
    fn unknown_command_and_wrong_parameter_count_are_errors() {
        // psx-spx: "INT5: 0x11, 0x40" style errors; "20h - Wrong number of
        // parameters".
        let mut rig = Rig::new(Some(licensed_disc()));
        assert_eq!(rig.run(0x00, &[]), (5, vec![STAT_MOTOR | 1, 0x40]));
        assert_eq!(rig.run(0x58, &[]), (5, vec![STAT_MOTOR | 1, 0x40]));
        assert_eq!(rig.run(0x01, &[1]), (5, vec![STAT_MOTOR | 1, 0x20]));
        assert_eq!(rig.run(0x02, &[0, 2]), (5, vec![STAT_MOTOR | 1, 0x20]));
    }

    #[test]
    fn controller_version() {
        let mut rig = Rig::new(None);
        assert_eq!(rig.run(0x19, &[0x20]), (3, vec![0x95, 0x05, 0x16, 0xC1]));
        assert_eq!(rig.run(0x19, &[0x99]), (5, vec![1, 0x10]));
    }

    // ---- without a disc ----------------------------------------------------

    #[test]
    fn no_disc_answers() {
        // psx-spx: error 80h "appears if no disk inserted at all"; GetID
        // "No Disk: INT3(stat), INT5(08h,40h, 00h,00h, 00h,00h,00h,00h)".
        let mut rig = Rig::new(None);
        assert_eq!(rig.run(0x01, &[]), (3, vec![0x00]), "motor off");
        assert_eq!(rig.run(0x06, &[]), (5, vec![0x01, 0x80]));
        assert_eq!(rig.run(0x13, &[]), (5, vec![0x01, 0x80]));
        assert_eq!(rig.run(0x1A, &[]), (3, vec![0x00]));
        assert_eq!(rig.wait(1_000_000), Some((5, vec![0x08, 0x40, 0, 0, 0, 0, 0, 0])));
    }

    // ---- with a disc -------------------------------------------------------

    #[test]
    fn getid_of_a_licensed_disc() {
        // "Licensed:Mode2: INT3(stat), INT2(02h,00h, 20h,00h, 53h,43h,45h,4xh)".
        let mut rig = Rig::new(Some(licensed_disc()));
        assert_eq!(rig.run(0x1A, &[]), (3, vec![0x02]));
        assert_eq!(rig.wait(1_000_000), Some((2, vec![0x02, 0x00, 0x20, 0x00, b'S', b'C', b'E', b'A'])));
        // "Unlicensed:Mode2: INT5(0Ah,80h, 20h,00h, 00h,00h,00h,00h)".
        let mut rig = Rig::new(Some(Disc::from_data_track(data_track(20)).unwrap()));
        rig.run(0x1A, &[]);
        assert_eq!(rig.wait(1_000_000), Some((5, vec![0x0A, 0x80, 0x20, 0, 0, 0, 0, 0])));
    }

    #[test]
    fn init_sets_mode_and_completes() {
        // psx-spx: "Init - Command 0Ah --> INT3(stat) --> INT2(stat)",
        // "Sets mode=20h".
        let mut rig = Rig::new(Some(licensed_disc()));
        rig.run(0x0E, &[0x80]);
        assert_eq!(rig.run(0x0A, &[]), (3, vec![0x02]));
        assert_eq!(rig.wait(1_000_000), Some((2, vec![0x02])));
        assert_eq!(rig.run(0x0F, &[]), (3, vec![0x02, 0x20, 0x00, 0x00, 0x00]), "Getparam");
    }

    #[test]
    fn setloc_validates_bcd() {
        let mut rig = Rig::new(Some(licensed_disc()));
        assert_eq!(rig.run(0x02, &[0x00, 0x02, 0x16]).0, 3);
        for bad in [[0x0A, 0x02, 0x00], [0x00, 0x60, 0x00], [0x00, 0x02, 0x75]] {
            assert_eq!(rig.run(0x02, &bad), (5, vec![0x03, 0x10]), "{bad:02X?}");
        }
    }

    #[test]
    fn toc_commands() {
        // psx-spx: "GetTN", "GetTD".
        let cue = "FILE a BINARY\nTRACK 01 MODE2/2352\nINDEX 01 00:00:00\nTRACK 02 AUDIO\nINDEX 01 00:04:00\n";
        let disc = Disc::from_cue(cue, |_| Ok(data_track(6000))).unwrap();
        let mut rig = Rig::new(Some(disc));
        assert_eq!(rig.run(0x13, &[]), (3, vec![0x02, 0x01, 0x02]));
        assert_eq!(rig.run(0x14, &[0x01]), (3, vec![0x02, 0x00, 0x02]));
        assert_eq!(rig.run(0x14, &[0x02]), (3, vec![0x02, 0x00, 0x06]));
        // Track 0: end of the last track = 6000 sectors + 2 s = 01:22.
        assert_eq!(rig.run(0x14, &[0x00]), (3, vec![0x02, 0x01, 0x22]));
        assert_eq!(rig.run(0x14, &[0x03]), (5, vec![0x03, 0x10]));
        assert_eq!(rig.run(0x14, &[0x0A]), (5, vec![0x03, 0x10]));
    }

    /// Setloc to `lba`, set the mode, start reading; returns after INT3.
    fn start_read(rig: &mut Rig, lba: u32, mode: u8) {
        let msf = Msf::from_lba(lba).bcd();
        rig.run(0x0E, &[mode]);
        rig.run(0x02, &msf);
        assert_eq!(rig.run(0x06, &[]), (3, vec![STAT_MOTOR]));
    }

    fn read_bytes(rig: &mut Rig, count: usize) -> Vec<u8> {
        // As software does: drop the old buffer, then request the new one.
        rig.cd.write(0, 0, &mut rig.irq);
        rig.cd.write(3, 0x00, &mut rig.irq);
        rig.cd.write(3, 0x80, &mut rig.irq);
        (0..count).map(|_| rig.cd.read(2)).collect()
    }

    #[test]
    fn read_delivers_sectors_in_order_at_the_sector_rate() {
        // psx-spx: "ReadN - Command 06h --> INT3(stat) --> INT1(stat) -->
        // datablock"; 75 sectors per second.
        let mut rig = Rig::new(Some(licensed_disc()));
        start_read(&mut rig, 20, 0x00);
        // The seek is in progress: stat shows Seek, not Read.
        assert_eq!(rig.run(0x01, &[]), (3, vec![STAT_MOTOR | STAT_SEEK]));
        let (int, bytes) = rig.wait(10_000_000).unwrap();
        assert_eq!((int, bytes), (1, vec![STAT_MOTOR | STAT_READ]));
        let data = read_bytes(&mut rig, 0x800);
        assert!(data.iter().all(|b| *b == 20), "the 2048 data bytes of sector 20");
        assert_eq!(rig.cd.read(0) & 0x40, 0, "data FIFO empty");

        // The next sector follows one sector time later.
        assert_eq!(rig.wait(SECTOR_CYCLES - 10_000), None);
        assert_eq!(rig.wait(20_000).map(|r| r.0), Some(1));
        assert_eq!(read_bytes(&mut rig, 1), [21]);
    }

    #[test]
    fn whole_sector_mode_and_double_speed() {
        // Setmode bit 5: 924h bytes starting at the header; bit 7: 150/s.
        let mut rig = Rig::new(Some(licensed_disc()));
        start_read(&mut rig, 30, 0xA0);
        rig.wait(10_000_000).unwrap();
        let data = read_bytes(&mut rig, 0x924);
        assert_eq!(&data[..4], &[0x00, 0x02, 0x30, 0x02], "header: 00:02:30, mode 2");
        assert_eq!(data[12], 30, "data starts after header and subheader");
        // "repeat the byte at index [924h-4] as padding value".
        assert_eq!(rig.cd.read(2), data[0x920]);
        assert_eq!(rig.wait(SECTOR_CYCLES / 2 + 10_000).map(|r| r.0), Some(1));
    }

    #[test]
    fn getloc_reports_header_and_position() {
        // psx-spx: "GetlocL", "GetlocP".
        let mut rig = Rig::new(Some(licensed_disc()));
        assert_eq!(rig.run(0x10, &[]), (5, vec![0x03, 0x80]), "nothing read yet");
        start_read(&mut rig, 80, 0x00);
        rig.wait(10_000_000).unwrap();
        // Header of sector 80 = 00:03:05 mode 2, subheader 00 00 08 00.
        assert_eq!(rig.run(0x10, &[]), (3, vec![0x00, 0x03, 0x05, 0x02, 0x00, 0x00, 0x08, 0x00]));
        // Position: track 1 index 1, relative 00:01:06, absolute 00:03:06
        // (the head is now at sector 81).
        assert_eq!(rig.run(0x11, &[]), (3, vec![0x01, 0x01, 0x00, 0x01, 0x06, 0x00, 0x03, 0x06]));
    }

    #[test]
    fn pause_stops_the_read() {
        // psx-spx: "Pause": first response with bit 5 still set, second
        // with it cleared.
        let mut rig = Rig::new(Some(licensed_disc()));
        start_read(&mut rig, 10, 0x80);
        rig.wait(10_000_000).unwrap();
        assert_eq!(rig.run(0x09, &[]), (3, vec![STAT_MOTOR | STAT_READ]));
        // Sectors may still arrive before the pause completes.
        let mut last = None;
        while let Some(response) = rig.wait(3_000_000) {
            last = Some(response);
        }
        assert_eq!(last, Some((2, vec![STAT_MOTOR])));
        assert_eq!(rig.wait(2_000_000), None, "no more sectors");
        // Pausing again answers quickly.
        rig.run(0x09, &[]);
        assert_eq!(rig.wait(PAUSE_IDLE_DELAY + 5000), Some((2, vec![STAT_MOTOR])));
    }

    #[test]
    fn reading_past_the_end_reports_data_end() {
        let mut rig = Rig::new(Some(licensed_disc()));
        start_read(&mut rig, 399, 0x80);
        assert_eq!(rig.wait(10_000_000).map(|r| r.0), Some(1));
        assert_eq!(rig.wait(10_000_000), Some((4, vec![STAT_MOTOR])));
    }

    #[test]
    fn seek_completes_with_int2_and_stop_turns_the_motor_off() {
        let mut rig = Rig::new(Some(licensed_disc()));
        rig.run(0x02, &[0x00, 0x03, 0x00]);
        assert_eq!(rig.run(0x15, &[]), (3, vec![STAT_MOTOR]));
        assert_eq!(rig.wait(10_000_000), Some((2, vec![STAT_MOTOR])));
        assert_eq!(rig.run(0x11, &[]).1[5..], [0x00, 0x03, 0x00]);
        // Stop, then MotorOn.
        assert_eq!(rig.run(0x08, &[]), (3, vec![STAT_MOTOR]));
        assert_eq!(rig.wait(20_000_000), Some((2, vec![0x00])));
        assert_eq!(rig.run(0x07, &[]), (3, vec![0x00]));
        assert_eq!(rig.wait(1_000_000), Some((2, vec![STAT_MOTOR])));
        // "works ONLY if the motor was off (otherwise fails with INT5(stat,20h))".
        assert_eq!(rig.run(0x07, &[]), (5, vec![STAT_MOTOR | 1, 0x20]));
    }

    #[test]
    fn responses_wait_for_the_previous_one_to_be_acknowledged() {
        // psx-spx: "INT3 is delivered first, and INT5 is not delivered until
        // INT3 is acknowledged".
        let mut rig = Rig::new(Some(licensed_disc()));
        rig.command(0x1A, &[]);
        rig.cd.tick(ACK_DELAY + GETID_DELAY + 100_000, &mut rig.irq);
        assert_eq!(rig.int(), 3, "the second response is held back");
        rig.cd.write(3, 0x07, &mut rig.irq);
        rig.cd.tick(NEXT_INT_DELAY, &mut rig.irq);
        assert_eq!(rig.int(), 2);
    }

    #[test]
    fn xa_audio_sectors_are_not_delivered_as_data() {
        // psx-spx: "Data/ADPCM Sector Filtering/Delivery".
        let mut data = data_track(60);
        // Sector 31: submode audio + realtime (bits 2 and 6).
        data[31 * SECTOR_SIZE + 18] = 0x64;
        let mut rig = Rig::new(Some(Disc::from_data_track(data).unwrap()));
        start_read(&mut rig, 30, 0x80 | 0x40);
        rig.wait(10_000_000).unwrap();
        assert_eq!(read_bytes(&mut rig, 1), [30]);
        rig.wait(10_000_000).unwrap();
        assert_eq!(read_bytes(&mut rig, 1), [32], "sector 31 went to the audio decoder");
    }

    #[test]
    fn play_moves_the_position_and_reports() {
        // psx-spx: "Play", "Report".
        let cue = "FILE a BINARY\nTRACK 01 MODE2/2352\nINDEX 01 00:00:00\nTRACK 02 AUDIO\nINDEX 01 00:04:00\n";
        let disc = Disc::from_cue(cue, |_| Ok(data_track(900))).unwrap();
        let mut rig = Rig::new(Some(disc));
        rig.run(0x0E, &[0x04]); // report
        assert_eq!(rig.run(0x03, &[0x02]), (3, vec![STAT_MOTOR]));
        // Track 2 starts at 00:06:00: the first report is absolute.
        let (int, bytes) = rig.wait(20_000_000).unwrap();
        assert_eq!(int, 1);
        assert_eq!(bytes, vec![STAT_MOTOR | STAT_PLAY, 0x02, 0x01, 0x00, 0x06, 0x00, 0, 0]);
        // Ten sectors later a relative one, seconds with bit 7 set.
        let (_, bytes) = rig.wait(20_000_000).unwrap();
        assert_eq!(bytes, vec![STAT_MOTOR | STAT_PLAY, 0x02, 0x01, 0x00, 0x80, 0x10, 0, 0]);
    }
}
