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
#![feature(try_trait)]
#![allow(unused_must_use, dead_code)]

pub mod error;
pub mod ffprobe;
pub mod profile;
mod session;

use crate::error::*;
use crate::profile::*;
use crate::session::Session;
use dashmap::DashMap;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub struct StateManager {
    outdir: String,
    sessions: Arc<DashMap<String, Session>>,
    cleaner: JoinHandle<()>,
}

impl StateManager {
    pub fn new(outdir: String) -> Self {
        let map = Arc::new(DashMap::new());
        let map_clone = Arc::clone(&map);
        Self {
            outdir,
            sessions: map,
            cleaner: thread::spawn(move || loop {
                for v in map_clone.iter() {
                    if v.is_timeout() {
                        v.join();
                    }
                }
                thread::sleep(Duration::from_millis(100));
            }),
        }
    }

    /// Function creates a stopped session returning an id.
    pub fn create(&self, file: String, profile: Profile, stream_type: StreamType) -> String {
        let session_id = uuid::Uuid::new_v4().to_hyphenated().to_string();

        let new_session = Session::new(
            session_id.clone(),
            file,
            profile,
            0,
            format!("{}/{}", self.outdir.clone(), session_id.clone()),
            stream_type,
        );

        self.sessions.insert(session_id.clone(), new_session);

        session_id
    }

    /// Attempt to get a transcoded chunk.
    pub fn try_get(&self, session_id: String, chunk: u64) -> Result<String> {
        let session = self
            .sessions
            .get(&session_id)
            .ok_or(NightfallError::SessionDoesntExist)?;

        if session.is_chunk_done(chunk) {
            session.reset_timeout(chunk);
            return Ok(session.chunk_to_path(chunk));
        }

        Err(NightfallError::ChunkNotDone)
    }

    /// Attempt to get a transcoded chunk with a timeout.
    pub fn get_timeout(&self, session_id: String, chunk: u64, timeout: Duration) -> Result<String> {
        let deadline = match Instant::now().checked_add(timeout) {
            Some(x) => x,
            None => return self.try_get(session_id, chunk),
        };

        loop {
            match self.try_get(session_id.clone(), chunk) {
                Ok(x) => return Ok(x),
                Err(e @ NightfallError::SessionDoesntExist) => return Err(e),
                Err(_) => {}
            }

            if Instant::now() >= deadline {
                return Err(NightfallError::Timeout);
            }
        }
    }

    /// Function starts unstarted streams, waits for a chunk, returns it or otherwise starts a new
    /// session and tries again.
    pub fn get_or_create(
        &self,
        session_id: String,
        chunk: u64,
        timeout: Duration,
    ) -> Result<String> {
        // first check if a stream has even been started and if it wasnt started, start it
        let session = self
            .sessions
            .get(&session_id)
            .ok_or(NightfallError::SessionDoesntExist)?;

        // we only start unstarted streams when we get requests for the first chunk.
        if !session.has_started() && chunk == 0 {
            self.sessions.update(&session_id, |_, v| {
                let mut v = v.to_new();
                v.start();
                v
            });
        }

        // attempt to grab a chunk.
        match self.get_timeout(session_id.clone(), chunk, timeout) {
            Err(NightfallError::Timeout) => {}
            e @ Ok(_) | e @ Err(_) => return e,
        }

        self.sessions.update(&session_id, |_, v| {
            // first stop the old session
            v.join();

            let mut new_session = v.to_new_with_chunk(chunk);
            new_session.start();
            new_session
        });

        self.get_timeout(session_id, chunk, timeout)
            .map_err(|_| NightfallError::Aborted)
    }

    /// Try to get the init segment of a stream.
    pub fn init_or_create(&self, session_id: String, timeout: Duration) -> Result<String> {
        match self.get_or_create(session_id.clone(), 0, timeout) {
            Ok(_) => {
                let session = self
                    .sessions
                    .get(&session_id)
                    .ok_or(NightfallError::SessionDoesntExist)?;

                Ok(session.init_seg())
            }
            e @ Err(_) => e,
        }
    }
}
