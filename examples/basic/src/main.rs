#![feature(proc_macro_hygiene, decl_macro, once_cell)]

#[macro_use]
extern crate rocket;

use chrono::{prelude::*, NaiveDateTime, Utc};
use nightfall::{ffprobe::FFProbeCtx, profile::*, StateManager};
use rocket::Data;
use rocket::State;
use rocket::{
    http::ContentType,
    http::Status,
    response::{NamedFile, Response},
};

use std::collections::HashMap;
use std::io::Cursor;
use std::io::Read;
use std::lazy::SyncOnceCell;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Duration;

use std::ffi::OsStr;

const DEMO_FILE: &str = "/home/hinach4n/media/media1/movies/John.Wick.Chapter.3.Parabellum.2019.1080p.AMZN.WEBRip.DD5.1.x264-FGT/John.Wick.Chapter.3.Parabellum.2019.1080p.AMZN.WEBRip.DD5.1.x264-FGT.mkv";
static VIDEO_UUID: SyncOnceCell<String> = SyncOnceCell::new();
static AUDIO_UUID: SyncOnceCell<String> = SyncOnceCell::new();
const FFMPEG_BIN: &str = "/usr/bin/ffmpeg";
const FFPROBE_BIN: &str = "/usr/bin/ffprobe";
const CACHE_DIR: &str = "/tmp/streaming_cache";

#[get("/manifest.mpd?<start_num>")]
fn get_manifest(
    state: State<StateManager>,
    start_num: Option<u64>,
) -> Result<Response<'static>, ()> {
    std::fs::File::open(DEMO_FILE).expect("demo file doesnt exist");

    let info = FFProbeCtx::new(FFPROBE_BIN)
        .get_meta(&std::path::PathBuf::from(DEMO_FILE))
        .unwrap();

    let start_num = start_num.unwrap_or(0);

    let mut ms = info.get_ms().unwrap().to_string();
    ms.truncate(4);

    let duration = chrono::DateTime::<Utc>::from_utc(
        NaiveDateTime::from_timestamp(info.get_duration().unwrap() as i64, 0),
        Utc,
    );

    let duration_string = format!(
        "PT{}H{}M{}.{}S",
        duration.hour(),
        duration.minute(),
        duration.second(),
        ms
    );

    let video =
        VIDEO_UUID.get_or_init(|| state.create(DEMO_FILE.into(), Profile::High, StreamType::Video));

    let audio = AUDIO_UUID
        .get_or_init(|| state.create(DEMO_FILE.into(), Profile::Audio, StreamType::Audio));

    let video_part = format!(
        include_str!("./video_segment.mpd"),
        bandwidth = info.get_bitrate(),
        init = format!("init/{}/{}_init.mp4", video.clone(), start_num),
        chunk_path = format!("chunks/{}/$Number$.m4s", video.clone()),
        start_num = start_num,
    );

    let audio_part = format!(
        include_str!("./audio_segment.mpd"),
        init = format!("init/{}/{}_init.mp4", audio.clone(), start_num),
        chunk_path = format!("chunks/{}/$Number$.m4s", audio.clone()),
        start_num = start_num,
    );

    let manifest = format!(
        include_str!("./manifest.mpd"),
        duration = duration_string,
        segments = format!("{}\n{}", video_part, audio_part),
    );

    /*
    let formatted = format!(
        include_str!("./manifest.mpd"),
        duration_string,
        duration_string,
        audio,
        audio,
        start_num.unwrap_or(0)
    );
    */

    Response::build()
        .header(ContentType::new("application", "dash+xml"))
        .sized_body(Cursor::new(manifest))
        .ok()
}

#[get("/id")]
fn get_id(state: State<StateManager>) -> String {
    VIDEO_UUID
        .get_or_init(|| state.create(DEMO_FILE.into(), Profile::High, StreamType::Video))
        .clone()
}

#[get("/is_chunk_ready/<id>/<chunk_num>")]
fn is_chunk_ready(state: State<StateManager>, id: String, chunk_num: u64) -> Status {
    if state.should_client_hard_seek(id, chunk_num).unwrap() {
        Status::NoContent
    } else {
        Status::Ok
    }
}

#[get("/init/<id>/<init..>")]
fn get_init(
    state: State<StateManager>,
    id: String,
    init: PathBuf,
) -> Result<Option<NamedFile>, ()> {
    if init.extension() != Some(OsStr::new("mp4")) {
        return Err(());
    }

    let path = state.init_or_create(id).unwrap();

    Ok(NamedFile::open(path).ok())
}

/// At the entry of the routine we go through the fast path and check if the segment queried for is
/// finished and whether we have polled a CLOSE event. If so we return the file.
///
/// First come first serve, subroutine will query the state for this segment while locking the
/// state till the completion of the chunk. if the head is beyond a certain threshold we kill the
/// previous ffmpeg session and start one at the offset of the chunk currently being queried for.
///
/// When the chunk is reported as finished, we await for a event of `CLOSE_WRITE` from inotify,
/// then return the file to avoid a race condition.
#[get("/chunks/<id>/<chunk..>")]
fn get_chunks(
    state: State<StateManager>,
    id: String,
    chunk: PathBuf,
) -> Result<Option<NamedFile>, ()> {
    let extension = chunk.extension().ok_or(())?.to_string_lossy().into_owned();

    // only accept requests for m4s files.
    if !["m4s"].contains(&extension.as_str()) {
        return Ok(None);
    }

    let chunk_num = chunk
        .file_stem()
        .ok_or(())?
        .to_string_lossy()
        .into_owned()
        .parse::<u64>()
        .unwrap_or(0);

    if let Err(_) = state.exists(id.clone()) {
        state
            .init_or_create(id.clone())
            .expect("failed to start stream");
    }

    // try to get the chunk or create one.
    let path = state.get_segment(id.clone(), chunk_num).unwrap();

    for _ in 0..5 {
        if let Ok(_) = NamedFile::open(path.clone()) {
            return Ok(NamedFile::open(path).ok());
        }

        std::thread::sleep(Duration::from_millis(100));
    }

    Ok(NamedFile::open(path).ok())
}

/// Manage stats
#[post("/session/stats/<id>", data = "<data>")]
fn set_stats(stats: State<nightfall::FfmpegSessionStats>, id: String, data: Data) -> Status {
    let mut data = data.open();

    loop {
        let mut string = String::new();
        data.read_to_string(&mut string).unwrap();
        println!("GOT DATA FOR {} {}", id, string);
        std::thread::sleep_ms(200);
    }
    return Status::Ok;

    /*
    let data: Vec<Vec<String>> = dbg!(data)
        .split(" ")
        .map(|x| {
            dbg!(x)
                .split("=")
                .map(|x| x.to_string())
                .collect::<Vec<String>>()
        })
        .collect();

    for pair in data {
        map.insert(pair[0].clone(), pair[1].clone());
    }

    println!("{:?}", map);

    stats.write().unwrap().insert(id, map);

    Status::Ok
    */
}

#[get("/")]
fn get_index() -> Option<NamedFile> {
    NamedFile::open(Path::new("static/index.html")).ok()
}

#[get("/js/<file..>")]
fn get_static_js(file: PathBuf) -> Option<NamedFile> {
    NamedFile::open(Path::new("static/js").join(file)).ok()
}

#[get("/css/<file..>")]
fn get_static_css(file: PathBuf) -> Option<NamedFile> {
    NamedFile::open(Path::new("static/css").join(file)).ok()
}

fn main() {
    let stats: nightfall::FfmpegSessionStats = Arc::new(RwLock::new(HashMap::new()));

    let cors: rocket_cors::CorsOptions = Default::default();
    let state_manager = StateManager::new(
        CACHE_DIR.into(),
        FFMPEG_BIN.into(),
        FFPROBE_BIN.into(),
        Arc::clone(&stats),
    );

    use rocket::config::Environment;
    use rocket::config::LoggingLevel;
    use rocket::Config;

    let config = Config::build(Environment::Development)
        .address("127.0.0.1")
        .port(8000)
        .workers(8)
        .log_level(LoggingLevel::Normal)
        .unwrap();

    rocket::custom(config)
        .manage(state_manager)
        .manage(stats)
        .mount(
            "/",
            routes![
                get_manifest,
                get_chunks,
                get_init,
                get_index,
                get_static_js,
                get_static_css,
                is_chunk_ready,
                get_id,
                set_stats
            ],
        )
        .attach(cors.to_cors().unwrap())
        .launch();
}
