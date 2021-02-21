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
#![feature(try_trait, result_flattening, peekable_next_if)]
#![allow(unused_must_use, dead_code)]

pub mod error;
pub mod ffprobe;
pub mod profile;
mod session;

use crate::error::*;
use crate::profile::*;
use crate::session::Session;

use std::sync::atomic::Ordering::SeqCst;
use std::sync::Arc;
use std::sync::RwLock;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam::channel::unbounded;
use crossbeam::channel::Receiver;
use crossbeam::channel::Sender;

use dashmap::DashMap;

pub enum OpCode {
    ChunkRequest {
        chunk: u64,
        chan: Sender<Result<String>>,
    },
    ChunkEta {
        chunk: u64,
        chan: Sender<Result<u64>>,
    },
    ShouldClientHardSeek {
        chunk: u64,
        chan: Sender<Result<bool>>,
    },
}

pub struct StateManager {
    outdir: String,
    ffmpeg_bin: String,
    ffprobe_bin: String,
    sessions: Arc<DashMap<String, Session>>,
    chunk_requester: Arc<DashMap<String, Sender<OpCode>>>,
    session_monitors: Arc<RwLock<Vec<JoinHandle<()>>>>,
    cleaner: Arc<JoinHandle<()>>,
}

impl StateManager {
    pub fn new(outdir: String, ffmpeg_bin: String, ffprobe_bin: String) -> Self {
        let sessions = Arc::new(DashMap::new());
        let map_clone = Arc::clone(&sessions);

        Self {
            outdir,
            sessions,
            ffmpeg_bin,
            ffprobe_bin,

            chunk_requester: Arc::new(DashMap::new()),
            session_monitors: Arc::new(RwLock::new(Vec::new())),

            cleaner: Arc::new(thread::spawn(move || loop {
                for v in map_clone.iter() {
                    if v.is_timeout() {
                        v.join();
                    }
                }
                thread::sleep(Duration::from_millis(10));
            })),
        }
    }

    fn session_monitor(
        session_id: String,
        rx: Receiver<OpCode>,
        sessions: Arc<DashMap<String, Session>>,
    ) {
        let mut rx = rx.iter().peekable();

        loop {
            // peek whether the next item is ready
            let session = sessions.get(&session_id).unwrap();

            let item = rx.next_if(|op| {
                if let OpCode::ChunkRequest { chunk, .. } = op {
                    return session.is_chunk_done(*chunk);
                }

                true
            });

            if let Some(OpCode::ChunkRequest { chunk, chan }) = item {
                let chunk_path = session.chunk_to_path(chunk);
                session.reset_timeout(chunk);
                chan.send(Ok(chunk_path));
                continue;
            }

            if let Some(OpCode::ChunkEta { chunk, chan }) = item {
                chan.send(Ok(session.eta_for(chunk).as_secs()));
                continue;
            }

            if let Some(OpCode::ShouldClientHardSeek { chunk, chan }) = item {
                // if we are seeking backwards we always want to restart the stream
                // This is because our init.mp4 gets overwritten if we seeked forward at some point
                // Furthermore we want to hard seek anyway if the player is browser based.
                if chunk < session.start_number {
                    chan.send(Ok(true));
                    continue;
                }

                chan.send(Ok((session.eta_for(chunk).as_millis() as f64)
                    > (10_000.0 / session.raw_speed()).max(5_000.0)));

                continue;
            }

            // check the eta of the next chunk
            if let Some(OpCode::ChunkRequest { chunk, .. }) = rx.peek() {
                // if the chunk being requested is less than the starting chunk of this session we
                // want to hard seek.
                if *chunk < session.start_number {
                    sessions.update(&session_id, |_, v| {
                        v.join();

                        let mut new_session = v.to_new_with_chunk(*chunk);
                        new_session.start();
                        new_session
                    });
                }

                // we tolerate a max eta of (10 / raw_speed).
                // if speed is 1.0x then eta will be 10s.
                // if the session is paused but we have a incoming segment ask we start the session
                if session.eta_for(*chunk).as_millis() as f64
                    > (10000.0 / session.raw_speed()).max(5000.0)
                    || session.paused.load(SeqCst)
                {
                    sessions.update(&session_id, |_, v| {
                        v.join();

                        let mut new_session = v.to_new_with_chunk(*chunk);
                        new_session.start();
                        new_session
                    });
                }
            }

            // if we get here that means the chunk isnt done yet, so we sleep for a bit.
            thread::sleep(Duration::from_millis(100));
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
            self.ffmpeg_bin.clone(),
        );

        self.sessions.insert(session_id.clone(), new_session);

        session_id
    }

    fn init_create(&self, session_id: String) -> Sender<OpCode> {
        // first setup the session monitor
        let (session_tx, session_rx) = unbounded();
        let sessions = self.sessions.clone();
        let session_id_clone = session_id.clone();
        self.session_monitors
            .write()
            .unwrap()
            .push(thread::spawn(move || {
                Self::session_monitor(session_id_clone, session_rx, sessions);
            }));

        // insert the tx channel into our map
        self.chunk_requester
            .insert(session_id.clone(), session_tx.clone());

        // start transcoding
        self.sessions.update(&session_id, |_, v| {
            let mut v = v.to_new();
            v.start();
            v
        });

        session_tx
    }

    /// Try to get the init segment of a stream.
    pub fn init_or_create(&self, session_id: String, _: Duration) -> Result<String> {
        let session_tx = if self
            .sessions
            .get(&session_id)
            .ok_or(NightfallError::SessionDoesntExist)?
            .has_started()
        {
            self.chunk_requester
                .get(&session_id)
                .unwrap()
                .value()
                .clone()
        } else {
            self.init_create(session_id.clone())
        };

        let (tx, rx) = unbounded();
        let chunk_request = OpCode::ChunkRequest { chunk: 0, chan: tx };
        session_tx.send(chunk_request);

        // we got here, that means chunk 0 is done.
        let _path = rx.recv().unwrap();

        let session = self.sessions.get(&session_id).unwrap();

        Ok(session.init_seg())
    }

    /// Method takes in a session id and chunk and will block until the chunk requested is ready or
    /// until a timeout.
    pub fn get_segment(&self, session_id: String, chunk: u64) -> Result<String> {
        let sender = self
            .chunk_requester
            .get(&session_id)
            .ok_or(NightfallError::SessionDoesntExist)?;

        let (chan, rx) = unbounded();
        sender.send(OpCode::ChunkRequest { chunk, chan });

        rx.recv().unwrap()
    }

    pub fn exists(&self, session_id: String, chunk: u64) -> Result<()> {
        self.chunk_requester
            .get(&session_id)
            .ok_or(NightfallError::SessionDoesntExist)?;
        Ok(())
    }

    pub fn eta_for_seg(&self, session_id: String, chunk: u64) -> Result<u64> {
        let sender = self
            .chunk_requester
            .get(&session_id)
            .ok_or(NightfallError::SessionDoesntExist)?;

        let (chan, rx) = unbounded();
        sender.send(OpCode::ChunkEta { chunk, chan });

        rx.recv().unwrap()
    }

    pub fn should_client_hard_seek(&self, session_id: String, chunk: u64) -> Result<bool> {
        let sender = self
            .chunk_requester
            .get(&session_id)
            .ok_or(NightfallError::SessionDoesntExist)?;

        let (chan, rx) = unbounded();
        sender.send(OpCode::ShouldClientHardSeek { chunk, chan });

        rx.recv().unwrap()
    }
}
