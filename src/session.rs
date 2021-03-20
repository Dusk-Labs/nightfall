use crate::profile::Profile;
use crate::profile::StreamType;

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;

use std::io::BufRead;
use std::io::BufReader;
use std::path::Path;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;

use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::Arc;
use std::sync::RwLock;

use crossbeam::atomic::AtomicCell;
use stoppable_thread::{self, SimpleAtomicBool, StoppableHandle};

cfg_if::cfg_if! {
    if #[cfg(feature = "fs_events")] {
        use inotify::Event;
        use inotify::EventMask;
        use inotify::Inotify;
        use inotify::WatchMask;
    }
}

/// Length of a chunk in seconds.
const CHUNK_SIZE: u64 = 5;
/// Represents how many chunks we encode before we require a timeout reset.
/// Basically if within MAX_CHUNKS_AHEAD we do not get a timeout reset we kill the stream.
/// This can be tuned
const MAX_CHUNKS_AHEAD: u64 = 15;

lazy_static::lazy_static! {
    /// This static contains stats about each stream. It is a Map of maps containing k/v pairs
    /// parsed from the ffmpeg stdout. Each Map is keyed by a session id.
    pub static ref STREAMING_SESSION: Arc<RwLock<HashMap<String, HashMap<String, String>>>> =
        Arc::new(RwLock::new(HashMap::new()));
}

pub struct Session {
    pub id: String,
    file: String,
    profile: Profile,
    outdir: String,
    ffmpeg_bin: String,
    process: AtomicCell<Option<StoppableHandle<()>>>,

    has_started: AtomicBool,
    pub paused: AtomicBool,
    pub start_number: AtomicU64,
    stream_type: StreamType,
    last_chunk: AtomicU64,

    child_pid: AtomicCell<Option<u32>>,

    #[cfg(feature = "fs_events")]
    fs_watcher: AtomicCell<Option<Inotify>>,
    #[cfg(feature = "fs_events")]
    fs_events: Arc<RwLock<Vec<Event<String>>>>,
}

impl Session {
    pub fn new(
        id: String,
        file: String,
        profile: Profile,
        start_number: u64,
        outdir: String,
        stream_type: StreamType,
        ffmpeg_bin: String,
    ) -> Self {
        std::fs::create_dir(&outdir).unwrap();

        cfg_if::cfg_if! {
            if #[cfg(feature = "fs_events")] {
                let mut fs_watcher = Inotify::init().expect("Failed to init inotify");
                fs_watcher
                    .add_watch(&outdir, WatchMask::CLOSE_WRITE)
                    .unwrap();
            }
        }

        Self {
            id,
            outdir,
            profile,
            ffmpeg_bin,
            stream_type,
            start_number: AtomicU64::new(start_number),
            last_chunk: AtomicU64::new(0),
            process: AtomicCell::new(None),
            paused: AtomicBool::new(false),
            has_started: AtomicBool::new(false),
            child_pid: AtomicCell::new(None),
            file,

            #[cfg(feature = "fs_events")]
            fs_watcher: AtomicCell::new(Some(fs_watcher)),
            #[cfg(feature = "fs_events")]
            fs_events: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn start(&self) -> Result<(), io::Error> {
        // make sure we actually have a path to write files to.
        self.has_started.store(true, SeqCst);
        self.paused.store(false, SeqCst);
        let _ = fs::create_dir_all(self.outdir.clone());
        let args = self.build_args();

        let process = Command::new(self.ffmpeg_bin.clone())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .args(args.as_slice())
            .spawn()?;

        self.child_pid.store(Some(process.id()));

        let mut process = TranscodeHandler::new(self.id.clone(), process);

        self.process
            .store(Some(stoppable_thread::spawn(move |signal| {
                process.handle(signal)
            })));

        Ok(())
    }

    pub fn start_num(&self) -> u64 {
        self.start_number.load(SeqCst)
    }

    // FIXME: This entire subroutine will silently break streams that have non-standard sepcifications,
    // such as fps that isnt 24
    fn build_args(&self) -> Vec<&str> {
        let mut args = vec![
            "-ss",
            string_to_static_str((self.start_num() * CHUNK_SIZE).to_string()),
            "-i",
            self.file.as_str(),
        ];

        match self.stream_type {
            StreamType::Audio => {
                args.append(&mut vec![
                    "-copyts", "-map", "0:1", "-c:0", "aac", "-ac", "2", "-ab", "0", "-threads",
                    "1",
                ]);
            }
            StreamType::Video => {
                args.append(&mut vec!["-copyts", "-map", "0:0"]);
                args.append(&mut self.profile.to_params().0);
            }
            StreamType::Muxed => {
                args.append(&mut vec!["-copyts"]);
                args.append(&mut self.profile.to_params().0);
                args.append(&mut vec![
                    "-c:a", "copy", "-ac", "2", "-ab", "0", "-threads", "1",
                ]);
            }
        }

        // args needed to decrease the chances of a race condition when fetching a segment
        args.append(&mut vec!["-flush_packets", "1"]);

        args.append(&mut vec![
            "-f",
            "hls",
            "-start_number",
            string_to_static_str(self.start_num().to_string()),
        ]);

        // needed so that in progress segments are named `tmp` and then renamed after the data is
        // on disk.
        // This in theory practically prevents the web server from returning a segment that is
        // in progress.
        args.append(&mut vec!["-hls_flags", "temp_file"]);

        // args needed so we can distinguish between init fragments for new streams.
        // Basically on the web seeking works by reloading the entire video because of
        // discontinuity issues that browsers seem to not ignore like mpv.
        args.append(&mut vec![
            "-hls_fmp4_init_filename",
            string_to_static_str(format!("{}_init.mp4", self.start_num())),
        ]);

        args.append(&mut vec![
            "-hls_time",
            string_to_static_str(CHUNK_SIZE.to_string()),
            "-initial_offset",
            string_to_static_str((self.start_num() * CHUNK_SIZE).to_string()),
            "-reset_timestamps",
            "1",
            "-force_key_frames",
            "expr:gte(t,n_forced*5)",
        ]);

        args.append(&mut vec!["-hls_segment_type", "1"]);
        args.append(&mut vec!["-loglevel", "info", "-progress", "pipe:1"]);
        args.append(&mut vec![
            "-hls_segment_filename",
            string_to_static_str(format!("{}/%d.m4s", self.outdir)),
        ]);
        args.append(&mut vec![string_to_static_str(format!(
            "{}/playlist.m3u8",
            self.outdir
        ))]);

        args
    }

    pub fn join(&self) {
        if let Some(x) = self.process.take() {
            x.stop().join();
        }
    }

    pub fn pause(&self) {
        if let Some(x) = self.child_pid.load() {
            crate::utils::pause_proc(x as i32);
            self.paused.store(true, SeqCst);
        }
    }

    pub fn cont(&self) {
        if let Some(x) = self.child_pid.load() {
            crate::utils::cont_proc(x as i32);
            self.paused.store(false, SeqCst);
        }
    }

    pub fn get_key(&self, k: &str) -> Result<String, std::option::NoneError> {
        let session = STREAMING_SESSION.read().unwrap();
        Ok(session.get(&self.id)?.get(k)?.clone())
    }

    pub fn current_chunk(&self) -> u64 {
        let frame = match self.stream_type {
            StreamType::Audio => {
                self.get_key("out_time_us")
                    .map(|x| x.parse::<u64>().unwrap_or(0))
                    .unwrap_or(0)
                    / 1000
                    / 1000
                    * 24
            }
            StreamType::Video | StreamType::Muxed => self
                .get_key("frame")
                .map(|x| x.parse::<u64>().unwrap_or(0))
                .unwrap_or(0),
        };

        match self.stream_type {
            StreamType::Audio => (frame / (CHUNK_SIZE * 24)).max(self.last_chunk.load(SeqCst)),
            StreamType::Video | StreamType::Muxed => frame / (CHUNK_SIZE * 24) + self.start_num(),
        }
    }

    pub fn raw_speed(&self) -> f64 {
        self.get_key("speed")
            .map(|x| x.trim_end_matches('x').to_string())
            .and_then(|x| x.parse::<f64>().map_err(|_| std::option::NoneError))
            .unwrap_or(1.0) // assume if key is missing that our speed is 2.0
    }

    // returns how many chunks per second
    pub fn speed(&self) -> f64 {
        (self.raw_speed().floor().max(20.0) * 24.0) / (CHUNK_SIZE as f64 * 24.0)
    }

    pub fn eta_for(&self, chunk: u64) -> Duration {
        let cps = self.speed();

        let current_chunk = self.current_chunk() as f64;
        let diff = (chunk as f64 - current_chunk).abs();

        Duration::from_secs((diff / cps).abs().ceil() as u64)
    }

    /// Method does some math magic to guess if a chunk has been fully written by ffmpeg yet
    pub fn is_chunk_done(&self, chunk_num: u64) -> bool {
        cfg_if::cfg_if! {
            // if fs_events is enabled (as it should be on *nix) we use inotify to check whether a
            // chunk has been closed by ffmpeg, thus it is ready and wont result in a race
            // condition when returned.
            //
            // if fs_events isnt enabled (as it should be on windows) we estimate whether the chunk
            // is done by checking whether current_chunk is 5 chunks ahead of the chunk requested.
            //
            // NOTE: This will break when seeking, and can result in unstable playback.
            if #[cfg(feature = "fs_events")] {
                self.poll_events();
                self.check_inotify(&format!("{}.m4s", chunk_num), EventMask::CLOSE_WRITE)
            } else {
                dbg!(Path::new(&format!("{}/{}.m4s", &self.outdir, chunk_num)).is_file())
            }
        }
    }

    #[cfg(feature = "fs_events")]
    fn poll_events(&self) {
        let mut buf = [0; 8192];
        if let Some(mut fs_watcher) = self.fs_watcher.take() {
            let mut events = fs_watcher
                .read_events(&mut buf)
                .unwrap()
                .map(|x| Event {
                    wd: x.wd,
                    mask: x.mask,
                    cookie: x.cookie,

                    name: x.name.map(|x| x.to_str().unwrap().to_string()),
                })
                .collect::<Vec<_>>();

            self.fs_events.write().unwrap().append(&mut events);
            self.fs_watcher.swap(Some(fs_watcher));
        }
    }

    #[cfg(feature = "fs_events")]
    fn check_inotify(&self, path: &str, mask: EventMask) -> bool {
        !self
            .fs_events
            .read()
            .unwrap()
            .iter()
            .filter(|x| {
                if let Some(name) = &x.name {
                    x.mask == mask && name.as_str() == path
                } else {
                    false
                }
            })
            .collect::<Vec<_>>()
            .is_empty()
    }

    pub fn is_timeout(&self) -> bool {
        self.current_chunk() > self.last_chunk.load(SeqCst) + MAX_CHUNKS_AHEAD
    }

    pub fn reset_timeout(&self, last_requested: u64) {
        self.last_chunk.store(last_requested, SeqCst);
    }

    pub fn chunk_to_path(&self, chunk_num: u64) -> String {
        format!("{}/{}.m4s", self.outdir, chunk_num)
    }

    pub fn init_seg(&self) -> String {
        format!("{}/{}_init.mp4", self.outdir, self.start_num())
    }

    pub fn has_started(&self) -> bool {
        self.has_started.load(SeqCst)
    }

    pub fn reset_to(&self, chunk: u64) {
        self.start_number.store(chunk, SeqCst);
        self.process.take();
        self.last_chunk.store(chunk, SeqCst);
        self.has_started.store(false, SeqCst);
        self.paused.store(true, SeqCst);
        self.child_pid.take();
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id)
            .field("start_number", &self.start_number)
            .field("last_chunk", &self.last_chunk)
            .finish()
    }
}

struct TranscodeHandler {
    id: String,
    process: Child,
}

impl TranscodeHandler {
    fn new(id: String, process: Child) -> Self {
        Self { id, process }
    }

    fn handle(&mut self, signal: &SimpleAtomicBool) {
        let stdio = BufReader::new(self.process.stdout.take().unwrap());
        let mut map: HashMap<String, String> = HashMap::new();

        /*
            map.insert("frame".into(), "0".into());
            map.insert("fps".into(), "0.0".into());
            map.insert("stream_0_0_q".into(), "0.0".into());
            map.insert("bitrate".into(), "0.0kbits/s".into());
            map.insert("total_size".into(), "0".into());
            map.insert("out_time_ms".into(), "0".into());
            map.insert("out_time".into(), "00:00:00.000000".into());
            map.insert("dup_frames".into(), "0".into());
            map.insert("drop_frames".into(), "0".into());
            map.insert("speed".into(), "0.00x".into());
            map.insert("progress".into(), "continue".into());
        */

        let mut stdio_b = stdio.lines().peekable();

        'stdout: while !signal.get() {
            if stdio_b.peek().is_some() {
                let output = stdio_b.next().unwrap().unwrap();
                let output: Vec<&str> = output.split('=').collect();

                // remove whitespace on both ends
                map.insert(output[0].into(), output[1].trim_start().trim_end().into());

                {
                    let mut lock = STREAMING_SESSION.write().unwrap();
                    let _ = lock.insert(self.id.clone(), map.clone());
                }
            }

            match self.process.try_wait() {
                Ok(Some(_)) => break 'stdout,
                Ok(None) => {}
                Err(x) => println!("handle_stdout got err on try_wait(): {:?}", x),
            }

            // sleep is necessary to avoid a deadlock.
            std::thread::sleep(Duration::from_millis(50));
        }

        println!("Broken out of stdout loop, killing ffmpeg");

        let _ = self.process.kill();
        let _ = self.process.wait();

        let mut lock = STREAMING_SESSION.write().unwrap();
        let _ = lock.remove(&self.id);
    }
}

fn string_to_static_str(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}
