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

use std::sync::atomic::Ordering::SeqCst;
use std::sync::Arc;
use std::sync::RwLock;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::time::Instant;

use crossbeam::channel::unbounded;
use crossbeam::channel::Receiver;
use crossbeam::channel::Sender;

use dashmap::DashMap;

/// Represents a operation that a route can dispatch to the state manager.
#[derive(Debug)]
#[non_exhaustive]
pub enum OpCode {
    /// Represents a request for a init chunk.
    ChunkInitRequest {
        chunk: u32,
        chan: Sender<Result<String>>,
    },
    /// This operation is used when a client has requested a chunk of a stream.
    ChunkRequest {
        chunk: u32,
        chan: Sender<Result<String>>,
    },
    /// This operation is used to find out the ETA of a chunk.
    ChunkEta {
        chunk: u32,
        chan: Sender<Result<u64>>,
    },
    /// Operation is used to determine whether the client should hard seek or not
    ///
    /// FIXME: This opcode is mainly here for compatibility with dash.js, when seeking in browsers we
    /// require the reload of a manifest to avoid the player freezing and entering a request next
    /// chunk loop.
    ShouldClientHardSeek {
        chunk: u32,
        chan: Sender<Result<bool>>,
    },
    Die {
        chan: Sender<Result<()>>,
    },
    GetStderr {
        chan: Sender<Result<String>>,
    },
    GetSub {
        name: String,
        chan: Sender<Result<String>>,
    },
}

impl OpCode {
    pub fn cancel(&self) {
        let msg = Err(NightfallError::Aborted);
        match self {
            Self::ChunkInitRequest { chan, .. } => {
                chan.send(msg);
            }
            Self::ChunkRequest { chan, .. } => {
                chan.send(msg);
            }
            Self::ChunkEta { chan, .. } => {
                chan.send(Err(NightfallError::Aborted));
            }
            Self::ShouldClientHardSeek { chan, .. } => {
                chan.send(Err(NightfallError::Aborted));
            }
            Self::Die { chan, .. } => {
                chan.send(Ok(()));
            }
            Self::GetStderr { chan, .. } => {
                chan.send(msg);
            }
            Self::GetSub { chan, .. } => {
                chan.send(msg);
            }
        }
    }
}

/// This is our state manager. It keeps track of all of our transcoding sessions.
/// Cleans up sessions that have time outted.
pub struct StateManager {
    /// The directory where we store session artifacts.
    outdir: String,
    /// Path to ffmpeg on disk.
    ffmpeg_bin: String,
    /// Path to ffprobe on disk.
    ffprobe_bin: String,

    /// Contains all of our sessions keyed by their session id.
    sessions: Arc<DashMap<String, Session>>,
    /// Contains all of our Sender channels keyed by their respective session id.
    /// When we want to request a chunk or get information on a session we have to look up a sender
    /// in this map first.
    chunk_requester: Arc<DashMap<String, Sender<OpCode>>>,
    /// This is the receiver side of our operations. Each thread serves one session and basically
    /// answers all operations and answers them accordingly.
    session_monitors: Arc<RwLock<Vec<JoinHandle<()>>>>,
    /// Cleaner thread reaps sessions that have time outed.
    cleaner: Arc<JoinHandle<()>>,
    /// Exit status
    exit_statuses: Arc<DashMap<String, String>>,
}

impl StateManager {
    pub fn new(outdir: String, ffmpeg_bin: String, ffprobe_bin: String) -> Self {
        let sessions = Arc::new(DashMap::new());
        let exit_statuses = Arc::new(DashMap::new());
        let map_clone = Arc::clone(&sessions);
        let exit_statuses_clone = Arc::clone(&exit_statuses);

        Self {
            outdir,
            sessions,
            ffmpeg_bin,
            ffprobe_bin,
            exit_statuses,

            chunk_requester: Arc::new(DashMap::new()),
            session_monitors: Arc::new(RwLock::new(Vec::new())),

            cleaner: Arc::new(thread::spawn(move || loop {
                map_clone.retain(|k, v| {
                    if v.is_hard_timeout() {
                        exit_statuses_clone.insert(k.clone(), v.stderr().unwrap_or_default());
                        v.join();
                        v.delete_tmp();
                        return false;
                    } else if v.try_wait() {
                        exit_statuses_clone.insert(k.clone(), v.stderr().unwrap_or_default());
                        return true;
                    }

                    true
                });
                for v in map_clone.iter() {
                    if v.is_timeout() && !v.paused.load(SeqCst) && !v.try_wait() {
                        v.pause();
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
        let mut last_hard_seek = Instant::now();
        let mut hard_seeked_at = 0;

        while let Ok(item) = rx.recv() {
            let session = match sessions.get(&session_id) {
                Some(x) => x,
                None => {
                    item.cancel();
                    return;
                }
            };

            if let OpCode::ChunkInitRequest { chunk, chan } = item {
                if !session.is_chunk_done(chunk) {
                    // if the chunk isnt done and start_num isnt what we expect we hard seek
                    if session.start_num() != chunk {
                        session.join();
                        session.reset_to(chunk);
                        session.start();

                        hard_seeked_at = chunk;
                        last_hard_seek = Instant::now();
                    }

                    if session.paused.load(SeqCst) {
                        session.cont();
                    }

                    chan.send(Err(NightfallError::ChunkNotDone));
                } else {
                    let chunk_path = session.chunk_to_path(session.start_num());
                    chan.send(Ok(chunk_path));
                }

                continue;
            }

            if let OpCode::ChunkRequest { chunk, chan } = item {
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
                            && Instant::now() < last_hard_seek + Duration::from_secs(15)
                            && chunk > hard_seeked_at)
                        || eta > eta_tol;

                    // if we get here, and the session is paused we need to start it again.
                    if session.paused.load(SeqCst) {
                        session.cont();
                    }

                    if should_hard_seek {
                        session.join();
                        session.reset_to(chunk);
                        session.start();

                        last_hard_seek = Instant::now();
                        hard_seeked_at = chunk;
                    }

                    chan.send(Err(NightfallError::ChunkNotDone));
                } else {
                    let chunk_path = session.chunk_to_path(chunk);

                    // hint that we should probably unpause ffmpeg for a bit
                    if chunk + 2 >= session.current_chunk() {
                        session.cont();
                    }

                    session.reset_timeout(chunk);
                    chan.send(Ok(chunk_path));
                }

                continue;
            }

            if let OpCode::ChunkEta { chunk, chan } = item {
                chan.send(Ok(session.eta_for(chunk).as_secs()));
                continue;
            }

            if let OpCode::ShouldClientHardSeek { chunk, chan } = item {
                if !session.has_started() {
                    chan.send(Ok(false));
                    continue;
                }
                // if we are seeking backwards we always want to restart the stream
                // This is because our init.mp4 gets overwritten if we seeked forward at some point
                // Furthermore we want to hard seek anyway if the player is browser based.
                if chunk < session.start_num() {
                    chan.send(Ok(true));
                    continue;
                }

                // FIXME: When we hard seek and start a new ffmpeg session for some reason ffmpeg
                // reports invalid speed but then evens out. The problem is that causes seeking
                // multiple times in a row to be very slow.
                // thus for like the first 10s after a hard seek we exclusively hard seek if the
                // target is over 10 chunks into the future.
                if chunk > session.current_chunk() + 15
                    && Instant::now() < last_hard_seek + Duration::from_secs(15)
                {
                    chan.send(Ok(true));
                    continue;
                }

                chan.send(Ok((session.eta_for(chunk).as_millis() as f64)
                    > (10_000.0 / session.raw_speed()).max(5_000.0)));

                continue;
            }

            if let OpCode::Die { chan } = item {
                session.join();
                session.set_timeout();
                chan.send(Ok(()));
                continue;
            }

            if let OpCode::GetSub { name, chan } = item {
                chan.send(session.subtitle(name).ok_or(NightfallError::ChunkNotDone));
                continue;
            }

            if let OpCode::GetStderr { chan } = item {
                chan.send(session.stderr().ok_or(NightfallError::Aborted));
            }
        }
    }

    /// Function creates a stopped session returning an id.
    pub fn create(&self, file: String, stream_type: StreamType) -> String {
        let session_id = uuid::Uuid::new_v4().to_hyphenated().to_string();

        let new_session = Session::new(
            session_id.clone(),
            file,
            0,
            format!("{}/{}", self.outdir.clone(), session_id.clone()),
            stream_type,
            self.ffmpeg_bin.clone(),
        );

        self.sessions.insert(session_id.clone(), new_session);

        session_id
    }

    pub fn init_create(&self, session_id: String) -> Sender<OpCode> {
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
        if let Some(x) = self.sessions.get(&session_id) {
            x.start();
        }

        session_tx
    }

    /// Try to get the init segment of a stream.
    pub fn init_or_create(&self, session_id: String, first_chunk: u32) -> Result<String> {
        let session_tx = if self
            .sessions
            .get(&session_id)
            .ok_or(NightfallError::SessionDoesntExist)?
            .has_started()
        {
            self.chunk_requester
                .get(&session_id)
                .ok_or(NightfallError::SessionDoesntExist)?
                .value()
                .clone()
        } else {
            self.init_create(session_id.clone())
        };

        let (tx, rx) = unbounded();
        let chunk_request = OpCode::ChunkInitRequest {
            chunk: first_chunk,
            chan: tx,
        };
        session_tx.send(chunk_request);

        // we got here, that means chunk 0 is done.
        let _path = rx
            .recv()
            .map_err(|_| NightfallError::SessionManagerDied)
            .flatten()?;

        let session = self
            .sessions
            .get(&session_id)
            .ok_or(NightfallError::SessionDoesntExist)?;

        Ok(session.init_seg())
    }

    /// Method takes in a session id and chunk and will block until the chunk requested is ready or
    /// until a timeout.
    pub fn get_segment(&self, session_id: String, chunk: u32) -> Result<String> {
        let sender = self
            .chunk_requester
            .get(&session_id)
            .ok_or(NightfallError::SessionDoesntExist)?;

        let (chan, rx) = unbounded();
        sender.send(OpCode::ChunkRequest { chunk, chan });

        rx.recv().map_err(|_| NightfallError::Aborted).flatten()
    }

    pub fn exists(&self, session_id: String) -> Result<()> {
        self.chunk_requester
            .get(&session_id)
            .ok_or(NightfallError::SessionDoesntExist)?;
        Ok(())
    }

    pub fn eta_for_seg(&self, session_id: String, chunk: u32) -> Result<u64> {
        let sender = self
            .chunk_requester
            .get(&session_id)
            .ok_or(NightfallError::SessionDoesntExist)?;

        let (chan, rx) = unbounded();
        sender.send(OpCode::ChunkEta { chunk, chan });

        rx.recv().map_err(|_| NightfallError::Aborted).flatten()
    }

    pub fn should_client_hard_seek(&self, session_id: String, chunk: u32) -> Result<bool> {
        let sender = self
            .chunk_requester
            .get(&session_id)
            .ok_or(NightfallError::SessionDoesntExist)?;

        let (chan, rx) = unbounded();
        sender.send(OpCode::ShouldClientHardSeek { chunk, chan });

        rx.recv().map_err(|_| NightfallError::Aborted).flatten()
    }

    pub fn get_stderr(&self, session_id: String) -> Result<String> {
        let sender = self
            .chunk_requester
            .get(&session_id)
            .ok_or(NightfallError::SessionDoesntExist)?;

        let (chan, rx) = unbounded();
        sender.send(OpCode::GetStderr { chan });

        rx.recv()
            .map_err(|_| NightfallError::Aborted)
            .flatten()
            .or_else(|_| {
                self.exit_statuses
                    .get(&session_id)
                    .map(|x| x.value().clone())
                    .ok_or(NightfallError::Aborted)
            })
    }

    pub fn kill(&self, session_id: String) -> Result<()> {
        let sender = self
            .chunk_requester
            .get(&session_id)
            .ok_or(NightfallError::SessionDoesntExist)?;

        let (chan, rx) = unbounded();
        sender.send(OpCode::Die { chan });

        rx.recv().map_err(|_| NightfallError::Aborted).flatten()
    }

    pub fn subtitle(&self, session_id: String, name: String) -> Result<String> {
        let sender = self
            .chunk_requester
            .get(&session_id)
            .ok_or(NightfallError::SessionDoesntExist)?;

        let (chan, rx) = unbounded();
        sender.send(OpCode::GetSub { name, chan });

        rx.recv().map_err(|_| NightfallError::Aborted).flatten()
    }
}
