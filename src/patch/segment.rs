use std::collections::VecDeque;
use std::fs::File;
use std::io::prelude::*;
use std::io::BufReader;
use std::io::Seek;
use std::io::SeekFrom;
use std::path::Path;

use crate::Result;
use slog::debug;
use tokio::task::spawn_blocking;

use mp4::mp4box::*;

/// Struct represents an individual segment from a stream.
#[derive(Clone, Default, Debug)]
pub struct Segment {
    /// styp box is needed if the segment is written to a separate file.
    /// in our case we just clone it from the parent init segment.
    pub styp: Option<FtypBox>,
    /// segment index box contains the index of the segment.
    pub sidx: SidxBox,
    /// Moof box contains metadata about the segment like the PTS and DTS.
    pub moof: MoofBox,
    /// Contains audio-visual data.
    pub mdat: MdatBox,
}

impl Segment {
    pub fn debug(&self) {
        println!("styp: {:?}", self.styp);
        println!("sidx: {:?}", self.sidx);
        println!("moof: {:?}", self.moof);
        println!(
            "mdat: {:?}",
            self.mdat.data.len()
        );
    }

    pub fn from_reader(
        mut reader: impl BufRead + Seek,
        size: u64,
        log: slog::Logger,
    ) -> Result<(Self, u64)> {
        let start = reader.seek(SeekFrom::Current(0))?;

        let mut current = start;
        let mut segment = Self::default();

        while current < size {
            let header = BoxHeader::read(&mut reader)?;
            let BoxHeader { name, size: s } = header;

            match name {
                BoxType::SidxBox => {
                    segment.sidx = SidxBox::read_box(&mut reader, s)?;
                }
                BoxType::MoofBox => {
                    segment.moof = MoofBox::read_box(&mut reader, s)?;
                }
                BoxType::MdatBox => {
                    segment.mdat = MdatBox::read_box(&mut reader, s)?;

                    // Since mdat would be the last box in the segment, we just return the segment
                    // here as well as the leftover bytes.
                    let leftover_bytes = reader.seek(SeekFrom::Current(0))?;
                    return Ok((segment, leftover_bytes));
                }
                BoxType::StypBox => {
                    let mut styp = FtypBox::read_box(&mut reader, s)?;
                    styp.box_type = BoxType::StypBox;
                    segment.styp = Some(styp);
                }
                b => {
                    debug!(log, "Got a weird box type."; "box_type" => b.to_string());
                    skip_box(&mut reader, s)?;
                }
            }

            current = reader.seek(SeekFrom::Current(0))?;
        }

        // NOTE: In some cases, we could get here without a complete segment existing. 
        Ok((segment, size))
    }

    pub fn set_segno(mut self, seq: u32) -> Self {
        self.moof.mfhd.sequence_number = seq;
        self
    }

    pub fn normalize_dts(mut self) -> Self {
        if let Some(tfdt) = self.moof.trafs[0].tfdt.as_mut() {
            tfdt.base_media_decode_time = self.sidx.earliest_presentation_time;
        }

        self
    }

    pub fn write(self, file: &mut File) -> Result<()> {
        if let Some(styp) = self.styp {
            styp.write_box(file)?;
        }

        self.sidx.write_box(file)?;
        self.moof.write_box(file)?;
        self.mdat.write_box(file)?;

        Ok(())
    }
}

/// Function reads a segment file and patches it so that it is consistent
///
/// # Arguments
/// * `log` - logger instance for debugging
/// * `file` - target input/output file.
/// * `seq` - starting sequence number.
///
/// # Returns
/// This function will return the index of the current segment.
pub async fn patch_segment(log: slog::Logger, file: impl AsRef<Path> + Send + 'static, mut seq: u32) -> Result<u32> {
    spawn_blocking(move || {
        let f = File::open(&file)?;
        let size = f.metadata()?.len();
        let mut reader = BufReader::new(f);

        let mut segments = VecDeque::new();
        let mut current = reader.seek(SeekFrom::Current(0))?;

        while current < size {
            let (segment, new_position) = Segment::from_reader(&mut reader, size, log.clone())?;
            segments.push_back(segment);
            current = new_position;
        }

        let mut f = File::create(&file)?;
        while let Some(segment) = segments.pop_front() {
            // Here we normalize the DTS to be equal to the EPT/PTS and we also set the corrent
            // segment number.
            segment
                .normalize_dts()
                .set_segno(seq)
                .write(&mut f)?;

            seq += 1;
        }

        Ok(seq)
    }).await.unwrap()
}
