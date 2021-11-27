use super::ProfileContext;
use super::ProfileType;
use super::StreamType;
use super::TranscodingProfile;

use crate::NightfallError;

use std::fs;
use std::path::PathBuf;

/// Vaapi transcoding profiles.
/// This is a unix exclusive transcoding profile that leverages vaapi. This profile will
/// automatically be enabled if any of your GPUs support encoding and decoding h264 with the
/// profiles `Main`, `High` and `ConstrainedBaseline`. This profile will only transcode h264 input
/// streams.
#[cfg(unix)]
#[derive(Debug)]
pub struct VaapiTranscodeProfile {
    profiles: Vec<rusty_vainfo::Profile>,
    vendor: String,
    dri: PathBuf,
}

impl VaapiTranscodeProfile {
    pub fn new() -> Option<Self> {
        let hw_targets = fs::read_dir("/dev/dri")
            .ok()?
            .filter_map(Result::ok)
            .filter(|x| x.file_name().to_string_lossy().find("render").is_some())
            .map(|x| x.path())
            .collect::<Vec<_>>();

        for target in hw_targets {
            if let Ok(x) = rusty_vainfo::VaInstance::with_drm(&target) {
                return Some(Self {
                    profiles: x.profiles().unwrap_or_default(),
                    vendor: x.vendor_string(),
                    dri: target,
                });
            }
        }

        Some(Self {
            profiles: Vec::new(),
            vendor: "<null_device>".into(),
            dri: PathBuf::new(),
        })
    }

    fn hw_scaling_supported(&self) -> bool {
        let required_profiles = ["VAProfileH264Main", "VAProfileH264High"];

        let enc_slice = "VAEntrypointEncSlice".to_string();

        for profile in required_profiles {
            let device_profile = if let Some(x) = self.profiles.iter().find(|x| x.name == profile) {
                x
            } else {
                continue;
            };

            // NOTE: We should probably warn the client here that scaling wont work because they
            // possibly have the free intel quicksync driver installed (if dri is a intel igpu).
            return device_profile.entrypoints.contains(&enc_slice);
        }

        false
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
        let required_features = [
            "VAEntrypointEncSlice".to_string(),
            "VAEntrypointVLD".to_string(),
        ];

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
                if !&device_profile.entrypoints.contains(&feature) {
                    continue;
                }
            }

            // NOTE: We should probably warn the client here that scaling wont work because they
            // possibly have the free intel quicksync driver installed (if dri is a intel igpu).
            /*
            if !device_profile.entrypoints.contains("VAEntrypointEncSlice") &&
                device_profile.entrypoints.contains("VAEntrypointEncSliceLP") {
                    return Ok(());
            }
            */

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
            "-vaapi_device".into(),
            self.dri.to_string_lossy().into(),
            "-hwaccel_output_format".into(),
            "vaapi".into(),
            "-y".into(),
            "-ss".into(),
            (ctx.output_ctx.start_num * ctx.output_ctx.target_gop).to_string(),
            "-i".into(),
            ctx.file.clone(),
            "-copyts".into(),
            "-map".into(),
            stream,
            "-c:0".into(),
            "h264_vaapi".into(),
            "-bf".into(),
            "0".into(),
        ];

        args.push("-vf".into());

        if let Some(height) = ctx.output_ctx.height {
            let mut vfilter = Vec::new();
            let width = ctx.output_ctx.width.unwrap_or(-2); // defaults to scaling by 2

            if self.hw_scaling_supported() {
                vfilter.push(format!("scale_vaapi={}:{}", height, width));
            }

            vfilter.push("hwdownload".into());

            // TODO: Detect if input file is 10-bit with a less hacky way.
            if ctx.input_ctx.profile.as_str() == "Main 10" {
                vfilter.push("format=p010le".into());
            }

            vfilter.push("format=nv12".into());

            if !self.hw_scaling_supported() {
                vfilter.push(format!("scale={}:{}", height, width));
            }

            vfilter.push("hwupload".into());

            args.push(vfilter.join(","));
        } else {
            args.push("hwdownload,format=nv12,hwupload".into());
        }

        if let Some(bitrate) = ctx.output_ctx.bitrate {
            // NOTE: it seems that when the non-free qsv driver is not installed then we cant use
            // -b:v. This might be a way to detect whether we can use -b:v flag but im not too
            // sure.
            if !self.hw_scaling_supported() {
                args.push("-maxrate".into());
                args.push(bitrate.to_string());
            } else {
                args.push("-b:v".into());
                args.push(bitrate.to_string());
            }
        }

        args.append(&mut vec![
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
            "-frag_duration".into(),
            "5000000".into(),
        ]);

        args.append(&mut super::video::get_discont_flags(&ctx));

        args.append(&mut vec![
            "-f".into(),
            "hls".into(),
            "-start_number".into(),
            start_num.clone(),
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

        args.append(&mut vec![
            "-hls_time".into(),
            ctx.output_ctx.target_gop.to_string(),
        ]);

        args.append(&mut vec![
            "-force_key_frames".into(),
            // NOTE: This might fix the seeking bug
            format!("expr:gte(t,n_forced*{})", ctx.output_ctx.target_gop),
            "-sc_threshold:v:0".into(),
            "0".into(),
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
        let decode_entrypoint = "VAEntrypointVLD".to_string();

        if !["h264", "hevc"].contains(&ctx.input_ctx.codec.as_str()) {
            return Err(NightfallError::ProfileNotSupported(
                "Profile only supports decoding h264 or h265 video streams.".into(),
            ));
        }

        // NOTE: Checks if the HWAccel device supports decoding HEVC content.
        if ctx.input_ctx.codec == "hevc"
            && self
                .profiles
                .iter()
                .find(|x| {
                    x.name == "VAProfileHEVCMain" && x.entrypoints.contains(&decode_entrypoint)
                })
                .is_none()
        {
            return Err(NightfallError::ProfileNotSupported(
                "HW Acceleration device doesnt support decoding hevc content.".into(),
            ));
        }

        if ctx.output_ctx.codec != "h264" {
            return Err(NightfallError::ProfileNotSupported(
                "Profile only supports h264 output streams.".into(),
            ));
        }

        let profile = match [ctx.input_ctx.codec.as_str(), ctx.input_ctx.profile.as_str()] {
            ["h264", "High"] => "VAProfileH264High",
            ["h264", "Main"] => "VAProfileH264Main",
            ["h264", "Baseline"] => "VAProfileH264Baseline",
            ["hevc", "Main"] => "VAProfileHEVCMain",
            ["hevc", "Main 10"] => "VAProfileHEVCMain10",
            [codec, profile] => {
                return Err(NightfallError::ProfileNotSupported(format!(
                    "Profile {} for {} not supported by device.",
                    profile, codec
                )))
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
