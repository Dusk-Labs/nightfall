#![doc = include_str!("../README.md")]
#![feature(
    assert_matches,
    result_flattening,
    hash_drain_filter,
    box_syntax,
    once_cell
)]

/// Contains all the error types for this crate.
pub mod error;
/// Helper methods to probe a mediafile for metadata.
pub mod ffprobe;
/// Contains all profiles currently implemented.
pub mod profiles;
/// Contains the struct representing a streaming session.
mod session;
/// Contains utils that make my life easier.
pub mod utils;

use crate::error::*;
use crate::profiles::*;
use crate::session::Session;

use std::collections::HashMap;
use std::collections::VecDeque;
use std::fs::File;
use std::io::prelude::*;
use std::io::BufReader;
use std::io::SeekFrom;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use xtra_proc::actor;
use xtra_proc::handler;

use slog::info;
use slog::warn;

use mp4::mp4box::*;

pub use tokio::process::ChildStdout;

pub struct StreamStat {
    hard_seeked_at: u32,
    last_hard_seek: Instant,
}

impl Default for StreamStat {
    fn default() -> Self {
        Self {
            hard_seeked_at: 0,
            last_hard_seek: Instant::now(),
        }
    }
}

#[actor]
pub struct StateManager {
    /// The directory where we store stream artifacts
    pub outdir: String,
    /// Path to a `ffmpeg` binary.
    pub ffmpeg: String,
    /// Contains all of the sessions currently managed by this actor.
    pub sessions: HashMap<String, Session>,
    /// Contains some useful stream stats
    pub stream_stats: HashMap<String, StreamStat>,
    /// Contains the exit status of dead sessions
    pub exit_statuses: HashMap<String, String>,
    /// Logger
    pub logger: slog::Logger,
}

#[actor]
impl StateManager {
    pub fn new(outdir: String, ffmpeg: String, logger: slog::Logger) -> Self {
        Self {
            outdir,
            ffmpeg,
            logger,
            sessions: HashMap::new(),
            stream_stats: HashMap::new(),
            exit_statuses: HashMap::new(),
        }
    }

    #[handler]
    async fn create(
        &mut self,
        profile: &'static dyn TranscodingProfile,
        profile_args: ProfileContext,
    ) -> Result<String> {
        let mut profile_args = profile_args;

        let session_id = uuid::Uuid::new_v4().to_hyphenated().to_string();
        let tag = if let Some(width) = profile_args.width {
            let bitrate = profile_args
                .bitrate
                .map(|x| format!("@{}", x))
                .unwrap_or_default();
            let height = profile_args.height.unwrap_or(-2);

            format!("{} ({}x{}{})", profile.tag(), width, height, bitrate)
        } else {
            let bitrate = profile_args
                .bitrate
                .map(|x| format!("@{}", x))
                .unwrap_or_default();
            format!("{}{}", profile.tag(), bitrate)
        };

        info!(
            self.logger,
            "New session {} map {} -> {}", &session_id, profile_args.stream, tag
        );

        profile_args.outdir = format!("{}/{}", &self.outdir, session_id);
        profile_args.ffmpeg_bin = self.ffmpeg.clone();

        let new_session = Session::new(session_id.clone(), profile, profile_args);

        self.sessions.insert(session_id.clone(), new_session);

        Ok(session_id)
    }

    #[handler]
    async fn chunk_init_request(&mut self, id: String, chunk: u32) -> Result<String> {
        let session = self
            .sessions
            .get_mut(&id)
            .ok_or(NightfallError::SessionDoesntExist)?;

        if !session.is_chunk_done(chunk) {
            if session.start_num() != chunk {
                session.join().await;
                session.reset_to(chunk);
                let _ = session.start().await;

                let stat = self.stream_stats.entry(id).or_default();
                stat.hard_seeked_at = chunk;
                stat.last_hard_seek = Instant::now();
            }

            session.cont();
        }

        if !session.has_started() {
            let _ = session.start().await;
        }

        if session.is_chunk_done(chunk) {
            return Ok(session.init_seg());
        }

        Err(NightfallError::ChunkNotDone)
    }

    #[handler]
    async fn chunk_request(&mut self, id: String, chunk: u32) -> Result<String> {
        let session = self
            .sessions
            .get_mut(&id)
            .ok_or(NightfallError::SessionDoesntExist)?;
        let stats = self.stream_stats.entry(id).or_default();

        if !session.has_started() {
            let _ = session.start().await;
        }

        if !session.is_chunk_done(chunk) {
            let eta = session.eta_for(chunk).as_millis() as f64;
            let eta_tol = (10_000.0 / session.raw_speed()).max(8_000.0);

            // FIXME: When we hard seek and start a new ffmpeg session for some reason ffmpeg
            // reports invalid speed but then evens out. The problem is that causes seeking
            // multiple times in a row to be very slow.
            // thus for like the first 10s after a hard seek we exclusively hard seek if the
            // target is over 10 chunks into the future.
            let should_hard_seek = chunk < session.start_num()
                || (chunk > session.current_chunk() + 15
                    && Instant::now() < stats.last_hard_seek + Duration::from_secs(15)
                    && chunk > stats.hard_seeked_at)
                || eta > eta_tol;

            session.cont();

            if should_hard_seek {
                session.join().await;
                session.reset_to(chunk);
                let _ = session.start().await;

                stats.last_hard_seek = Instant::now();
                stats.hard_seeked_at = chunk;
            }

            Err(NightfallError::ChunkNotDone)
        } else {
            let chunk_path = session.chunk_to_path(chunk);

            // hint that we should probably unpause ffmpeg for a bit
            if chunk + 2 >= session.current_chunk() {
                session.cont();
            }

            session.reset_timeout(chunk);

            let log = self.logger.clone();
            let path = chunk_path.clone();

            let real_segment = session.real_segment;
            match tokio::task::spawn_blocking(move || patch_segment(log, path, real_segment))
                .await
                .unwrap()
            {
                Ok(seq) => session.real_segment = seq,
                Err(e) => warn!(self.logger, "Failed to patch segment."; "error" => e.to_string()),
            }

            Ok(chunk_path)
        }
    }

    #[handler]
    async fn chunk_eta(&mut self, id: String, chunk: u32) -> Result<u64> {
        let session = self
            .sessions
            .get_mut(&id)
            .ok_or(NightfallError::SessionDoesntExist)?;
        Ok(session.eta_for(chunk).as_secs())
    }

    #[handler]
    async fn should_hard_seek(&mut self, id: String, chunk: u32) -> Result<bool> {
        let session = self
            .sessions
            .get_mut(&id)
            .ok_or(NightfallError::SessionDoesntExist)?;
        let stats = self.stream_stats.entry(id).or_default();

        if !session.has_started() {
            return Ok(false);
        }
        // if we are seeking backwards we always want to restart the stream
        // This is because our init.mp4 gets overwritten if we seeked forward at some point
        // Furthermore we want to hard seek anyway if the player is browser based.
        if chunk < session.start_num() {
            return Ok(true);
        }

        // FIXME: When we hard seek and start a new ffmpeg session for some reason ffmpeg
        // reports invalid speed but then evens out. The problem is that causes seeking
        // multiple times in a row to be very slow.
        // thus for like the first 10s after a hard seek we exclusively hard seek if the
        // target is over 10 chunks into the future.
        if chunk > session.current_chunk() + 15
            && Instant::now() < stats.last_hard_seek + Duration::from_secs(15)
        {
            return Ok(true);
        }

        Ok((session.eta_for(chunk).as_millis() as f64)
            > (10_000.0 / session.raw_speed()).max(5_000.0))
    }

    #[handler]
    async fn die(&mut self, id: String) -> Result<()> {
        let session = self
            .sessions
            .get_mut(&id)
            .ok_or(NightfallError::SessionDoesntExist)?;
        info!(self.logger, "Killing session {}", id);
        session.join().await;
        session.set_timeout();

        Ok(())
    }

    #[handler]
    async fn die_ignore_gc(&mut self, id: String) -> Result<()> {
        let session = self
            .sessions
            .get_mut(&id)
            .ok_or(NightfallError::SessionDoesntExist)?;
        info!(self.logger, "Killing session {}", id);
        session.join().await;

        Ok(())
    }

    #[handler]
    async fn get_sub(&mut self, id: String, name: String) -> Result<String> {
        let session = self
            .sessions
            .get_mut(&id)
            .ok_or(NightfallError::SessionDoesntExist)?;

        if !session.has_started() {
            let _ = session.start().await;
        }

        session.subtitle(name).ok_or(NightfallError::ChunkNotDone)
    }

    #[handler]
    async fn get_stderr(&mut self, id: String) -> Result<String> {
        // TODO: Move this out of here, instead we should just return the log file.
        let session = self
            .sessions
            .get_mut(&id)
            .ok_or(NightfallError::SessionDoesntExist)?;
        session.stderr().ok_or(NightfallError::Aborted)
    }

    #[handler]
    async fn garbage_collect(&mut self) -> Result<()> {
        #[allow(clippy::ptr_arg)]
        fn collect(_: &String, session: &mut Session) -> bool {
            if session.is_hard_timeout() {
                return true;
            } else if session.try_wait() {
                return false;
            }

            false
        }

        let mut to_reap: HashMap<_, _> = self.sessions.drain_filter(collect).collect();

        if !to_reap.is_empty() {
            info!(self.logger, "Reaping {} streams", to_reap.len());
        }

        for (k, v) in to_reap.iter_mut() {
            self.exit_statuses
                .insert(k.clone(), v.stderr().unwrap_or_default());
            v.join().await;
            v.delete_tmp();
        }

        let mut cnt = 0;
        for (_, v) in self.sessions.iter_mut() {
            if v.is_timeout() && !v.paused && !v.try_wait() {
                v.pause();
                cnt += 1;
            }
        }

        if cnt != 0 {
            info!(self.logger, "Paused {} streams", cnt);
        }

        Ok(())
    }

    #[handler]
    async fn take_stdout(&mut self, id: String) -> Result<ChildStdout> {
        let session = self
            .sessions
            .get_mut(&id)
            .ok_or(NightfallError::SessionDoesntExist)?;

        session.take_stdout().ok_or(NightfallError::Aborted)
    }

    #[handler]
    async fn start(&mut self, id: String) -> Result<()> {
        let session = self
            .sessions
            .get_mut(&id)
            .ok_or(NightfallError::SessionDoesntExist)?;

        session.start().await.map_err(|_| NightfallError::Aborted)
    }
}

#[derive(Default)]
pub struct Segment {
    pub styp: Option<FtypBox>,
    pub sidx: Option<SidxBox>,
    pub moof: Option<MoofBox>,
    pub mdat: Option<MdatBox>,
}

impl Segment {
    pub fn normalize_and_dump(mut self, seq: u32, file: &mut File) -> Result<()> {
        if let Some(tfdt) = self
            .moof
            .as_mut()
            .map(|x| x.trafs[0].tfdt.as_mut())
            .ok_or(NightfallError::MissingSegmentBox)?
        {
            if let Some(sidx) = self.sidx.as_ref() {
                tfdt.base_media_decode_time = sidx.earliest_presentation_time;
            }
        }

        if let Some(moof) = self.moof.as_mut() {
            moof.mfhd.sequence_number = seq;
        }

        if let Some(mut styp) = self.styp {
            styp.box_type = BoxType::StypBox;
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

/// Function will patch a segment to appear as if it belongs to the current stream and is
/// sequential.
fn patch_segment<T: AsRef<Path>>(log: slog::Logger, file: T, mut seq: u32) -> Result<u32> {
    let f = File::open(&file)?;
    let size = f.metadata()?.len();
    let mut reader = BufReader::new(f);

    let start = reader.seek(SeekFrom::Current(0))?;

    let mut segments = VecDeque::new();
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
                // mdat is the last atom in a segment so we break the segment here and append it to
                // our list.
                segments.push_back(current_segment);
                current_segment = Segment::default();
            }
            BoxType::StypBox | BoxType::FtypBox => {
                current_segment.styp = Some(FtypBox::read_box(&mut reader, s)?);
            }
            b => {
                warn!(log, "Got a weird box type."; "box_type" => b.to_string());
                skip_box(&mut reader, s)?;
            }
        }

        current = reader.seek(SeekFrom::Current(0))?;
    }

    let mut out = File::create(&file)?;

    while let Some(segment) = segments.pop_front() {
        segment.normalize_and_dump(seq, &mut out)?;
        seq += 1;
    }

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
