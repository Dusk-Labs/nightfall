pub mod audio;
pub mod subtitle;
pub mod video;

pub use audio::AacTranscodeProfile;
pub use subtitle::WebvttTranscodeProfile;
pub use video::H264TranscodeProfile;
pub use video::H264TransmuxProfile;
pub use video::RawVideoTranscodeProfile;

use std::lazy::SyncOnceCell;

static PROFILES: SyncOnceCell<Vec<Box<dyn TranscodingProfile>>> = SyncOnceCell::new();

pub fn profiles_init(_ffmpeg_bin: String) {
    let profiles: Vec<Box<dyn TranscodingProfile>> = vec![
            box AacTranscodeProfile,
            box H264TranscodeProfile,
            box H264TransmuxProfile,
            box RawVideoTranscodeProfile,
            box WebvttTranscodeProfile
    ];

    let _ = PROFILES.set(profiles.into_iter().filter(|x| x.is_enabled()).collect());
}

pub fn get_profile_for(
    stream_type: StreamType,
    codec_in: &str,
    codec_out: &str,
) -> Vec<&'static dyn TranscodingProfile> {
    let mut profiles: Vec<_> = PROFILES
        .get()
        .expect("nightfall::PROFILES not initialized.")
        .iter()
        .filter(|x| x.stream_type() == stream_type && x.supports(codec_in, codec_out))
        .map(AsRef::as_ref)
        .collect();
    
    profiles.sort_by_key(|x| x.profile_type());

    profiles
}

pub trait TranscodingProfile: Send + Sync + 'static {
    /// Function must return what kind of profile it is.
    fn profile_type(&self) -> ProfileType;

    /// Function will return what type of stream this profile is for.
    fn stream_type(&self) -> StreamType;

    /// This function gets called at run-time to check whether this profile is enabled.
    /// By default this function is auto-implemented to return `true`, however for complex
    /// profiles such as VAAPI we may want at run-time to check whether ffmpeg will actually
    /// transcode the given file.
    fn is_enabled(&self) -> bool {
        true
    }

    /// Function will build a list of arguments to be passed to ffmpeg for the profile which
    /// implements this trait. The function will return `None` if the parameters supplied in the
    /// context are invalid or cant be used here.
    fn build(&self, ctx: ProfileContext) -> Option<Vec<String>>;

    /// Function will return whether the conversion to `codec_out` is possible. Some
    /// implementations of this function (HWAccelerated profiles) will also check whether
    /// a direct conversion betwen`codec_in` and `codec_out` is possible.
    fn supports(&self, _codec_in: &str, codec_out: &str) -> bool;

    fn tag(&self) -> &str;

    fn is_stdio_stream(&self) -> bool {
        false
    }
}

/// A context which contains information we may need when building the ffmpeg arguments.
#[derive(Clone, Debug)]
pub struct ProfileContext {
    pub stream: usize,
    pub pre_args: Vec<String>,
    pub start_num: u32,
    pub file: String,
    pub outdir: String,
    pub seek: Option<i64>,
    pub max_to_transcode: Option<u64>,

    pub bitrate: Option<u64>,
    pub height: Option<i64>,
    pub width: Option<i64>,
    pub audio_channels: u64,
    pub ffmpeg_bin: String,
}

impl Default for ProfileContext {
    fn default() -> Self {
        Self {
            stream: 0,
            pre_args: Vec::new(),
            start_num: 0,
            file: String::new(),
            outdir: String::new(),
            seek: None,
            max_to_transcode: None,
            bitrate: None,
            height: None,
            width: None,
            audio_channels: 2,
            ffmpeg_bin: "ffmpeg".into(),
        }
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Ord, PartialOrd)]
pub enum ProfileType {
    HardwareTranscode,
    Transmux,
    Transcode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamType {
    Video,
    Audio,
    Subtitle,
}
