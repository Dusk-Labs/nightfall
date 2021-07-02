use super::ProfileContext;
use super::ProfileType;
use super::StreamType;
use super::TranscodingProfile;

use crate::session::CHUNK_SIZE;

pub struct H264TransmuxProfile;

impl TranscodingProfile for H264TransmuxProfile {
    fn profile_type(&self) -> ProfileType {
        ProfileType::Transmux
    }

    fn stream_type(&self) -> StreamType {
        StreamType::Video
    }

    fn build(&self, ctx: ProfileContext) -> Option<Vec<String>> {
        let start_num = ctx.start_num.to_string();
        let stream = format!("0:{}", ctx.stream);
        let init_seg = format!("{}_init.mp4", &start_num);
        let seg_name = format!("{}/%d.m4s", ctx.outdir);
        let outdir = format!("{}/playlist.m3u8", ctx.outdir);

        let mut args = vec![
            "-y".into(),
            "-ss".into(),
            (ctx.start_num * CHUNK_SIZE).to_string(),
            "-i".into(),
            ctx.file,
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
    fn supports(&self, codec_in: &str, codec_out: &str) -> bool {
        codec_in == codec_out && codec_in == "h264"
    }

    fn tag(&self) -> &str {
        "h264_copy"
    }
}

pub struct H264TranscodeProfile;

impl TranscodingProfile for H264TranscodeProfile {
    fn profile_type(&self) -> ProfileType {
        ProfileType::Transcode
    }

    fn stream_type(&self) -> StreamType {
        StreamType::Video
    }

    fn build(&self, ctx: ProfileContext) -> Option<Vec<String>> {
        let start_num = ctx.start_num.to_string();
        let stream = format!("0:{}", ctx.stream);
        let init_seg = format!("{}_init.mp4", &start_num);
        let seg_name = format!("{}/%d.m4s", ctx.outdir);
        let outdir = format!("{}/playlist.m3u8", ctx.outdir);

        let mut args = vec![
            "-y".into(),
            "-ss".into(),
            (ctx.start_num * CHUNK_SIZE).to_string(),
            "-i".into(),
            ctx.file,
            "-copyts".into(),
            "-map".into(),
            stream,
            "-c:0".into(),
            "libx264".into(),
            "-preset".into(),
            "veryfast".into(),
        ];

        if let Some(height) = ctx.height {
            let width = ctx.width.unwrap_or(-2); // defaults to scaling by 2
            args.push("-vf".into());
            args.push(format!("scale={}:{}", height, width));
        }

        if let Some(bitrate) = ctx.bitrate {
            args.push("-b:v".into());
            args.push(bitrate.to_string());
        }

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

    fn supports(&self, _: &str, codec_out: &str) -> bool {
        codec_out == "h264"
    }

    fn tag(&self) -> &str {
        "h264"
    }
}

pub struct RawVideoTranscodeProfile;

impl TranscodingProfile for RawVideoTranscodeProfile {
    fn profile_type(&self) -> ProfileType {
        ProfileType::Transcode
    }

    fn stream_type(&self) -> StreamType {
        StreamType::Video
    }

    fn build(&self, ctx: ProfileContext) -> Option<Vec<String>> {
        let mut args = vec!["-y".into()];

        if let Some(seek) = ctx.seek {
            let flag = if seek.is_positive() {
                "-ss".into()
            } else {
                "-sseof".into()
            };

            args.push(flag);
            args.push(seek.to_string());
        }

        if let Some(max_to_transcode) = ctx.max_to_transcode {
            args.push("-t".into());
            args.push(max_to_transcode.to_string());
        }

        args.append(&mut vec!["-map".into(), format!("0:{}", ctx.stream)]);
        args.append(&mut vec!["-c:v".into(), "rawvideo".into()]);
        args.append(&mut vec![
            "-flags2".into(),
            "-pix_fmt".into(),
            "rgb24".into(),
        ]);
        args.append(&mut vec!["-preset".into(), "ultrafast".into()]);

        if let Some(height) = ctx.height {
            let width = ctx.width.unwrap_or(-2);

            args.append(&mut vec![
                "-vf".into(),
                format!("scale={}:{}", height, width),
            ]);
        }

        args.append(&mut vec!["-f".into(), "data".into(), "-".into()]);

        Some(args)
    }

    fn supports(&self, _: &str, codec_out: &str) -> bool {
        codec_out == "rawvideo"
    }

    fn tag(&self) -> &str {
        "rawvideo"
    }

    fn is_stdio_stream(&self) -> bool {
        true
    }
}

pub struct VaapiTranscodeProfile;

impl TranscodingProfile for VaapiTranscodeProfile {
    fn profile_type(&self) -> ProfileType {
        ProfileType::HardwareTranscode
    }

    fn stream_type(&self) -> StreamType {
        StreamType::Video
    }

    fn build(&self, ctx: ProfileContext) -> Option<Vec<String>> {
        let start_num = ctx.start_num.to_string();
        let stream = format!("0:{}", ctx.stream);
        let init_seg = format!("{}_init.mp4", &start_num);
        let seg_name = format!("{}/%d.m4s", ctx.outdir);
        let outdir = format!("{}/playlist.m3u8", ctx.outdir);

        let mut args = vec![
            "-hwaccel".into(),
            "vaapi".into(),
            "-hwaccel_output_format".into(),
            "vaapi".into(),
            "-y".into(),
            "-ss".into(),
            (ctx.start_num * CHUNK_SIZE).to_string(),
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
            "format=nv12,hwupload".into(),
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
    fn supports(&self, codec_in: &str, codec_out: &str) -> bool {
        codec_in == codec_out && codec_in == "h264"
    }

    fn tag(&self) -> &str {
        "h264_vaapi"
    }
}
