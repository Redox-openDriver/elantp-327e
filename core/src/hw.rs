pub trait Registers {
    fn read(&mut self, offset: usize) -> u32;
    fn write(&mut self, offset: usize, value: u32);
}

pub trait Clock {
    fn micros(&mut self) -> u64;
}
