//! Nightfall - Hackers streaming lib.
//!
//! # Whats this?
//! Nightfall is a implementation of on-demand video streaming and transcoding. Unlike other
//! implementations, this lib has support for seamless and cheap seeking. It requires
//! nothing but for `ffmpeg` and `ffprobe` to exist on the system it runs on.
//!
//! # How does it work?
//! The implementation is quite hacky and involves abusing dash manifests. All the logic
//! essentially boils down to two mechanics
//!
//! # Manifest Generation
//! Generating mpeg-dash manifest that doesnt have hardcoded chunk ranges in (sorta like a
//! live-manifest). The player thus assumes that all chunks are exactly n-seconds long thus it
//! can just keep requesting the next chunk until we 404.
//!
//! # Transcoding chunks on-demand
//! Transcoding chunks on-demand. Once a player requests the next chunk in a video stream we get
//! that request, we do a lookup to see if that chunk exists and has been completely written
//! (this is to avoid data races and crashes). If it exists we return the absolute path to the
//! chunk, otherwise we return a None.
//!    
//! Of course this logic is quite brittle, thus we introduce timeouts. If after x seconds the
//! chunk hasnt finished transcoding we kill the previous process and start a new one with a
//! offset of the chunk we want.
//!    
//! This does mainly one thing for us. It allows us to seek anywhere in a video without having to
//! wait for the rest of the video to transcode.
//!
//! # Caveats
//! The overhead of this is quite big at the moment (dont quote me on this), thus players have to
//! have lean request timeouts as in some cases spawning and killing ffmpeg processes when seeking
//! around could turn out to be slow.
//!
//! # Notes
//! Each track in a manifest is unique, thus they get unique ids. When seeking in the current track
//! the ID is preserved.
//!
//! What happens if two chunks for the same stream are requested simulatenously??
#![feature(try_trait, result_flattening, hash_drain_filter)]
#![allow(unused_must_use, dead_code)]

/// Contains all the error types for this crate.
pub mod error;
/// Helper methods to probe a mediafile for metadata.
pub mod ffprobe;
/// Contains our profiles as well as their respective args.
pub mod profile;
/// Contains the struct representing a streaming session.
mod session;
/// Contains utils that make my life easier.
pub mod utils;

use crate::error::*;
use crate::profile::*;
use crate::session::Session;

use std::time::Duration;
use std::time::Instant;

use xtra_proc::actor;
use xtra_proc::handler;
use std::collections::HashMap;
use async_trait::async_trait;

use slog::info;

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
    async fn create(&mut self, stream_type: StreamType, file: String) -> Result<String> {
        let session_id = uuid::Uuid::new_v4().to_hyphenated().to_string();

        let new_session = Session::new(
            session_id.clone(),
            file,
            0,
            format!("{}/{}", self.outdir.clone(), session_id.clone()),
            stream_type,
            self.ffmpeg.clone(),
        );

        self.sessions.insert(session_id.clone(), new_session);

        info!(self.logger, "New session {} ({})", &session_id, stream_type);

        Ok(session_id)
    }

    #[handler]
    async fn chunk_init_request(&mut self, id: String, chunk: u32) -> Result<String> {
        let session = self.sessions.get_mut(&id).ok_or(NightfallError::SessionDoesntExist)?;

        if !session.is_chunk_done(chunk) {
            if session.start_num() != chunk {
                session.join();
                session.reset_to(chunk);
                session.start();

                let stat = self.stream_stats.entry(id).or_default();
                stat.hard_seeked_at = chunk;
                stat.last_hard_seek = Instant::now();
            }

            session.cont();
        }

        if !session.has_started() {
            session.start();
        }
        
        if session.is_chunk_done(chunk) {
            return Ok(session.init_seg());
        }

        Err(NightfallError::ChunkNotDone)
    }

    #[handler]
    async fn chunk_request(&mut self, id: String, chunk: u32) -> Result<String> {
        let session = self.sessions.get_mut(&id).ok_or(NightfallError::SessionDoesntExist)?;
        let stats = self.stream_stats.entry(id).or_default();

        if !session.has_started() {
            session.start();
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
                session.join();
                session.reset_to(chunk);
                session.start();

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
            Ok(chunk_path)
        }
    }

    #[handler]
    async fn chunk_eta(&mut self, id: String, chunk: u32) -> Result<u64> {
        let session = self.sessions.get_mut(&id).ok_or(NightfallError::SessionDoesntExist)?;
        Ok(session.eta_for(chunk).as_secs())
    }

    #[handler]
    async fn should_hard_seek(&mut self, id: String, chunk: u32) -> Result<bool> {
        let session = self.sessions.get_mut(&id).ok_or(NightfallError::SessionDoesntExist)?;
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
        let session = self.sessions.get_mut(&id).ok_or(NightfallError::SessionDoesntExist)?;
        info!(self.logger, "Killing session {}", id);
        session.join();
        session.set_timeout();
        
        Ok(())
    }

    #[handler]
    async fn get_sub(&mut self, id: String, name: String) -> Result<String> {
        let session = self.sessions.get_mut(&id).ok_or(NightfallError::SessionDoesntExist)?;

        if !session.has_started() {
            session.start();
        }

        session.subtitle(name).ok_or(NightfallError::ChunkNotDone)
    }

    #[handler]
    async fn get_stderr(&mut self, id: String) -> Result<String> {
        // TODO: Move this out of here, instead we should just return the log file.
        let session = self.sessions.get_mut(&id).ok_or(NightfallError::SessionDoesntExist)?;
        session.stderr().ok_or(NightfallError::Aborted)
    }

    #[handler]
    async fn garbage_collect(&mut self) -> Result<()> {
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
            self.exit_statuses.insert(k.clone(), v.stderr().unwrap_or_default());
            v.join();
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
}
