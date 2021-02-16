use crate::profile::Profile;
use crate::profile::StreamType;
use crossbeam::atomic::AtomicCell;
use std::fmt;
use std::sync::atomic::AtomicBool;
use std::time::Instant;
use std::{
    collections::HashMap,
    fs,
    io::{self, BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering::SeqCst},
        Arc, RwLock,
    },
    time::Duration,
};
use stoppable_thread::{self, SimpleAtomicBool, StoppableHandle};

cfg_if::cfg_if! {
    if #[cfg(feature = "fs_events")] {
        use inotify::Event;
        use inotify::EventMask;
        use inotify::Inotify;
        use inotify::WatchMask;
    }
}

const CHUNK_SIZE: u64 = 5;
/// Represents how many chunks we encode before we require a timeout reset.
/// Basically if within MAX_CHUNKS_AHEAD we do not get a timeout reset we kill the stream.
/// This can be tuned
const MAX_CHUNKS_AHEAD: u64 = 15;

lazy_static::lazy_static! {
    pub static ref STREAMING_SESSION: Arc<RwLock<HashMap<String, HashMap<String, String>>>> =
        Arc::new(RwLock::new(HashMap::new()));
}

pub struct Session {
    pub id: String,
    file: String,
    profile: Profile,
    outdir: String,
    process: AtomicCell<Option<StoppableHandle<()>>>,
    has_started: bool,
    pub paused: AtomicBool,
    start_number: u64,
    stream_type: StreamType,
    last_chunk: AtomicU64,

    #[cfg(feature = "fs_events")]
    fs_watcher: AtomicCell<Option<Inotify>>,
    #[cfg(feature = "fs_events")]
    fs_events: Arc<RwLock<Vec<Event<String>>>>,

    last_reset: AtomicCell<Instant>,
}

impl Session {
    pub fn new(
        id: String,
        file: String,
        profile: Profile,
        start_number: u64,
        outdir: String,
        stream_type: StreamType,
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
            start_number,
            profile,
            stream_type,
            last_chunk: AtomicU64::new(0),
            process: AtomicCell::new(None),
            paused: AtomicBool::new(false),
            has_started: false,
            file: format!("file://{}", file),

            #[cfg(feature = "fs_events")]
            fs_watcher: AtomicCell::new(Some(fs_watcher)),
            #[cfg(feature = "fs_events")]
            fs_events: Arc::new(RwLock::new(Vec::new())),

            last_reset: AtomicCell::new(Instant::now()),
        }
    }

    pub fn start(&mut self) -> Result<(), io::Error> {
        // make sure we actually have a path to write files to.
        self.has_started = true;
        self.paused.store(false, SeqCst);
        let _ = fs::create_dir_all(self.outdir.clone());
        let args = self.build_args();
        let mut process = Command::new("/usr/bin/ffmpeg");

        process
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .args(args.as_slice());

        let mut process = TranscodeHandler::new(self.id.clone(), process.spawn()?);
        self.process = AtomicCell::new(Some(stoppable_thread::spawn(move |signal| {
            process.handle(signal)
        })));
        Ok(())
    }

    // FIXME: This entire subroutine will silently break streams that have non-standard sepcifications,
    // such as fps that isnt 24
    fn build_args(&self) -> Vec<&str> {
        let mut args = vec![
            "-ss",
            string_to_static_str((self.start_number * CHUNK_SIZE).to_string()),
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
        }

        // args needed for strict keyframing so that video.js plays nicely
        args.append(&mut vec![
            "-x264-params",
            "keyint=120:min-keyint=120;no-scenecut=1",
        ]);

        // args needed to decrease the chances of a race condition when fetching a segment
        args.append(&mut vec!["-flush_packets", "1"]);

        args.append(&mut vec![
            "-f",
            "hls",
            "-start_number",
            string_to_static_str(self.start_number.to_string()),
        ]);

        /*
         * FIXME: For some reason this doubles our timestamp lol
        args.append(&mut vec![
            "-output_ts_offset",
            string_to_static_str((self.start_number * CHUNK_SIZE).to_string()),
        ]);
        */

        args.append(&mut vec![
            "-hls_time",
            string_to_static_str(CHUNK_SIZE.to_string()),
            "-initial_offset",
            string_to_static_str((self.start_number * CHUNK_SIZE).to_string()),
            "-reset_timestamps",
            "1",
            "-force_key_frames",
            "expr:gte(t,n_forced*5)",
        ]);

        args.append(&mut vec!["-hls_segment_type", "1"]);
        args.append(&mut vec!["-loglevel", "error", "-progress", "pipe:1"]);
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
            self.paused.store(true, SeqCst);
            println!("joining thread");
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
            StreamType::Video => self
                .get_key("frame")
                .map(|x| x.parse::<u64>().unwrap_or(0))
                .unwrap_or(0),
        };

        match self.stream_type {
            StreamType::Audio => (frame / (CHUNK_SIZE * 24)).max(self.last_chunk.load(SeqCst)),
            StreamType::Video => frame / (CHUNK_SIZE * 24) + self.start_number,
        }
    }

    // returns how many chunks per second
    pub fn speed(&self) -> f64 {
        let assumed = match self.stream_type {
            StreamType::Audio => 10.0,
            StreamType::Video => 2.0,
        };

        let fps = self
            .get_key("speed")
            .map(|x| x.trim_end_matches('x').to_string())
            .and_then(|x| x.parse::<f64>().map_err(|_| std::option::NoneError))
            .unwrap_or(assumed) // assume if key is missing that our speed is 2.0
            .floor()
            .max(20.0)
            * 24.0;

        fps / (CHUNK_SIZE as f64 * 24.0)
    }

    pub fn eta_for(&self, chunk: u64) -> Duration {
        if self.stream_type == StreamType::Audio {
            let lock = STREAMING_SESSION.read().unwrap();
            if let Some(x) = lock.get(&self.id) {}
        }
        let cps = self.speed();

        //        println!("CPS: {} RAW: {:?}", cps, self.get_key("speed"));

        let current_chunk = self.current_chunk() as f64;
        let diff = (chunk as f64 - current_chunk).abs();

        //        println!("cps: {} cur: {} diff: {}", cps, current_chunk, diff);

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
                self.current_chunk() > 5 && self.current_chunk() - 5 >= chunk_num
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

            if !events.is_empty() {
                // got events
            }

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
        Instant::now() > self.last_reset.load() + Duration::from_secs(30)
    }

    pub fn reset_timeout(&self, last_requested: u64) {
        self.last_reset.store(Instant::now());
    }

    pub fn chunk_to_path(&self, chunk_num: u64) -> String {
        format!("{}/{}.m4s", self.outdir, chunk_num)
    }

    pub fn init_seg(&self) -> String {
        format!("{}/init.mp4", self.outdir)
    }

    pub fn has_started(&self) -> bool {
        self.has_started
    }

    pub fn to_new(&self) -> Self {
        Self {
            id: self.id.clone(),
            file: self.file.clone(),
            profile: self.profile,
            outdir: self.outdir.clone(),
            process: AtomicCell::new(None),
            start_number: self.start_number,
            stream_type: self.stream_type,
            last_chunk: AtomicU64::new(self.last_chunk.load(SeqCst)),
            has_started: false,
            paused: AtomicBool::new(true),
            last_reset: AtomicCell::new(Instant::now()),

            #[cfg(feature = "fs_events")]
            fs_watcher: AtomicCell::new(self.fs_watcher.take()),
            #[cfg(feature = "fs_events")]
            fs_events: self.fs_events.clone(),
        }
    }

    pub fn to_new_with_chunk(&self, chunk: u64) -> Self {
        Self {
            id: self.id.clone(),
            file: self.file.clone(),
            profile: self.profile,
            outdir: self.outdir.clone(),
            process: AtomicCell::new(None),
            start_number: chunk,
            stream_type: self.stream_type,
            last_chunk: AtomicU64::new(self.last_chunk.load(SeqCst)),
            has_started: false,
            paused: AtomicBool::new(true),
            last_reset: AtomicCell::new(Instant::now()),

            #[cfg(feature = "fs_events")]
            fs_watcher: AtomicCell::new(self.fs_watcher.take()),
            #[cfg(feature = "fs_events")]
            fs_events: self.fs_events.clone(),
        }
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

        let mut stdio_b = stdio.lines();

        'stdout: while !signal.get() {
            let output = stdio_b.next().unwrap().unwrap();
            let output: Vec<&str> = output.split('=').collect();

            // remove whitespace on both ends
            map.insert(output[0].into(), output[1].trim_start().trim_end().into());

            {
                let mut lock = STREAMING_SESSION.write().unwrap();
                let _ = lock.insert(self.id.clone(), map.clone());
            }

            match self.process.try_wait() {
                Ok(Some(_)) => break 'stdout,
                Ok(None) => {}
                Err(x) => println!("handle_stdout got err on try_wait(): {:?}", x),
            }
        }

        let _ = self.process.kill();
        let _ = self.process.wait();

        let mut lock = STREAMING_SESSION.write().unwrap();
        let _ = lock.remove(&self.id);
    }
}

fn string_to_static_str(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}
