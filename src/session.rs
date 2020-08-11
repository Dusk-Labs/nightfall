use crate::profile::Profile;
use crate::profile::StreamType;
use std::{
    cell::Cell,
    collections::HashMap,
    fs,
    io::{self, BufReader, Read},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering::SeqCst},
        Arc, RwLock,
    },
};
use stoppable_thread::{self, SimpleAtomicBool, StoppableHandle};

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
    process: Cell<Option<StoppableHandle<()>>>,
    has_started: bool,
    start_number: u64,
    stream_type: StreamType,
    last_chunk: AtomicU64,
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
        Self {
            id,
            outdir,
            start_number,
            profile,
            stream_type,
            last_chunk: AtomicU64::new(0),
            process: Cell::new(None),
            has_started: false,
            file: format!("file://{}", file),
        }
    }

    pub fn start(&mut self) -> Result<(), io::Error> {
        // make sure we actually have a path to write files to.
        self.has_started = true;
        let _ = fs::create_dir_all(self.outdir.clone());
        let args = self.build_args();
        let mut process = Command::new("/usr/bin/ffmpeg");

        process
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .args(args.as_slice());

        let mut process = TranscodeHandler::new(self.id.clone(), process.spawn()?);
        self.process = Cell::new(Some(stoppable_thread::spawn(move |signal| {
            process.handle(signal)
        })));
        Ok(())
    }

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
                    "-copyts", "-map", "0:1", "-c:0", "aac", "-ac", "2", "-ab", "0",
                ]);
            }
            StreamType::Video => {
                args.append(&mut vec!["-copyts", "-map", "0:0"]);
                args.append(&mut self.profile.to_params().0);
            }
        }

        args.append(&mut vec![
            "-f",
            "hls",
            "-start_number",
            string_to_static_str(self.start_number.to_string()),
        ]);

        args.append(&mut vec![
            "-hls_time",
            string_to_static_str(CHUNK_SIZE.to_string()),
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
        }
    }

    pub fn current_chunk(&self) -> u64 {
        let frame = |k: &str| -> Result<u64, std::option::NoneError> {
            {
                let session = STREAMING_SESSION.read().unwrap();
                Ok(session.get(&self.id)?.get(k)?.parse::<u64>().unwrap_or(0))
            }
        };

        match self.stream_type {
            StreamType::Audio => {
                (frame("out_time_ms").unwrap_or(0) / (CHUNK_SIZE * 1000)
                    + (self.start_number * (CHUNK_SIZE * 1000)))
                    / 5000
            }
            StreamType::Video => {
                frame("frame").unwrap_or(0) / (CHUNK_SIZE * 24) + self.start_number
            }
        }
    }

    /// Method does some math magic to guess if a chunk has been fully written by ffmpeg yet
    pub fn is_chunk_done(&self, chunk_num: u64) -> bool {
        self.current_chunk() > chunk_num + 5
    }

    pub fn is_timeout(&self) -> bool {
        self.current_chunk() >= self.last_chunk.load(SeqCst) + MAX_CHUNKS_AHEAD
    }

    pub fn reset_timeout(&self, last_requested: u64) {
        // NOTE: experiment between setting last_chunk to current chunk or taking in the last chunk
        // requested
        if self.current_chunk() < last_requested + MAX_CHUNKS_AHEAD {
            self.last_chunk.store(self.current_chunk(), SeqCst)
        }
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
            process: Cell::new(None),
            start_number: self.start_number,
            stream_type: self.stream_type,
            last_chunk: AtomicU64::new(self.last_chunk.load(SeqCst)),
            has_started: false,
        }
    }

    pub fn to_new_with_chunk(&self, chunk: u64) -> Self {
        Self {
            id: self.id.clone(),
            file: self.file.clone(),
            profile: self.profile,
            outdir: self.outdir.clone(),
            process: Cell::new(None),
            start_number: chunk,
            stream_type: self.stream_type,
            last_chunk: AtomicU64::new(self.last_chunk.load(SeqCst)),
            has_started: false,
        }
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
        let mut stdio = BufReader::new(self.process.stdout.take().unwrap());
        let mut map: HashMap<String, String> = {
            let mut map = HashMap::new();
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
            map
        };
        let mut out: [u8; 256] = [0; 256];

        'stdout: while !signal.get() {
            let _ = stdio.read_exact(&mut out);
            let output = String::from_utf8_lossy(&out);
            let mut pairs = output
                .lines()
                .map(|x| x.split('=').filter(|x| x.len() > 1).collect::<Vec<&str>>())
                .filter(|x| x.len() == 2)
                .collect::<Vec<Vec<&str>>>();

            pairs.dedup_by(|a, b| a[0].eq(b[0]));

            for pair in pairs {
                if let Some(v) = map.get_mut(&pair[0].to_string()) {
                    *v = pair[1].into();
                }
            }

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

// FIXME: While i can make the promise that the Cell fields will never be accessed, i should
// probably (and will) switch to RefCell
unsafe impl Sync for Session {}
