use crate::profile::Profile;
use crate::profile::StreamType;

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::io::Read;

use std::io::BufRead;
use std::io::BufReader;
use std::path::Path;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;
use std::time::Instant;

use std::cell::RefCell;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;

use crossbeam::atomic::AtomicCell;
use stoppable_thread::{self, SimpleAtomicBool, StoppableHandle};

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
    hard_timeout: AtomicCell<Instant>,

    child_pid: AtomicCell<Option<u32>>,
    real_process: Arc<Mutex<RefCell<Option<Child>>>>,
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
        std::fs::create_dir_all(&outdir).unwrap();

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
            real_process: Arc::new(Mutex::new(RefCell::new(None))),
            hard_timeout: AtomicCell::new(Instant::now() + Duration::from_secs(30 * 60)),
            file,
        }
    }

    pub fn start(&self) -> Result<(), io::Error> {
        // make sure we actually have a path to write files to.
        self.has_started.store(true, SeqCst);
        self.paused.store(false, SeqCst);
        let _ = fs::create_dir_all(self.outdir.clone());
        let args = dbg!(self.build_args());

        // FIXME(Windows): For some reason if we dont tell rust to
        // use real stderr for ffmpeg instead of creating a new pipe
        // ffmpeg starts but it wont execute anything, it just idles.
        cfg_if::cfg_if! {
            if #[cfg(unix)] {
                let stderr = Stdio::piped();
            } else {
                let stderr = Stdio::inherit();
            }
        }

        let mut process = Command::new(self.ffmpeg_bin.clone())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .args(args.as_slice())
            .spawn()?;

        self.child_pid.store(Some(process.id()));

        let stdout = process.stdout.take().unwrap();
        let stdout_parser_thread = TranscodeHandler::new(self.id.clone(), stdout, process.id());

        let lock = self.real_process.lock().unwrap();
        let mut proc_ref = lock.borrow_mut();
        *proc_ref = Some(process);

        self.process
            .store(Some(stoppable_thread::spawn(move |signal| {
                stdout_parser_thread.handle(signal)
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
            "-y",
            "-ss",
            string_to_static_str((self.start_num() * CHUNK_SIZE).to_string()),
            "-i",
            self.file.as_str(),
        ];

        match self.stream_type {
            StreamType::Audio(stream) => {
                args.append(&mut vec![
                    "-copyts",
                    "-map",
                    string_to_static_str(format!("0:{}", stream)),
                    "-c:0",
                    "aac",
                    "-ac",
                    "2",
                    "-ab",
                    "0",
                    "-threads",
                    "1",
                ]);
            }
            StreamType::Video(stream) => {
                args.append(&mut vec![
                    "-copyts",
                    "-map",
                    string_to_static_str(format!("0:{}", stream)),
                ]);
                args.append(&mut self.profile.to_params().0);
            }
        }

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
            "-force_key_frames",
            "expr:gte(t,n_forced*5.00)",
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
        if let Some(x) = self.real_process.lock().unwrap().borrow_mut().as_mut() {
            x.kill();
            x.wait();
        }
    }

    pub fn stderr(&self) -> Option<String> {
        if let Some(x) = self.real_process.lock().unwrap().borrow_mut().as_mut() {
            let mut buf = String::new();
            x.stderr.as_mut()?.read_to_string(&mut buf);
            if buf.len() <= 250 {
                return Some(buf);
            }
            return Some(buf.split_off(buf.len() - 250));
        }

        None
    }

    pub fn try_wait(&self) -> bool {
        if let Some(x) = self.real_process.lock().unwrap().borrow_mut().as_mut() {
            if let Ok(Some(_)) = x.try_wait() {
                x.wait();
                return true;
            }
        }

        return false;
    }

    pub fn is_hard_timeout(&self) -> bool {
        Instant::now() > self.hard_timeout.load()
    }

    pub fn set_timeout(&self) {
        self.hard_timeout.store(Instant::now());
    }

    pub fn delete_tmp(&self) {
        fs::remove_dir_all(self.outdir.clone());
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
            StreamType::Audio(_) => {
                self.get_key("out_time_us")
                    .map(|x| x.parse::<u64>().unwrap_or(0))
                    .unwrap_or(0)
                    / 1000
                    / 1000
                    * 24
            }
            StreamType::Video(_) => self
                .get_key("frame")
                .map(|x| x.parse::<u64>().unwrap_or(0))
                .unwrap_or(0),
        };

        match self.stream_type {
            StreamType::Audio(_) => (frame / (CHUNK_SIZE * 24)).max(self.last_chunk.load(SeqCst)),
            StreamType::Video(_) => frame / (CHUNK_SIZE * 24) + self.start_num(),
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
    /// only works when `ffmpeg` writes files to tmp then renames them.
    pub fn is_chunk_done(&self, chunk_num: u64) -> bool {
        Path::new(&format!("{}/{}.m4s", &self.outdir, chunk_num)).is_file()
    }

    pub fn is_timeout(&self) -> bool {
        self.current_chunk() > self.last_chunk.load(SeqCst) + MAX_CHUNKS_AHEAD
    }

    pub fn reset_timeout(&self, last_requested: u64) {
        self.last_chunk.store(last_requested, SeqCst);
        self.hard_timeout
            .store(Instant::now() + Duration::from_secs(30 * 60));
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

use std::process::ChildStdout;

struct TranscodeHandler {
    id: String,
    process_stdout: ChildStdout,
    pid: u32,
}

impl TranscodeHandler {
    fn new(id: String, process_stdout: ChildStdout, pid: u32) -> Self {
        Self {
            id,
            process_stdout,
            pid,
        }
    }

    fn handle(self, signal: &SimpleAtomicBool) {
        let stdio = BufReader::new(self.process_stdout);
        let mut map: HashMap<String, String> = HashMap::new();

        let mut stdio_b = stdio.lines().peekable();

        'stdout: while !signal.get() {
            while stdio_b.peek().is_some() {
                let output = stdio_b.next().unwrap().unwrap();
                let output: Vec<&str> = output.split('=').collect();

                // remove whitespace on both ends
                map.insert(output[0].into(), output[1].trim_start().trim_end().into());

                {
                    let mut lock = STREAMING_SESSION.write().unwrap();
                    let _ = lock.insert(self.id.clone(), map.clone());
                }
            }

            if crate::utils::is_process_effectively_dead(self.pid) {
                break 'stdout;
            }

            std::thread::sleep(Duration::from_millis(100));
        }

        println!("Broken out of stdout loop, killing ffmpeg");

        let mut lock = STREAMING_SESSION.write().unwrap();
        let _ = lock.remove(&self.id);
    }
}

fn string_to_static_str(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}
