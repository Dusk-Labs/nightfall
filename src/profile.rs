use std::fmt;

pub trait Profile {
    fn to_args(&self, start_num: u32, outdir: &str) -> Vec<String>;
}

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum StreamType {
    Video {
        map: usize,
        profile: VideoProfile,
    },
    Audio {
        map: usize,
        profile: AudioProfile,
    },
    Subtitle {
        map: usize,
        profile: SubtitleProfile,
    },
    RawVideo {
        map: usize,
        profile: RawVideoProfile,
        tt: Option<usize>,
    },
}

impl StreamType {
    pub fn map(&self) -> usize {
        match self {
            Self::Video { map, .. } => *map,
            Self::Audio { map, .. } => *map,
            Self::Subtitle { map, .. } => *map,
            Self::RawVideo { map, .. } => *map,
        }
    }
}

impl fmt::Display for StreamType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "map {} -> {}",
            self.map().to_string(),
            match self {
                Self::Video { profile, .. } => profile.to_string(),
                Self::Audio { profile, .. } => profile.to_string(),
                Self::Subtitle { profile, .. } => profile.to_string(),
                Self::RawVideo { profile, .. } => profile.to_string(),
            }
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoProfile {
    /// Only transmuxes the stream, keeps the same resolution and bitrate
    Direct,
    /// Transcodes the stream but keeps the native resolution and bitrate
    Native,
    High,
    Medium,
    Low,
}

impl VideoProfile {
    #[allow(clippy::wrong_self_convention)]
    pub fn to_params(&self) -> (Vec<&str>, &str) {
        match self {
            Self::Direct => (vec!["-c:0", "copy"], "direct"),
            Self::Native => (vec!["-c:0", "libx264", "-preset", "veryfast"], "max"),
            Self::High => (
                vec![
                    "-c:0",
                    "libx264",
                    "-b:v",
                    "5M",
                    "-preset",
                    "veryfast",
                    "-vf",
                    "scale=1280:-2",
                ],
                "5000kb",
            ),
            Self::Medium => (
                vec![
                    "-c:0",
                    "libx264",
                    "-b:v",
                    "2M",
                    "-preset",
                    "ultrafast",
                    "-vf",
                    "scale=720:-2",
                ],
                "2000kb",
            ),
            Self::Low => (
                vec![
                    "-c:0",
                    "libx264",
                    "-b:v",
                    "1M",
                    "-preset",
                    "ultrafast",
                    "-vf",
                    "scale=480:-2",
                ],
                "1000kb",
            ),
        }
    }

    pub fn from_string<T: AsRef<str>>(profile: T) -> Option<Self> {
        Some(match profile.as_ref() {
            "direct" => Self::Direct,
            "native" => Self::Native,
            "5000kb" => Self::High,
            "2000kb" => Self::Medium,
            "1000kb" => Self::Low,
            _ => return None,
        })
    }
}

impl fmt::Display for VideoProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Direct => "copy",
                Self::Native => "h264@native",
                Self::High => "h264@5000kb",
                Self::Medium => "h264@2000kb",
                Self::Low => "h264@1000kb",
            }
        )
    }
}

impl Profile for VideoProfile {
    fn to_args(&self, start_num: u32, outdir: &str) -> Vec<String> {
        let start_num = start_num.to_string();
        let init_seg = format!("{}_init.mp4", &start_num);
        let seg_name = format!("{}/%d.m4s", outdir);
        let outdir = format!("{}/playlist.m3u8", outdir);

        let mut args = self.to_params().0;

        args.append(&mut vec![
            "-start_at_zero",
            "-vsync",
            "passthrough",
            "-avoid_negative_ts",
            "disabled",
            "-max_muxing_queue_size",
            "2048",
        ]);

        args.append(&mut vec!["-f", "hls", "-start_number", &start_num]);

        // needed so that in progress segments are named `tmp` and then renamed after the data is
        // on disk.
        // This in theory practically prevents the web server from returning a segment that is
        // in progress.
        args.append(&mut vec![
            "-hls_flags",
            "temp_file",
            "-max_delay",
            "5000000",
        ]);

        // args needed so we can distinguish between init fragments for new streams.
        // Basically on the web seeking works by reloading the entire video because of
        // discontinuity issues that browsers seem to not ignore like mpv.
        args.append(&mut vec!["-hls_fmp4_init_filename", &init_seg]);

        args.append(&mut vec!["-hls_time", "5"]);

        // NOTE: I dont think we need to force key frames if we are not transcoding
        match self {
            Self::Direct => {}
            _ => args.append(&mut vec![
                "-force_key_frames",
                "expr:if(isnan(prev_forced_t),eq(t,t),gte(t,prev_forced_t+5.00))",
            ]),
        }

        args.append(&mut vec!["-hls_segment_type", "1"]);
        args.append(&mut vec!["-loglevel", "info", "-progress", "pipe:1"]);
        args.append(&mut vec!["-hls_segment_filename", &seg_name]);
        args.append(&mut vec![&outdir]);

        args.into_iter().map(ToString::to_string).collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioProfile {
    Low,
}

impl AudioProfile {
    #[allow(clippy::wrong_self_convention)]
    pub fn to_params(&self) -> (Vec<&str>, &str) {
        match self {
            Self::Low => (vec!["-c:0", "aac", "-ac", "2", "-ab", "128k"], "128kb"),
        }
    }

    pub fn from_string<T: AsRef<str>>(profile: T) -> Option<Self> {
        Some(match profile.as_ref() {
            "120kb" => Self::Low,
            _ => return None,
        })
    }
}

impl fmt::Display for AudioProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "aac@128kb")
    }
}

impl Profile for AudioProfile {
    fn to_args(&self, start_num: u32, outdir: &str) -> Vec<String> {
        let start_num = start_num.to_string();
        let init_seg = format!("{}_init.mp4", &start_num);
        let seg_name = format!("{}/%d.m4s", outdir);
        let outdir = format!("{}/playlist.m3u8", outdir);

        let mut args = self.to_params().0;

        args.append(&mut vec![
            "-start_at_zero",
            "-vsync",
            "-1",
            "-avoid_negative_ts",
            "disabled",
            "-max_muxing_queue_size",
            "2048",
        ]);

        args.append(&mut vec!["-f", "hls", "-start_number", &start_num]);

        // needed so that in progress segments are named `tmp` and then renamed after the data is
        // on disk.
        // This in theory practically prevents the web server from returning a segment that is
        // in progress.
        args.append(&mut vec![
            "-hls_flags",
            "temp_file",
            "-max_delay",
            "5000000",
        ]);

        // args needed so we can distinguish between init fragments for new streams.
        // Basically on the web seeking works by reloading the entire video because of
        // discontinuity issues that browsers seem to not ignore like mpv.
        args.append(&mut vec!["-hls_fmp4_init_filename", &init_seg]);

        args.append(&mut vec![
            "-hls_time",
            "5",
            "-force_key_frames",
            "expr:if(isnan(prev_forced_t),eq(t,t),gte(t,prev_forced_t+5.00))",
        ]);

        args.append(&mut vec!["-hls_segment_type", "1"]);
        args.append(&mut vec!["-loglevel", "info", "-progress", "pipe:1"]);
        args.append(&mut vec!["-hls_segment_filename", &seg_name]);
        args.append(&mut vec![&outdir]);

        args.into_iter().map(ToString::to_string).collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubtitleProfile {
    Webvtt,
}

impl fmt::Display for SubtitleProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "vtt")
    }
}

impl Profile for SubtitleProfile {
    fn to_args(&self, _: u32, _: &str) -> Vec<String> {
        let mut args = vec![];

        // we want to stream subtitles, thus we pipe its output to stdout and then we flush it to disk manually
        args.append(&mut vec!["-f", "webvtt", "-"]);

        args.into_iter().map(ToString::to_string).collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RawVideoProfile {
    RawRgb,
}

impl fmt::Display for RawVideoProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RawRgb")
    }
}

impl Profile for RawVideoProfile {
    fn to_args(&self, _: u32, _: &str) -> Vec<String> {
        let mut args = vec![];

        // FIXME: stop hardcoding max extraction time
        args.append(&mut vec!["-c:v", "rawvideo"]);
        args.append(&mut vec!["-vf", "scale=8:8"]);
        args.append(&mut vec!["-pix_fmt", "rgb24", "-preset", "ultrafast"]);
        args.append(&mut vec!["-f", "data", "-"]); // pipe data back

        args.into_iter().map(ToString::to_string).collect()
    }
}
