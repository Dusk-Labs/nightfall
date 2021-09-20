use std::collections::VecDeque;
use std::io::*;
use std::fs::File;

use super::segment::Segment;
use crate::Result;

use mp4::mp4box::*;
use slog::warn;

#[derive(Default)]
pub struct InitSegment {
    pub ftyp: Option<FtypBox>,
    // FIXME: implement parsers for mvex and mdta boxes.
    // For now we just save and dump the bytes since we dont modify them ever.
    pub moov: Vec<u8>,
    pub segments: VecDeque<Segment>,
}

impl InitSegment {
    pub fn from_reader(mut reader: impl BufRead + Seek, size: u64, log: slog::Logger) -> Result<Self> {
        let mut segment = Self::default();
        let start = reader.seek(SeekFrom::Current(0))?;

        let mut current = start;
        let mut current_segment = Segment::default();

        while current < size {
            let header = BoxHeader::read(&mut reader)?;
            let BoxHeader { name, size: s } = header;

            match name {
                BoxType::SidxBox => {
                    current_segment.sidx = SidxBox::read_box(&mut reader, s)?;
                }
                BoxType::MoofBox => {
                    current_segment.moof = MoofBox::read_box(&mut reader, s)?;
                }
                BoxType::MdatBox => {
                    current_segment.mdat = MdatBox::read_box(&mut reader, s)?;
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
                    warn!(log, "Got a weird box type."; "box_type" => b.to_string());
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
