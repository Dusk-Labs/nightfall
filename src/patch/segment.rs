use std::collections::VecDeque;
use std::fs::File;
use std::io::prelude::*;
use std::io::BufReader;
use std::io::Seek;
use std::io::SeekFrom;
use std::path::Path;

use crate::NightfallError;
use crate::Result;

use tokio::task::spawn_blocking;

use mp4::mp4box::*;
use tracing::debug;

/// Struct represents an individual segment from a stream.
#[derive(Clone, Default, Debug)]
pub struct Segment {
    /// styp box is needed if the segment is written to a separate file.
    /// in our case we just clone it from the parent init segment.
    pub styp: Option<FtypBox>,
    /// segment index box contains the index of the segment.
    pub sidx: Option<SidxBox>,
    /// Moof box contains metadata about the segment like the PTS and DTS.
    pub moof: Option<MoofBox>,
    /// Contains audio-visual data.
    pub mdat: Option<MdatBox>,
}

impl Segment {
    pub fn debug(&self) {
        println!("styp: {:?}", self.styp);
        println!("sidx: {:?}", self.sidx);
        println!("moof: {:?}", self.moof);
        println!(
            "mdat: {:?}",
            self.mdat.as_ref().map(|x| x.data.len()).unwrap_or(0)
        );
    }

    pub fn from_reader(mut reader: impl BufRead + Seek, size: u64) -> Result<(Self, u64)> {
        let start = reader.seek(SeekFrom::Current(0))?;

        let mut current = start;
        let mut segment = Self::default();

        while current < size {
            let header = BoxHeader::read(&mut reader)?;
            let BoxHeader { name, size: s } = header;

            match name {
                BoxType::SidxBox => {
                    segment.sidx = Some(SidxBox::read_box(&mut reader, s)?);
                }
                BoxType::MoofBox => {
                    segment.moof = Some(MoofBox::read_box(&mut reader, s)?);
                }
                BoxType::MdatBox => {
                    segment.mdat = Some(MdatBox::read_box(&mut reader, s)?);

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
                    debug!(box_type = %b, "Got a weird box type.");
                    skip_box(&mut reader, s)?;
                }
            }

            current = reader.seek(SeekFrom::Current(0))?;
        }

        // NOTE: In some cases, we could get here without a complete segment existing.
        Ok((segment, size))
    }

    /// Sometimes ffmpeg will output a bare initial segment.
    /// This method allows us to detect such segments and apply a fix if we want accurate seeks.
    /// An empty segment consists of a segment with only a `styp` box.
    pub fn is_empty_segment(&self) -> bool {
        self.styp.is_some() && self.sidx.is_none() && self.moof.is_none() && self.mdat.is_none()
    }

    /// Method will create a styp box for this segment if it doesnt exist
    pub fn gen_styp(mut self) -> Self {
        if self.styp.is_none() {
            let mut styp = FtypBox::default();
            styp.box_type = BoxType::StypBox;

            self.styp = Some(styp);
        }

        self
    }

    pub fn set_styp(mut self) -> Self {
        if let Some(styp) = self.styp.as_mut() {
            styp.box_type = BoxType::StypBox;
        }

        self
    }

    pub fn set_segno(mut self, seq: u32) -> Self {
        if let Some(moof) = self.moof.as_mut() {
            moof.mfhd.sequence_number = seq;
        }
        self
    }

    pub fn normalize_dts(mut self) -> Self {
        // NOTE: Sometimes the first segment after init.mp4 can be blank, in cases like that we
        // just ignore that moof is empty.
        if let Some(tfdt) = self
            .moof
            .as_mut()
            .and_then(|x| x.trafs.get_mut(0).and_then(|x| x.tfdt.as_mut()))
        {
            if let Some(sidx) = self.sidx.as_ref() {
                tfdt.base_media_decode_time = sidx.earliest_presentation_time;
            }
        }

        self
    }

    pub fn write(self, file: &mut File) -> Result<()> {
        if let Some(styp) = self.styp {
            styp.write_box(file)?;
        }

        if let Some(sidx) = self.sidx {
            sidx.write_box(file)?;
        }

        if let Some(moof) = self.moof {
            moof.write_box(file)?;
        }

        if let Some(mdat) = self.mdat {
            mdat.write_box(file)?;
        }

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
pub async fn patch_segment(file: impl AsRef<Path> + Send + 'static, mut seq: u32) -> Result<u32> {
    spawn_blocking(move || {
        let f = File::open(&file)?;
        let size = f.metadata()?.len();
        let mut reader = BufReader::new(f);

        let mut segments = VecDeque::new();
        let mut current = reader.seek(SeekFrom::Current(0))?;

        while current < size {
            let (segment, new_position) = Segment::from_reader(&mut reader, size)?;
            segments.push_back(segment);
            current = new_position;
        }

        // Sometimes we get partial segments, ie empty segments where the data is actually in the
        // init segment. This is a problem when hard seeking as we lose out on ~5s of data, thus we
        // fix by patching the init segment and moving the data over.
        if segments.len() == 1 && segments[0].is_empty_segment() {
            return Err(NightfallError::PartialSegment(
                segments.pop_front().unwrap(),
            ));
        }

        let mut f = File::create(&file)?;
        while let Some(segment) = segments.pop_front() {
            // Here we normalize the DTS to be equal to the EPT/PTS and we also set the corrent
            // segment number.
            segment
                .gen_styp()
                .set_segno(seq)
                .write(&mut f)?;

            seq += 1;
        }

        Ok(seq)
    })
    .await
    .unwrap()
}
