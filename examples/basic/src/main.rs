#![feature(proc_macro_hygiene, decl_macro)]

#[macro_use]
extern crate rocket;

use chrono::{prelude::*, NaiveDateTime, Utc};
use nightfall::{ffprobe::FFProbeCtx, profile::*, StateManager};
use rocket::State;
use rocket::{
    http::ContentType,
    response::{NamedFile, Response},
};
use std::io::Cursor;
use std::path::PathBuf;
use std::time::Duration;

const DEMO_FILE: &str = "/home/hinach4n/media/media1/movies/John.Wick.Chapter.3.Parabellum.2019.1080p.AMZN.WEBRip.DD5.1.x264-FGT/John.Wick.Chapter.3.Parabellum.2019.1080p.AMZN.WEBRip.DD5.1.x264-FGT.mkv";

#[get("/manifest.mpd")]
fn get_manifest(state: State<StateManager>) -> Result<Response<'static>, ()> {
    let info = FFProbeCtx::new("/usr/bin/ffprobe")
        .get_meta(&std::path::PathBuf::from(DEMO_FILE))
        .map_err(|_| ())?;

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

    let video = state.create(DEMO_FILE.into(), Profile::High, StreamType::Video);
    let audio = state.create(DEMO_FILE.into(), Profile::Audio, StreamType::Audio);
    let formatted = format!(
        include_str!("./manifest.mpd"),
        duration_string,
        info.get_bitrate().as_str().parse::<u64>().unwrap_or(0),
        video,
        video,
        audio,
        audio,
    );

    Response::build()
        .header(ContentType::new("application", "dash+xml"))
        .sized_body(Cursor::new(formatted))
        .ok()
}

#[get("/chunks/<id>/init.mp4", rank = 1)]
fn get_init(state: State<StateManager>, id: String) -> Result<Option<NamedFile>, ()> {
    let path = state
        .init_or_create(id, Duration::from_millis(5000))
        .unwrap();

    Ok(NamedFile::open(path).ok())
}

#[get("/chunks/<id>/<chunk..>", rank = 2)]
fn get_chunks(
    state: State<StateManager>,
    id: String,
    chunk: PathBuf,
) -> Result<Option<NamedFile>, ()> {
    let extension = chunk.extension().ok_or(())?.to_string_lossy().into_owned();

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

    let path = state
        .get_or_create(id, chunk_num, Duration::from_millis(5000))
        .unwrap();

    Ok(NamedFile::open(path).ok())
}

fn main() {
    rocket::ignite()
        .manage(StateManager::new("/tmp/streaming_cache".into()))
        .mount("/", routes![get_manifest, get_chunks, get_init])
        .launch();
}
