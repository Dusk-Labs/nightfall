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
#![feature(try_trait, result_flattening, array_value_iter)]
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

use xtra::*;
use std::collections::HashMap;
use async_trait::async_trait;

struct StreamStat {
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

pub struct StateManager {
    /// The directory where we store stream artifacts
    outdir: String,
    /// Path to a `ffmpeg` binary.
    ffmpeg: String,
    /// Contains all of the sessions currently managed by this actor.
    sessions: HashMap<String, Session>,
    /// Contains some useful stream stats
    stream_stats: HashMap<String, StreamStat>,
}

impl Actor for StateManager {}

impl StateManager {
    pub fn new(outdir: String, ffmpeg: String) -> Self {
        Self {
            outdir,
            ffmpeg,
            sessions: HashMap::new(),
            stream_stats: HashMap::new(),
        }
    }

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

        Ok(session_id)
    }

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
            return Ok(session.chunk_to_path(chunk));
        }

        Err(NightfallError::ChunkNotDone)
    }

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

    async fn chunk_eta(&mut self, id: String, chunk: u32) -> Result<u64> {
        let session = self.sessions.get_mut(&id).ok_or(NightfallError::SessionDoesntExist)?;
        Ok(session.eta_for(chunk).as_secs())
    }

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

    async fn die(&mut self, id: String) -> Result<()> {
        let session = self.sessions.get_mut(&id).ok_or(NightfallError::SessionDoesntExist)?;
        session.join();
        session.set_timeout();
        
        Ok(())
    }

    async fn get_sub(&mut self, id: String, name: String) -> Result<String> {
        let session = self.sessions.get_mut(&id).ok_or(NightfallError::SessionDoesntExist)?;

        if !session.has_started() {
            session.start();
        }

        session.subtitle(name).ok_or(NightfallError::ChunkNotDone)
    }

    async fn get_stderr(&mut self, id: String) -> Result<String> {
        let session = self.sessions.get_mut(&id).ok_or(NightfallError::SessionDoesntExist)?;
        // FIXME: Return status if no stderr exists
        /*
          .or_else(|_| {
              self.exit_statuses
                  .get(&session_id)
                  .map(|x| x.value().clone())
                  .ok_or(NightfallError::Aborted)
           })
        */
        session.stderr().ok_or(NightfallError::Aborted)
    }
}

pub struct StateCreate {
    stream_type: StreamType,
    file: String
}

impl Message for StateCreate {
    type Result = Result<String>;
}

#[async_trait]
impl Handler<StateCreate> for StateManager {
    async fn handle(&mut self, args: StateCreate, _: &mut Context<Self>) -> Result<String> {
        self.create(args.stream_type, args.file).await
    }
}

pub struct ChunkInitRequest(String, u32);

impl Message for ChunkInitRequest {
    type Result = Result<String>;
}

#[async_trait]
impl Handler<ChunkInitRequest> for StateManager {
    async fn handle(&mut self, args: ChunkInitRequest, _: &mut Context<Self>) -> Result<String> {
        self.chunk_init_request(args.0, args.1).await
    }
}

pub struct ChunkRequest(String, u32);

impl Message for ChunkRequest {
    type Result = Result<String>;
}

#[async_trait]
impl Handler<ChunkRequest> for StateManager {
    async fn handle(&mut self, args: ChunkRequest, _: &mut Context<Self>) -> Result<String> {
        self.chunk_request(args.0, args.1).await
    }
}

pub struct ChunkEta(String, u32);

impl Message for ChunkEta {
    type Result = Result<u64>;
}

#[async_trait]
impl Handler<ChunkEta> for StateManager {
    async fn handle(&mut self, args: ChunkEta, _: &mut Context<Self>) -> Result<u64> {
        self.chunk_eta(args.0, args.1).await
    }
}

pub struct ShouldClientHardSeek(String, u32);

impl Message for ShouldClientHardSeek {
    type Result = Result<bool>;
}

#[async_trait]
impl Handler<ShouldClientHardSeek> for StateManager {
    async fn handle(&mut self, args: ShouldClientHardSeek, _: &mut Context<Self>) -> Result<bool> {
        self.should_hard_seek(args.0, args.1).await
    }
}

pub struct Die(String);

impl Message for Die {
    type Result = Result<()>;
}

#[async_trait]
impl Handler<Die> for StateManager {
    async fn handle(&mut self, args: Die, _: &mut Context<Self>) -> Result<()> {
        self.die(args.0).await
    }
}

pub struct GetSub(String, String);

impl Message for GetSub {
    type Result = Result<String>;
}

#[async_trait]
impl Handler<GetSub> for StateManager {
    async fn handle(&mut self, args: GetSub, _: &mut Context<Self>) -> Result<String> {
        self.get_sub(args.0, args.1).await
    }
}

pub struct GetStderr(String);

impl Message for GetStderr {
    type Result = Result<String>;
}

#[async_trait]
impl Handler<GetStderr> for StateManager {
    async fn handle(&mut self, args: GetStderr, _: &mut Context<Self>) -> Result<String> {
        self.get_stderr(args.0).await
    }
}

pub struct GarbageCollect;

impl Message for GarbageCollect {
    type Result = ();
}

#[async_trait]
impl Handler<GarbageCollect> for StateManager {
    async fn handle(&mut self, _: GarbageCollect, _: &mut Context<Self>) {
        fn collect(_: &String, session: &mut Session) -> bool {
            if session.is_hard_timeout() {
                // exit_statuses_clone.insert(k.clone(), v.stderr().unwrap_or_default());
                session.join();
                session.delete_tmp();
                return false;
            } else if session.try_wait() {
                // exit_statuses_clone.insert(k.clone(), v.stderr().unwrap_or_default());
                return true;
            }

            true
        }

        self.sessions.retain(collect);

        for (_, v) in self.sessions.iter_mut() {
            if v.is_timeout() && !v.paused && !v.try_wait() {
                v.pause();
            }
        }
    }
}
