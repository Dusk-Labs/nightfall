pub mod segment;
pub mod init_segment;

use crate::NightfallError;
use crate::Result;
use self::segment::*;
use self::init_segment::*;

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use mp4::mp4box::*;

fn patch_init_segment<T: AsRef<Path>>(
    log: slog::Logger,
    init: T,
    next_seg: T,
    mut seq: u32,
) -> Result<u32> {
    return Err(NightfallError::Aborted);
    let f = File::open(&init)?;
    let size = f.metadata()?.len();
    let reader = BufReader::new(f);
    let mut init_segment = InitSegment::from_reader(reader, size, log.clone())?;

    if init_segment.segments.is_empty() {
        return Ok(seq);
    }

    let mut next_seg = File::create(&next_seg)?;
    while let Some(segment) = init_segment.segments.pop_front() {
        //segment.normalize_and_dump(seq, &mut next_seg)?;
        seq += 1;
    }

    let mut init = File::create(&init)?;
    init_segment.normalize_and_dump(&mut init)?;

    Ok(seq)
}

trait WriteBoxToFile {
    fn write_box(&self, writer: File) -> Result<u64>;
}

impl<T> WriteBoxToFile for T
where
    T: WriteBox<File>,
{
    fn write_box<'a>(&self, writer: File) -> Result<u64> {
        Ok(<T as WriteBox<File>>::write_box(&self, writer)?)
    }
}
