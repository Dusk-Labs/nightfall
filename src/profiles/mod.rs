#[cfg(windows)]
pub mod amf;
pub mod audio;
#[cfg(all(unix, feature = "cuda"))]
pub mod cuda;
pub mod subtitle;
#[cfg(all(unix, feature = "vaapi"))]
pub mod vaapi;
pub mod video;

#[cfg(windows)]
pub use amf::AmfTranscodeProfile;
pub use audio::AacTranscodeProfile;
#[cfg(all(unix, feature = "cuda"))]
pub use cuda::CudaTranscodeProfile;
#[cfg(feature = "ssa_transmux")]
pub use subtitle::AssExtractProfile;
pub use subtitle::WebvttTranscodeProfile;
use tracing::debug;
use tracing::info;
use tracing::warn;
#[cfg(all(unix, feature = "vaapi"))]
pub use vaapi::VaapiTranscodeProfile;
pub use video::H264TranscodeProfile;
pub use video::H264TransmuxProfile;
pub use video::RawVideoTranscodeProfile;

use crate::NightfallError;
use std::fmt::Debug;

use once_cell::sync::OnceCell;

static PROFILES: OnceCell<Vec<Box<dyn TranscodingProfile>>> = OnceCell::new();

pub fn profiles_init(_ffmpeg_bin: String) {
    let profiles: Vec<Option<Box<dyn TranscodingProfile>>> = vec![
        Some(Box::new(AacTranscodeProfile)),
        Some(Box::new(H264TranscodeProfile)),
        Some(Box::new(H264TransmuxProfile)),
        Some(Box::new(RawVideoTranscodeProfile)),
        Some(Box::new(WebvttTranscodeProfile)),
        #[cfg(feature = "ssa_transmux")]
        Some(Box::new(AssExtractProfile)),
        #[cfg(all(unix, feature = "cuda"))]
        Some(Box::new(CudaTranscodeProfile)),
        #[cfg(all(unix, feature = "vaapi"))]
        VaapiTranscodeProfile::new().map(|x| Box::new(x) as _),
        #[cfg(windows)]
        Some(Box::new(AmfTranscodeProfile)),
    ];

    let profiles = profiles.into_iter().filter_map(|x| x).collect::<Vec<_>>();

    let _ = PROFILES.set(
        profiles
            .into_iter()
            .filter(|x| {
                if let Err(e) = x.is_enabled() {
                    warn!(
                        profile = x.name(),
                        reason = %e,
                        "Disabling profile"
                    );

                    false
                } else {
                    info!(profile = x.name(), "Enabling profile");

                    true
                }
            })
            .collect(),
    );
}

pub fn get_active_profiles() -> Vec<&'static dyn TranscodingProfile> {
    PROFILES
        .get()
        .expect("nightfall::PROFILES not initialized.")
        .iter()
        .map(AsRef::as_ref)
        .collect()
}

pub fn get_profile_for(
    stream_type: StreamType,
    ctx: &ProfileContext,
) -> Vec<&'static dyn TranscodingProfile> {
    let mut profiles: Vec<_> = PROFILES
        .get()
        .expect("nightfall::PROFILES not initialized.")
        .iter()
        .filter(|x| {
            x.stream_type() == stream_type
                && if let Err(e) = x.supports(ctx) {
                    debug!(
                        profile = x.name(),
                        reason = %e,
                        "Profile not supported for ctx"
                    );

                    false
                } else {
                    true
                }
        })
        .map(AsRef::as_ref)
        .collect();

    profiles.sort_by_key(|x| x.profile_type());

    profiles
}

pub fn get_profile_for_with_type(
    stream_type: StreamType,
    profile_type: ProfileType,
    ctx: &ProfileContext,
) -> Vec<&'static dyn TranscodingProfile> {
    let mut profiles: Vec<_> = PROFILES
        .get()
        .expect("nightfall::PROFILES not initialized.")
        .iter()
        .filter(|x| {
            x.profile_type() == profile_type
                && x.stream_type() == stream_type
                && if let Err(e) = x.supports(ctx) {
                    debug!(
                        profile = x.name(),
                        reason = %e,
                        "Profile not supported for ctx"
                    );

                    false
                } else {
                    true
                }
        })
        .map(AsRef::as_ref)
        .collect();

    profiles.sort_by_key(|x| x.profile_type());

    profiles
}

pub trait TranscodingProfile: Debug + Send + Sync + 'static {
    /// Function must return what kind of profile it is.
    fn profile_type(&self) -> ProfileType;

    /// Function will return what type of stream this profile is for.
    fn stream_type(&self) -> StreamType;

    /// This function gets called at run-time to check whether this profile is enabled.
    /// By default this function is auto-implemented to return `true`, however for complex
    /// profiles such as VAAPI we may want at run-time to check whether ffmpeg will actually
    /// transcode the given file.
    fn is_enabled(&self) -> Result<(), NightfallError> {
        Ok(())
    }

    /// Function will build a list of arguments to be passed to ffmpeg for the profile which
    /// implements this trait. The function will return `None` if the parameters supplied in the
    /// context are invalid or cant be used here.
    fn build(&self, ctx: ProfileContext) -> Option<Vec<String>>;

    /// Function will return whether the conversion to `codec_out` is possible. Some
    /// implementations of this function (HWAccelerated profiles) will also check whether
    /// a direct conversion betwen`codec_in` and `codec_out` is possible.
    fn supports(&self, ctx: &ProfileContext) -> Result<(), NightfallError>;

    /// Return tag of this profile.
    fn tag(&self) -> &str;

    /// Return name of this profile.
    fn name(&self) -> &str;

    /// Function will return whether this profile emit data over stdout instead of progress information.
    fn is_stdio_stream(&self) -> bool {
        false
    }
}

/// A context which contains information we may need when building the ffmpeg arguments.
#[derive(Clone, Debug)]
pub struct ProfileContext {
    pub file: String,
    pub pre_args: Vec<String>,
    pub input_ctx: InputCtx,
    pub output_ctx: OutputCtx,
    pub ffmpeg_bin: String,
}

#[derive(Clone, Debug)]
pub struct InputCtx {
    pub stream: usize,
    pub audio_channels: u64,
    pub codec: String,
    pub pix_fmt: String,
    pub profile: String,
    pub bframes: Option<u64>,
    pub fps: f64,
    pub bitrate: u64,
    pub seek: Option<i64>,
}

impl Default for InputCtx {
    fn default() -> Self {
        Self {
            stream: 0,
            codec: String::new(),
            audio_channels: 2,
            pix_fmt: String::new(),
            profile: String::new(),
            bframes: None,
            fps: 0.0,
            bitrate: 0,
            seek: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct OutputCtx {
    pub codec: String,
    pub start_num: u32,
    pub outdir: String,
    pub max_to_transcode: Option<u64>,
    pub bitrate: Option<u64>,
    pub height: Option<i64>,
    pub width: Option<i64>,
    pub audio_channels: u64,
    pub target_gop: u32,
}

impl Default for OutputCtx {
    fn default() -> Self {
        Self {
            codec: String::new(),
            start_num: 0,
            outdir: String::new(),
            max_to_transcode: None,
            bitrate: None,
            height: None,
            width: None,
            audio_channels: 2,
            target_gop: 5,
        }
    }
}

impl Default for ProfileContext {
    fn default() -> Self {
        Self {
            file: String::new(),
            pre_args: Vec::new(),
            input_ctx: Default::default(),
            output_ctx: Default::default(),
            ffmpeg_bin: "ffmpeg".into(),
        }
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Ord, PartialOrd)]
pub enum ProfileType {
    Transcode,
    Transmux,
    HardwareTranscode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamType {
    Video,
    Audio,
    Subtitle,
}
