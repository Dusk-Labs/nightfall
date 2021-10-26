use std::collections::VecDeque;
use std::fs::File;
use std::io::prelude::*;
use std::io::BufReader;
use std::io::Seek;
use std::io::SeekFrom;
use std::path::Path;

use super::segment::Segment;
use crate::Result;

use tokio::task::spawn_blocking;

use mp4::mp4box::*;
use tracing::warn;

#[derive(Default)]
pub struct InitSegment {
    pub ftyp: Option<FtypBox>,
    // FIXME: implement parsers for mvex and mdta boxes.
    // For now we just save and dump the bytes since we dont modify them ever.
    pub moov: Vec<u8>,
    pub segments: VecDeque<Segment>,
}

impl InitSegment {
    pub fn from_reader(mut reader: impl BufRead + Seek, size: u64) -> Result<Self> {
        let mut segment = Self::default();
        let start = reader.seek(SeekFrom::Current(0))?;

        let mut current = start;
        let mut current_segment = Segment::default();

        while current < size {
            let header = BoxHeader::read(&mut reader)?;
            let BoxHeader { name, size: s } = header;

            match name {
                BoxType::SidxBox => {
                    current_segment.sidx = Some(SidxBox::read_box(&mut reader, s)?);
                }
                BoxType::MoofBox => {
                    current_segment.moof = Some(MoofBox::read_box(&mut reader, s)?);
                }
                BoxType::MdatBox => {
                    current_segment.mdat = Some(MdatBox::read_box(&mut reader, s)?);
                    // segments packed in the init segments dont come with a styp box
                    // so we clone the ftyp box of the init segment and change its type.
                    if current_segment.styp.is_none() {
                        current_segment.styp = segment.ftyp.clone();
                    }
                    // mdat is the last atom in a segment so we break the segment here and append it to
                    // our list.
                    segment.segments.push_back(current_segment);
                    current_segment = Segment::default();
                }
                BoxType::FtypBox => {
                    segment.ftyp = Some(FtypBox::read_box(&mut reader, s)?);
                }
                BoxType::MoovBox => {
                    segment.moov = vec![0; (s - 8) as usize];
                    reader.read_exact(segment.moov.as_mut_slice())?;
                }
                BoxType::StypBox => {
                    current_segment.styp = Some(FtypBox::read_box(&mut reader, s)?);
                }
                b => {
                    warn!(box_type = %b, "Got a weird box type.");
                    BoxHeader { name: b, size: s }.write(&mut segment.moov)?;
                    let mut boks = vec![0; (s - 8) as usize];
                    reader.read_exact(boks.as_mut_slice())?;
                    segment.moov.append(&mut boks);
                }
            }

            current = reader.seek(SeekFrom::Current(0))?;
        }

        Ok(segment)
    }

    /// Method will check if this init segment contains any real segments.
    pub fn contains_segments(&self) -> bool {
        !self.segments.is_empty()
    }

    pub fn normalize_and_dump(self, file: &mut File) -> Result<()> {
        if let Some(ftyp) = self.ftyp {
            ftyp.write_box(file)?;
        }

        BoxHeader {
            name: BoxType::MoovBox,
            size: self.moov.len() as u64 + 8,
        }
        .write(file)?;
        file.write_all(&self.moov)?;

        Ok(())
    }
}

/// Function reads a init segment and moves audio-visual data over from the init segment into
/// `segment`.
///
/// # Arguments
/// * `log` - logger instance for debugging
/// * `init` - Path to the initialization segment.
/// * `segment` - Path to the segment
/// * `seq` - starting sequence number
pub async fn patch_init_segment(
    init: impl AsRef<Path> + Send + 'static,
    segment_path: impl AsRef<Path> + Send + 'static,
    mut seq: u32,
) -> Result<u32> {
    spawn_blocking(move || {
        let f = File::open(&init)?;
        let size = f.metadata()?.len();
        let mut reader = BufReader::new(f);

        let mut segment = InitSegment::from_reader(&mut reader, size)?;

        let mut f = File::create(&segment_path)?;
        while let Some(segment) = segment.segments.pop_front() {
            // Here we normalize the DTS to be equal to the EPT/PTS and we also set the corrent
            // segment number.
            segment
                .gen_styp()
                .set_styp()
                .normalize_dts()
                .set_segno(seq)
                .write(&mut f)?;

            seq += 1;
        }

        let mut f = File::create(&init)?;
        segment.normalize_and_dump(&mut f)?;

        Ok(seq)
    })
    .await
    .unwrap()
}
