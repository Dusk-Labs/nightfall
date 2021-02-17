#![feature(proc_macro_hygiene, decl_macro, once_cell)]

#[macro_use]
extern crate rocket;

use chrono::{prelude::*, NaiveDateTime, Utc};
use nightfall::{ffprobe::FFProbeCtx, profile::*, StateManager};
use rocket::State;
use rocket::{
    http::ContentType,
    http::Status,
    response::{NamedFile, Response},
};
use std::io::Cursor;
use std::lazy::SyncOnceCell;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

const DEMO_FILE: &str = "/home/hinach4n/media/media1/movies/John.Wick.Chapter.3.Parabellum.2019.1080p.AMZN.WEBRip.DD5.1.x264-FGT/John.Wick.Chapter.3.Parabellum.2019.1080p.AMZN.WEBRip.DD5.1.x264-FGT.mkv";
static VIDEO_UUID: SyncOnceCell<String> = SyncOnceCell::new();

#[get("/manifest.mpd?<start_num>")]
fn get_manifest(
    state: State<StateManager>,
    start_num: Option<u64>,
) -> Result<Response<'static>, ()> {
    let info =
        dbg!(FFProbeCtx::new("/usr/bin/ffprobe").get_meta(&std::path::PathBuf::from(DEMO_FILE)))
            .unwrap();

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
    let audio = state.create(DEMO_FILE.into(), Profile::Audio, StreamType::Audio);

    println!("codec: {:?}", info);
    let formatted = format!(
        include_str!("./manifest.mpd"),
        duration_string,
        duration_string,
        info.get_bitrate(),
        video,
        video,
        start_num.unwrap_or(0)
    );

    Response::build()
        .header(ContentType::new("application", "dash+xml"))
        .sized_body(Cursor::new(formatted))
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
    if state.eta_for_seg(id, chunk_num).unwrap() <= 5 {
        Status::Ok
    } else {
        Status::NoContent
    }
}

#[get("/chunks/<id>/init.mp4", rank = 1)]
fn get_init(state: State<StateManager>, id: String) -> Result<Option<NamedFile>, ()> {
    let path = state
        .init_or_create(id, Duration::from_millis(5000))
        .unwrap();

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
#[get("/chunks/<id>/<chunk..>", rank = 2)]
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

    if let Err(_) = state.exists(id.clone(), chunk_num) {
        state
            .init_or_create(id.clone(), Duration::from_millis(5000))
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
    let cors: rocket_cors::CorsOptions = Default::default();

    rocket::ignite()
        .manage(StateManager::new("/tmp/streaming_cache".into()))
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
            ],
        )
        .attach(cors.to_cors().unwrap())
        .launch();
}
