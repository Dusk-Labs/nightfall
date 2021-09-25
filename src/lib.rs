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
/// Contains utils that patch segments to make them appear continuous.
pub mod patch;
/// Contains all profiles currently implemented.
pub mod profiles;
/// Contains the struct representing a streaming session.
mod session;
/// Contains utils that make my life easier.
pub mod utils;

use crate::error::*;
use crate::patch::segment::patch_segment;
use crate::profiles::*;
use crate::session::Session;

use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use xtra_proc::actor;
use xtra_proc::handler;

use slog::debug;
use slog::info;
use slog::warn;

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
        profile_chain: Vec<&'static dyn TranscodingProfile>,
        profile_args: ProfileContext,
    ) -> Result<String> {
        let mut profile_args = profile_args;

        let first_tag = profile_chain.first().expect("Empty profile chain.").tag();
        let chain = profile_chain
            .iter()
            .map(|x| x.tag())
            .collect::<Vec<_>>()
            .join(" -> ");

        let session_id = uuid::Uuid::new_v4().to_hyphenated().to_string();
        let tag = if let Some(width) = profile_args.output_ctx.width {
            let bitrate = profile_args
                .output_ctx
                .bitrate
                .map(|x| format!("@{}", x))
                .unwrap_or_default();
            let height = profile_args.output_ctx.height.unwrap_or(-2);

            format!("{} ({}x{}{})", &first_tag, width, height, bitrate)
        } else {
            let bitrate = profile_args
                .output_ctx
                .bitrate
                .map(|x| format!("@{}", x))
                .unwrap_or_default();
            format!("{}{}", &first_tag, bitrate)
        };

        info!(
            self.logger,
            "New session {} map {} -> {}", &session_id, profile_args.input_ctx.stream, tag
        );
        info!(self.logger, "Session {} chain {}", &session_id, chain);

        profile_args.output_ctx.outdir = format!("{}/{}", &self.outdir, session_id);
        profile_args.ffmpeg_bin = self.ffmpeg.clone();

        let new_session = Session::new(session_id.clone(), profile_chain, profile_args);

        self.sessions.insert(session_id.clone(), new_session);

        Ok(session_id)
    }

    #[handler]
    async fn chunk_init_request(&mut self, id: String, chunk: u32) -> Result<String> {
        let session = self
            .sessions
            .get_mut(&id)
            .ok_or(NightfallError::SessionDoesntExist)?;

        // If ffmpeg abrupty closes we want to move down the profile chain and try other profiles
        // until we get something that works or we exhaust all our profiles.
        if let Some(status) = session.exit_status.as_ref() {
            if !status.success() {
                if let Some(x) = session.next_profile() {
                    info!(
                        self.logger,
                        "Session {}<{}>trying profile {}", &id, chunk, x
                    );
                    session.reset_to(session.start_num());
                } else {
                    return Err(NightfallError::ProfileChainExhausted);
                }
            }
        }

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
        let stats = self.stream_stats.entry(id.clone()).or_default();

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

                debug!(
                    self.logger,
                    "Resetting {} to chunk {} because user seeked.", &id, chunk
                );
            }

            Err(NightfallError::ChunkNotDone)
        } else {
            let chunk_path = session.chunk_to_path(chunk);
            let log = self.logger.clone();
            let path = chunk_path.clone();
            let real_segment = session.real_segment;

            // hint that we should probably unpause ffmpeg for a bit
            if chunk + 2 >= session.current_chunk() {
                session.cont();
            }

            match patch_segment(log, path, real_segment).await {
                Ok(seq) => session.real_segment = seq,
                Err(e) => warn!(self.logger, "Failed to patch segment."; "error" => e.to_string()),
            }

            session.reset_timeout(chunk);

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
