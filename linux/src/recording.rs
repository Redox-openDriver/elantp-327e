use std::io::{self, Read, Write};

use elantp_core::report::MAX_DESCRIPTOR;

use crate::hidraw::MAX_REPORT;

pub const MAGIC: [u8; 4] = *b"ETPR";
pub const VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recording {
    pub descriptor: Vec<u8>,
    pub reports: Vec<(u64, Vec<u8>)>,
}

#[derive(Debug)]
pub enum RecordingError {
    Io(io::Error),
    Magic,
    Version(u16),
    Descriptor(usize),
    Report(usize),
}

impl From<io::Error> for RecordingError {
    fn from(error: io::Error) -> Self {
        RecordingError::Io(error)
    }
}

pub fn header(out: &mut impl Write, descriptor: &[u8]) -> io::Result<()> {
    let length = u16::try_from(descriptor.len()).ok().filter(|l| usize::from(*l) <= MAX_DESCRIPTOR).ok_or(io::ErrorKind::InvalidInput)?;
    out.write_all(&MAGIC)?;
    out.write_all(&VERSION.to_le_bytes())?;
    out.write_all(&length.to_le_bytes())?;
    out.write_all(descriptor)
}

pub fn report(out: &mut impl Write, micros: u64, bytes: &[u8]) -> io::Result<()> {
    let length = u16::try_from(bytes.len()).ok().filter(|l| usize::from(*l) <= MAX_REPORT).ok_or(io::ErrorKind::InvalidInput)?;
    out.write_all(&micros.to_le_bytes())?;
    out.write_all(&length.to_le_bytes())?;
    out.write_all(bytes)
}

fn exact<const N: usize>(input: &mut impl Read) -> io::Result<Option<[u8; N]>> {
    let mut buffer = [0u8; N];
    let mut filled = 0;
    while filled < N {
        match input.read(&mut buffer[filled..])? {
            0 if filled == 0 => return Ok(None),
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            n => filled += n,
        }
    }
    Ok(Some(buffer))
}

fn bytes(input: &mut impl Read, length: usize) -> io::Result<Vec<u8>> {
    let mut buffer = vec![0u8; length];
    input.read_exact(&mut buffer)?;
    Ok(buffer)
}

pub fn read(input: &mut impl Read) -> Result<Recording, RecordingError> {
    let magic: [u8; 4] = exact(input)?.ok_or(RecordingError::Magic)?;
    if magic != MAGIC {
        return Err(RecordingError::Magic);
    }
    let version = u16::from_le_bytes(exact(input)?.ok_or(RecordingError::Magic)?);
    if version != VERSION {
        return Err(RecordingError::Version(version));
    }
    let length = usize::from(u16::from_le_bytes(exact(input)?.ok_or(RecordingError::Magic)?));
    if length > MAX_DESCRIPTOR {
        return Err(RecordingError::Descriptor(length));
    }
    let descriptor = bytes(input, length)?;
    let mut reports = Vec::new();
    while let Some(time) = exact::<8>(input)? {
        let length = usize::from(u16::from_le_bytes(exact(input)?.ok_or(io::Error::from(io::ErrorKind::UnexpectedEof))?));
        if length == 0 || length > MAX_REPORT {
            return Err(RecordingError::Report(reports.len()));
        }
        reports.push((u64::from_le_bytes(time), bytes(input, length)?));
    }
    Ok(Recording { descriptor, reports })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recording_reads_back_as_written_and_damage_is_reported() {
        let mut file = Vec::new();
        header(&mut file, &[1, 2, 3]).unwrap();
        report(&mut file, 0, &[0x54, 9]).unwrap();
        report(&mut file, 8_000, &[0x54, 8, 7]).unwrap();
        let recording = read(&mut file.as_slice()).unwrap();
        assert_eq!(recording, Recording { descriptor: vec![1, 2, 3], reports: vec![(0, vec![0x54, 9]), (8_000, vec![0x54, 8, 7])] });
        assert!(matches!(read(&mut &file[..file.len() - 1]), Err(RecordingError::Io(_))));
        assert!(matches!(read(&mut &b"XXXX"[..]), Err(RecordingError::Magic)));
        let mut future = file.clone();
        future[4] = 9;
        assert!(matches!(read(&mut future.as_slice()), Err(RecordingError::Version(9))));
        let mut empty = Vec::new();
        header(&mut empty, &[]).unwrap();
        empty.extend_from_slice(&5u64.to_le_bytes());
        empty.extend_from_slice(&0u16.to_le_bytes());
        assert!(matches!(read(&mut empty.as_slice()), Err(RecordingError::Report(0))));
        assert!(header(&mut Vec::new(), &[0u8; MAX_DESCRIPTOR + 1]).is_err());
    }
}
