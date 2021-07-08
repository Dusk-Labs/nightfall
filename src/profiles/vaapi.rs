use super::ProfileContext;
use super::ProfileType;
use super::StreamType;
use super::TranscodingProfile;

use crate::session::CHUNK_SIZE;
use crate::NightfallError;

/// Vaapi transcoding profiles.
/// This is a unix exclusive transcoding profile that leverages vaapi. This profile will
/// automatically be enabled if any of your GPUs support encoding and decoding h264 with the
/// profiles `Main`, `High` and `ConstrainedBaseline`. This profile will only transcode h264 input
/// streams.
#[cfg(unix)]
pub struct VaapiTranscodeProfile {
    profiles: Vec<rusty_vainfo::Profile>,
    vendor: String,
}

impl Default for VaapiTranscodeProfile {
    fn default() -> Self {
        let (vendor, profiles) = if let Ok(x) = rusty_vainfo::VaInstance::new() {
            (x.vendor_string(), x.profiles().unwrap_or_default())
        } else {
            ("<null_device>".to_string(), Default::default())
        };

        Self { profiles, vendor }
    }
}

#[cfg(unix)]
impl TranscodingProfile for VaapiTranscodeProfile {
    fn profile_type(&self) -> ProfileType {
        ProfileType::HardwareTranscode
    }

    fn stream_type(&self) -> StreamType {
        StreamType::Video
    }

    fn name(&self) -> &str {
        "VaapiTranscodeProfile"
    }

    fn is_enabled(&self) -> Result<(), NightfallError> {
        // Currently this profile only supports HW Encoding + decoding.
        let required_features = ["VAEntrypointEncSlice", "VAEntrypointVLD"];

        // NOTE: These could technically be less restrictive and we could match for them inside
        // build. Although I doubt that there are actually any devices that dont support all three
        // of these profiles.
        // see: https://github.com/intel/libva/blob/6e86b4fb4dafa123b1e31821f61da88f10cfbe91/va/va.h#L493
        let required_profiles = [
            "VAProfileH264ConstrainedBaseline",
            "VAProfileH264Main",
            "VAProfileH264High",
        ];

        // FIXME: We want to enable this profile if any of the above profiles are enabled for those
        // required features.
        for profile in required_profiles {
            let device_profile = self.profiles.iter().find(|x| x.name == profile).ok_or(
                NightfallError::ProfileNotSupported(format!(
                    "Device {} doesnt support profile {} (Supported profiles: {})",
                    self.vendor,
                    profile,
                    self.profiles
                        .iter()
                        .map(|x| x.name.clone())
                        .collect::<Vec<_>>()
                        .join(" | ")
                )),
            )?;

            for feature in required_features {
                if !device_profile.entrypoints.contains(&feature.to_string()) {
                    continue;
                }
            }

            // we really only care if one of the profiles supports both enc+dec as this step only
            // checks whether the device supports hw decoding in general.
            return Ok(());
        }

        Err(NightfallError::ProfileNotSupported(format!(
            "Device {} doesnt seem to support hardware transcoding.",
            self.vendor
        )))
    }

    fn build(&self, ctx: ProfileContext) -> Option<Vec<String>> {
        let start_num = ctx.output_ctx.start_num.to_string();
        let stream = format!("0:{}", ctx.input_ctx.stream);
        let init_seg = format!("{}_init.mp4", &start_num);
        let seg_name = format!("{}/%d.m4s", ctx.output_ctx.outdir);
        let outdir = format!("{}/playlist.m3u8", ctx.output_ctx.outdir);

        let mut args = vec![
            "-hwaccel".into(),
            "vaapi".into(),
            "-hwaccel_output_format".into(),
            "vaapi".into(),
            "-y".into(),
            "-ss".into(),
            (ctx.output_ctx.start_num * CHUNK_SIZE).to_string(),
            "-i".into(),
            ctx.file,
            "-copyts".into(),
            "-map".into(),
            stream,
            "-c:0".into(),
            "h264_vaapi".into(),
        ];

        args.append(&mut vec![
            "-start_at_zero".into(),
            "-vsync".into(),
            "passthrough".into(),
            "-avoid_negative_ts".into(),
            "disabled".into(),
            "-max_muxing_queue_size".into(),
            "2048".into(),
            "-keyint_min".into(),
            "120".into(),
            "-g".into(),
            "120".into(),
            "-vf".into(),
            "hwdownload,format=nv12,hwupload".into(),
            "-frag_duration".into(),
            "5000000".into(),
            "-movflags".into(),
            "frag_keyframe".into(),
            "-use_mfra_for".into(),
            "pts".into(),
        ]);

        args.append(&mut vec![
            "-f".into(),
            "hls".into(),
            "-start_number".into(),
            start_num,
        ]);

        // needed so that in progress segments are named `tmp` and then renamed after the data is
        // on disk.
        // This in theory practically prevents the web server from returning a segment that is
        // in progress.
        args.append(&mut vec![
            "-hls_flags".into(),
            "independent_segments".into(),
            "-hls_flags".into(),
            "temp_file".into(),
            "-max_delay".into(),
            "5000000".into(),
        ]);

        // args needed so we can distinguish between init fragments for new streams.
        // Basically on the web seeking works by reloading the entire video because of
        // discontinuity issues that browsers seem to not ignore like mpv.
        args.append(&mut vec!["-hls_fmp4_init_filename".into(), init_seg]);

        args.append(&mut vec!["-hls_time".into(), "5".into()]);

        args.append(&mut vec![
            "-force_key_frames".into(),
            "expr:if(isnan(prev_forced_t),eq(t,t),gte(t,prev_forced_t+5.00))".into(),
        ]);

        args.append(&mut vec!["-hls_segment_type".into(), 1.to_string()]);
        args.append(&mut vec![
            "-loglevel".into(),
            "info".into(),
            "-progress".into(),
            "pipe:1".into(),
        ]);
        args.append(&mut vec!["-hls_segment_filename".into(), seg_name]);
        args.push(outdir);

        Some(args)
    }

    /// This profile technically could work on any codec since the codec is just `copy` here, but
    /// the container doesnt support it, so we will be constricting it down.
    fn supports(&self, ctx: &ProfileContext) -> Result<(), NightfallError> {
        if ctx.input_ctx.codec != ctx.output_ctx.codec || ctx.input_ctx.codec != "h264" {
            return Err(NightfallError::ProfileNotSupported(
                "Profile only supports h264 input and output streams.".into(),
            ));
        }

        let profile = match ctx.input_ctx.profile.as_str() {
            "High" => "VAProfileH264High",
            "Main" => "VAProfileH264Main",
            "Baseline" => "VAProfileH264Baseline",
            _ => {
                return Err(NightfallError::ProfileNotSupported(
                    "Profile not supported by device.".into(),
                ))
            }
        };

        if self.profiles.iter().find(|x| x.name == profile).is_none() {
            return Err(NightfallError::ProfileNotSupported(format!(
                "Profile {} not supported by device.",
                profile
            )));
        }

        Ok(())
    }

    fn tag(&self) -> &str {
        "h264_vaapi"
    }
}
