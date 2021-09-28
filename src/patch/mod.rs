pub mod init_segment;
pub mod segment;

use crate::Result;
use mp4::mp4box::*;
use std::fs::File;

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
