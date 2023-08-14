use super::ProfileContext;
use super::ProfileType;
use super::StreamType;
use super::TranscodingProfile;

use crate::NightfallError;

#[derive(Debug)]
pub struct H264TransmuxProfile;

impl TranscodingProfile for H264TransmuxProfile {
    fn profile_type(&self) -> ProfileType {
        ProfileType::Transmux
    }

    fn stream_type(&self) -> StreamType {
        StreamType::Video
    }

    fn name(&self) -> &str {
        "H264TransmuxProfile"
    }

    fn build(&self, ctx: ProfileContext) -> Option<Vec<String>> {
        let start_num = ctx.output_ctx.start_num.to_string();
        let stream = format!("0:{}", ctx.input_ctx.stream);
        let init_seg = format!("{}_init.mp4", &start_num);
        let seg_name = format!("{}/%d.m4s", ctx.output_ctx.outdir);
        let outdir = format!("{}/playlist.m3u8", ctx.output_ctx.outdir);

        let mut args = vec![
            "-y".into(),
            "-ss".into(),
            (ctx.output_ctx.start_num * ctx.output_ctx.target_gop).to_string(),
            "-i".into(),
            ctx.file.clone(),
            "-copyts".into(),
            "-map".into(),
            stream,
            "-c:0".into(),
            "copy".into(),
        ];

        args.append(&mut vec![
            "-start_at_zero".into(),
            "-vsync".into(),
            "passthrough".into(),
            "-avoid_negative_ts".into(),
            "disabled".into(),
            "-max_muxing_queue_size".into(),
            "2048".into(),
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

        args.append(&mut get_discont_flags(&ctx));

        args.append(&mut vec![
            "-force_key_frames".into(),
            format!("expr:gte(t,n_forced*{})", ctx.output_ctx.target_gop),
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
        if ctx.output_ctx.height.is_some()
            || ctx.output_ctx.width.is_some()
            || ctx.output_ctx.bitrate.is_some()
        {
            return Err(NightfallError::ProfileNotSupported(
                "Transmuxed streams cannot be resized.".into(),
            ));
        }

        if ctx.input_ctx.codec == ctx.output_ctx.codec && ctx.input_ctx.codec == "h264" {
            return Ok(());
        }

        Err(NightfallError::ProfileNotSupported(
            "Profile only supports h264 input and output codecs.".into(),
        ))
    }

    fn tag(&self) -> &str {
        "h264_copy"
    }
}

#[derive(Debug)]
pub struct H264TranscodeProfile;

impl TranscodingProfile for H264TranscodeProfile {
    fn profile_type(&self) -> ProfileType {
        ProfileType::Transcode
    }

    fn stream_type(&self) -> StreamType {
        StreamType::Video
    }

    fn name(&self) -> &str {
        "H264TranscodeProfile"
    }

    fn build(&self, ctx: ProfileContext) -> Option<Vec<String>> {
        let start_num = ctx.output_ctx.start_num.to_string();
        let stream = format!("0:{}", ctx.input_ctx.stream);
        let init_seg = format!("{}_init.mp4", &start_num);
        let seg_name = format!("{}/%d.m4s", ctx.output_ctx.outdir);
        let outdir = format!("{}/playlist.m3u8", ctx.output_ctx.outdir);

        let mut args = vec![
            "-y".into(),
            "-ss".into(),
            (ctx.output_ctx.start_num * ctx.output_ctx.target_gop).to_string(),
            "-i".into(),
            ctx.file.clone(),
            "-copyts".into(),
            "-map".into(),
            stream,
            "-c:0".into(),
            "libx264".into(),
            "-preset".into(),
            "veryfast".into(),
        ];

        if let Some(height) = ctx.output_ctx.height {
            let width = ctx.output_ctx.width.unwrap_or(-2); // defaults to scaling by 2
            args.push("-vf".into());
            args.push(format!("scale={}:{}", height, width));
        }

        if let Some(bitrate) = ctx.output_ctx.bitrate {
            args.push("-b:v".into());
            args.push(bitrate.to_string());
        }

        args.append(&mut vec![
            "-vsync".into(),
            "passthrough".into(),
            "-avoid_negative_ts".into(),
            "make_non_negative".into(),
            "-max_muxing_queue_size".into(),
            "2048".into(),
        ]);

        args.append(&mut vec![
            "-f".into(),
            "hls".into(),
            "-start_number".into(),
            start_num,
        ]);

        args.append(&mut get_discont_flags(&ctx));

        // needed so that in progress segments are named `tmp` and then renamed after the data is
        // on disk.
        // This in theory practically prevents the web server from returning a segment that is
        // in progress.
        args.append(&mut vec![
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
            format!("expr:gte(t,n_forced*{})", ctx.output_ctx.target_gop),
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

    fn supports(&self, ctx: &ProfileContext) -> Result<(), NightfallError> {
        if ctx.output_ctx.codec == "h264" {
            return Ok(());
        }

        Err(NightfallError::ProfileNotSupported(format!(
            "Got output codec {} but profile only supports `h264`.",
            ctx.output_ctx.codec
        )))
    }

    fn tag(&self) -> &str {
        "h264"
    }
}

#[derive(Debug)]
pub struct RawVideoTranscodeProfile;

impl TranscodingProfile for RawVideoTranscodeProfile {
    fn profile_type(&self) -> ProfileType {
        ProfileType::Transcode
    }

    fn stream_type(&self) -> StreamType {
        StreamType::Video
    }

    fn name(&self) -> &str {
        "RawVideoTranscodeProfile"
    }

    fn build(&self, ctx: ProfileContext) -> Option<Vec<String>> {
        let mut args = vec!["-y".into()];

        if let Some(seek) = ctx.input_ctx.seek {
            let flag = if seek.is_positive() {
                "-ss".into()
            } else {
                "-sseof".into()
            };

            args.push(flag);
            args.push(seek.to_string());
        }

        if let Some(max_to_transcode) = ctx.output_ctx.max_to_transcode {
            args.push("-t".into());
            args.push(max_to_transcode.to_string());
        }

        args.append(&mut vec![
            "-map".into(),
            format!("0:{}", ctx.input_ctx.stream),
        ]);
        args.append(&mut vec!["-c:v".into(), "rawvideo".into()]);
        args.append(&mut vec![
            "-flags2".into(),
            "-pix_fmt".into(),
            "rgb24".into(),
        ]);
        args.append(&mut vec!["-preset".into(), "ultrafast".into()]);

        if let Some(height) = ctx.output_ctx.height {
            let width = ctx.output_ctx.width.unwrap_or(-2);

            args.append(&mut vec![
                "-vf".into(),
                format!("scale={}:{}", height, width),
            ]);
        }

        args.append(&mut vec!["-f".into(), "data".into(), "-".into()]);

        Some(args)
    }

    fn supports(&self, ctx: &ProfileContext) -> Result<(), NightfallError> {
        if ctx.output_ctx.codec == "rawvideo" {
            return Ok(());
        }

        Err(NightfallError::ProfileNotSupported(format!(
            "Codec {} is not supported.",
            ctx.output_ctx.codec
        )))
    }

    fn tag(&self) -> &str {
        "rawvideo"
    }

    fn is_stdio_stream(&self) -> bool {
        true
    }
}

pub(super) fn get_discont_flags(ctx: &ProfileContext) -> Vec<String> {
    // these args are needed if we start a new stream in the middle of a old one, such as when
    // seeking. These args will reset the base decode ts to equal the earliest presentation
    // timestamp.
    if ctx.output_ctx.start_num > 0 {
        vec![
            "-hls_segment_options".into(),
            "movflags=frag_custom+dash+delay_moov+frag_discont".into(),
        ]
    } else {
        vec![
            "-hls_segment_options".into(),
            "movflags=frag_custom+dash+delay_moov".into(),
        ]
    }
}
