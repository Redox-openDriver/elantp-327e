use crate::hw::{Clock, Registers};

pub const CON: usize = 0x00;
pub const TAR: usize = 0x04;
pub const DATA_CMD: usize = 0x10;
pub const SS_SCL_HCNT: usize = 0x14;
pub const SS_SCL_LCNT: usize = 0x18;
pub const FS_SCL_HCNT: usize = 0x1c;
pub const FS_SCL_LCNT: usize = 0x20;
pub const INTR_MASK: usize = 0x30;
pub const RAW_INTR_STAT: usize = 0x34;
pub const RX_TL: usize = 0x38;
pub const TX_TL: usize = 0x3c;
pub const CLR_INTR: usize = 0x40;
pub const CLR_TX_ABRT: usize = 0x54;
pub const CLR_STOP_DET: usize = 0x60;
pub const ENABLE: usize = 0x6c;
pub const STATUS: usize = 0x70;
pub const TX_ABRT_SOURCE: usize = 0x80;
pub const ENABLE_STATUS: usize = 0x9c;
pub const COMP_PARAM_1: usize = 0xf4;
pub const COMP_TYPE: usize = 0xfc;

pub const DESIGNWARE: u32 = 0x4457_0140;
pub const CON_MASTER: u32 = 1;
pub const CON_FAST: u32 = 2 << 1;
pub const CON_RESTART: u32 = 1 << 5;
pub const CON_SLAVE_DISABLE: u32 = 1 << 6;
pub const CMD_READ: u32 = 1 << 8;
pub const CMD_STOP: u32 = 1 << 9;
pub const CMD_RESTART: u32 = 1 << 10;
pub const INTR_TX_ABRT: u32 = 1 << 6;
pub const INTR_STOP_DET: u32 = 1 << 9;
pub const STATUS_TFNF: u32 = 1 << 1;
pub const STATUS_RFNE: u32 = 1 << 3;
pub const ABRT_NOACK: u32 = 0x1f;

const MAX_TRANSFER: usize = 4096 + 64;
const FIFO_FLOOR: usize = 2;
const FIFO_CEILING: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Disable,
    Transfer,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum I2cError {
    NotDesignware(u32),
    BadAddress(u16),
    TooLong(usize),
    Timeout(Step),
    Nack(u32),
    Abort(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub high: u32,
    pub low: u32,
}

fn cycles(clock_hz: u32, nanoseconds: u32) -> u32 {
    (u64::from(clock_hz) * u64::from(nanoseconds)).div_ceil(1_000_000_000) as u32
}

pub fn timing(clock_hz: u32, bus_hz: u32) -> Timing {
    let (low_ns, high_ns) = if bus_hz > 100_000 { (1_600, 900) } else { (5_000, 4_300) };
    let low = cycles(clock_hz, low_ns).max(8) - 1;
    let high = cycles(clock_hz, high_ns).max(16) - 8;
    let period = clock_hz.div_ceil(bus_hz.max(1));
    let low = low + period.saturating_sub(low + 1 + high + 8);
    Timing { high, low }
}

pub struct Master<R, C> {
    registers: R,
    clock: C,
    depth: usize,
    budget: u64,
}

impl<R: Registers, C: Clock> Master<R, C> {
    pub fn new(mut registers: R, clock: C, clock_hz: u32, bus_hz: u32, budget: u64) -> Result<Self, I2cError> {
        let kind = registers.read(COMP_TYPE);
        if kind != DESIGNWARE {
            return Err(I2cError::NotDesignware(kind));
        }
        let parameters = registers.read(COMP_PARAM_1);
        let rx = ((parameters >> 8) & 0xff) as usize + 1;
        let tx = ((parameters >> 16) & 0xff) as usize + 1;
        let depth = rx.min(tx).clamp(FIFO_FLOOR, FIFO_CEILING);
        let mut master = Master { registers, clock, depth, budget };
        master.disable()?;
        let fast = timing(clock_hz, bus_hz);
        let standard = timing(clock_hz, 100_000);
        master.registers.write(CON, CON_MASTER | CON_FAST | CON_RESTART | CON_SLAVE_DISABLE);
        master.registers.write(FS_SCL_HCNT, fast.high);
        master.registers.write(FS_SCL_LCNT, fast.low);
        master.registers.write(SS_SCL_HCNT, standard.high);
        master.registers.write(SS_SCL_LCNT, standard.low);
        master.registers.write(TX_TL, 0);
        master.registers.write(RX_TL, 0);
        master.registers.write(INTR_MASK, 0);
        Ok(master)
    }

    pub fn into_parts(self) -> (R, C) {
        (self.registers, self.clock)
    }

    fn disable(&mut self) -> Result<(), I2cError> {
        self.registers.write(ENABLE, 0);
        let start = self.clock.micros();
        while self.registers.read(ENABLE_STATUS) & 1 != 0 {
            if self.clock.micros().saturating_sub(start) > self.budget {
                return Err(I2cError::Timeout(Step::Disable));
            }
        }
        Ok(())
    }

    fn abort(&mut self) -> I2cError {
        let source = self.registers.read(TX_ABRT_SOURCE);
        self.registers.read(CLR_TX_ABRT);
        let _ = self.disable();
        if source & ABRT_NOACK != 0 { I2cError::Nack(source) } else { I2cError::Abort(source) }
    }

    pub fn transfer(&mut self, address: u16, write: &[u8], read: &mut [u8]) -> Result<(), I2cError> {
        if address > 0x7f {
            return Err(I2cError::BadAddress(address));
        }
        let total = write.len() + read.len();
        if total == 0 {
            return Ok(());
        }
        if total > MAX_TRANSFER {
            return Err(I2cError::TooLong(total));
        }
        self.disable()?;
        self.registers.write(TAR, u32::from(address));
        self.registers.write(ENABLE, 1);
        self.registers.read(CLR_INTR);
        let command = |index: usize| {
            let last = if index + 1 == total { CMD_STOP } else { 0 };
            match write.get(index) {
                Some(byte) => u32::from(*byte) | last,
                None => {
                    let restart = if index == write.len() && !write.is_empty() { CMD_RESTART } else { 0 };
                    CMD_READ | restart | last
                }
            }
        };
        let start = self.clock.micros();
        let (mut sent, mut received) = (0usize, 0usize);
        while sent < total || received < read.len() {
            while sent < total && self.registers.read(STATUS) & STATUS_TFNF != 0 {
                let asked = sent.saturating_sub(write.len());
                if sent >= write.len() && asked - received >= self.depth {
                    break;
                }
                self.registers.write(DATA_CMD, command(sent));
                sent += 1;
            }
            while received < read.len() && self.registers.read(STATUS) & STATUS_RFNE != 0 {
                read[received] = self.registers.read(DATA_CMD) as u8;
                received += 1;
            }
            if self.registers.read(RAW_INTR_STAT) & INTR_TX_ABRT != 0 {
                return Err(self.abort());
            }
            if self.clock.micros().saturating_sub(start) > self.budget {
                let _ = self.disable();
                return Err(I2cError::Timeout(Step::Transfer));
            }
        }
        loop {
            let raw = self.registers.read(RAW_INTR_STAT);
            if raw & INTR_TX_ABRT != 0 {
                return Err(self.abort());
            }
            if raw & INTR_STOP_DET != 0 {
                self.registers.read(CLR_STOP_DET);
                return Ok(());
            }
            if self.clock.micros().saturating_sub(start) > self.budget {
                let _ = self.disable();
                return Err(I2cError::Timeout(Step::Stop));
            }
        }
    }
}

#[cfg(test)]
pub mod fake {
    use alloc::collections::VecDeque;
    use alloc::vec::Vec;

    use super::*;

    pub struct Ticker(pub u64);

    impl Clock for Ticker {
        fn micros(&mut self) -> u64 {
            self.0 += 1;
            self.0
        }
    }

    pub struct Target {
        pub address: u16,
        pub respond: fn(&[u8]) -> Vec<u8>,
        pub silent: bool,
    }

    pub struct Bus {
        pub registers: [u32; 64],
        pub enabled: bool,
        pub target: Target,
        pub written: Vec<u8>,
        pub commands: Vec<u32>,
        pub reply: VecDeque<u8>,
        pub received: VecDeque<u8>,
        pub raw: u32,
        pub source: u32,
        pub kind: u32,
        pub replied: bool,
        pub overflowed: bool,
    }

    impl Bus {
        pub fn new(target: Target) -> Self {
            Bus {
                registers: [0; 64],
                enabled: false,
                target,
                written: Vec::new(),
                commands: Vec::new(),
                reply: VecDeque::new(),
                received: VecDeque::new(),
                raw: 0,
                source: 0,
                kind: DESIGNWARE,
                replied: false,
                overflowed: false,
            }
        }

        fn command(&mut self, value: u32) {
            self.commands.push(value);
            if self.registers[TAR / 4] != u32::from(self.target.address) {
                self.raw |= INTR_TX_ABRT;
                self.source = 1;
                return;
            }
            if value & CMD_READ != 0 {
                self.overflowed |= self.received.len() >= 16;
                if !self.replied {
                    self.reply = (self.target.respond)(&self.written).into();
                    self.replied = true;
                }
                self.received.push_back(self.reply.pop_front().unwrap_or(0xff));
            } else {
                self.written.push(value as u8);
            }
            if value & CMD_STOP != 0 && !self.target.silent {
                self.raw |= INTR_STOP_DET;
            }
        }
    }

    impl Registers for Bus {
        fn read(&mut self, offset: usize) -> u32 {
            match offset {
                COMP_TYPE => self.kind,
                COMP_PARAM_1 => (15 << 16) | (15 << 8),
                ENABLE_STATUS => u32::from(self.enabled),
                STATUS => STATUS_TFNF | if self.received.is_empty() { 0 } else { STATUS_RFNE },
                DATA_CMD => u32::from(self.received.pop_front().unwrap_or(0)),
                RAW_INTR_STAT => self.raw,
                TX_ABRT_SOURCE => self.source,
                CLR_INTR => {
                    self.raw = 0;
                    0
                }
                CLR_TX_ABRT => {
                    self.raw &= !INTR_TX_ABRT;
                    0
                }
                CLR_STOP_DET => {
                    self.raw &= !INTR_STOP_DET;
                    0
                }
                other => self.registers[other / 4],
            }
        }

        fn write(&mut self, offset: usize, value: u32) {
            match offset {
                ENABLE => {
                    self.enabled = value & 1 == 1;
                    if self.enabled {
                        self.written.clear();
                        self.replied = false;
                    }
                }
                DATA_CMD => self.command(value),
                other => self.registers[other / 4] = value,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::fake::{Bus, Target, Ticker};
    use super::*;

    fn descriptor(written: &[u8]) -> Vec<u8> {
        if written == [0x01, 0x00] { (0..30).collect() } else { vec![0xee; 4] }
    }

    fn pad() -> Bus {
        Bus::new(Target { address: 0x15, respond: descriptor, silent: false })
    }

    #[test]
    fn the_amd_controller_runs_at_no_more_than_four_hundred_kilohertz() {
        let fast = timing(150_000_000, 400_000);
        assert_eq!(fast, Timing { high: 127, low: 239 });
        let period = fast.low + 1 + fast.high + 8;
        assert!(period >= 375, "{period}");
        let standard = timing(150_000_000, 100_000);
        assert_eq!(standard, Timing { high: 637, low: 854 });
    }

    #[test]
    fn the_controller_is_set_up_as_a_fast_master() {
        let master = Master::new(pad(), Ticker(0), 150_000_000, 400_000, 50_000).unwrap();
        let (bus, _) = master.into_parts();
        assert_eq!(bus.registers[CON / 4], 0x65);
        assert_eq!((bus.registers[FS_SCL_HCNT / 4], bus.registers[FS_SCL_LCNT / 4]), (127, 239));
        let mut other = pad();
        other.kind = 0x1234_5678;
        assert_eq!(Master::new(other, Ticker(0), 150_000_000, 400_000, 50_000).err(), Some(I2cError::NotDesignware(0x1234_5678)));
    }

    #[test]
    fn a_register_read_writes_the_address_then_reads_after_a_restart_and_stops_once() {
        let mut master = Master::new(pad(), Ticker(0), 150_000_000, 400_000, 50_000).unwrap();
        let mut reply = [0u8; 30];
        master.transfer(0x15, &[0x01, 0x00], &mut reply).unwrap();
        assert_eq!(reply.to_vec(), (0..30).collect::<Vec<u8>>());
        let (bus, _) = master.into_parts();
        assert_eq!(bus.written, [0x01, 0x00]);
        assert_eq!(bus.commands[2], CMD_READ | CMD_RESTART);
        assert_eq!(*bus.commands.last().unwrap(), CMD_READ | CMD_STOP);
        assert_eq!(bus.commands.iter().filter(|c| *c & CMD_STOP != 0).count(), 1);
        assert_eq!(bus.registers[TAR / 4], 0x15);
    }

    #[test]
    fn a_plain_write_and_a_plain_read_carry_no_restart() {
        let mut master = Master::new(pad(), Ticker(0), 150_000_000, 400_000, 50_000).unwrap();
        master.transfer(0x15, &[0x05, 0x00, 0x00, 0x08], &mut []).unwrap();
        let mut reply = [0u8; 4];
        master.transfer(0x15, &[], &mut reply).unwrap();
        assert_eq!(reply, [0xee; 4]);
        let (bus, _) = master.into_parts();
        assert!(bus.commands.iter().all(|c| c & CMD_RESTART == 0));
        assert_eq!(bus.commands.iter().filter(|c| *c & CMD_STOP != 0).count(), 2);
    }

    #[test]
    fn an_absent_device_is_a_nack_and_a_stuck_bus_is_a_timeout() {
        let mut master = Master::new(pad(), Ticker(0), 150_000_000, 400_000, 50_000).unwrap();
        let mut reply = [0u8; 2];
        assert_eq!(master.transfer(0x2c, &[0x01, 0x00], &mut reply), Err(I2cError::Nack(1)));
        assert_eq!(master.transfer(0x15, &[0x01, 0x00], &mut reply), Ok(()));
        let silent = Bus::new(Target { address: 0x15, respond: descriptor, silent: true });
        let mut stuck = Master::new(silent, Ticker(0), 150_000_000, 400_000, 1_000).unwrap();
        assert_eq!(stuck.transfer(0x15, &[0x01, 0x00], &mut reply), Err(I2cError::Timeout(Step::Stop)));
        assert_eq!(master.transfer(0x80, &[1], &mut []), Err(I2cError::BadAddress(0x80)));
        assert_eq!(master.transfer(0x15, &[0u8; 5000], &mut []), Err(I2cError::TooLong(5000)));
    }

    #[test]
    fn long_reads_never_ask_for_more_than_the_fifo_holds() {
        fn big(_: &[u8]) -> Vec<u8> {
            (0..=255u8).cycle().take(1200).collect()
        }
        let bus = Bus::new(Target { address: 0x15, respond: big, silent: false });
        let mut master = Master::new(bus, Ticker(0), 150_000_000, 400_000, 1_000_000).unwrap();
        let mut reply = vec![0u8; 1200];
        master.transfer(0x15, &[0x06, 0x00], &mut reply).unwrap();
        assert!(reply.iter().enumerate().all(|(i, b)| *b == i as u8));
        assert!(!master.into_parts().0.overflowed);
    }
}
