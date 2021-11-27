use super::ProfileContext;
use super::ProfileType;
use super::StreamType;
use super::TranscodingProfile;

use crate::NightfallError;

/// Cuda(NVENC/NVDEC) transcoding profiles.
/// This is a nvidia exclusive transcoding profile that leverages cuda. This profile will
/// automatically be enabled if any of your GPUs support encoding and decoding h264 with the
/// profiles `Main`, `High` and `ConstrainedBaseline`. This profile will only transcode h264 input
/// streams.
#[cfg(unix)]
#[derive(Debug)]
pub struct CudaTranscodeProfile;

#[cfg(unix)]
impl TranscodingProfile for CudaTranscodeProfile {
    fn profile_type(&self) -> ProfileType {
        ProfileType::HardwareTranscode
    }

    fn stream_type(&self) -> StreamType {
        StreamType::Video
    }

    fn name(&self) -> &str {
        "CudaTranscodeProfile"
    }

    fn is_enabled(&self) -> Result<(), NightfallError> {
        // TODO: Add runtime profile support detection.
        return Ok(());
    }

    fn build(&self, ctx: ProfileContext) -> Option<Vec<String>> {
        let start_num = ctx.output_ctx.start_num.to_string();
        let stream = format!("0:{}", ctx.input_ctx.stream);
        let init_seg = format!("{}_init.mp4", &start_num);
        let seg_name = format!("{}/%d.m4s", ctx.output_ctx.outdir);
        let outdir = format!("{}/playlist.m3u8", ctx.output_ctx.outdir);

        // ffmpeg -hwaccel cuda -hwaccel_output_format cuda -i input -c:v h264_nvenc -preset slow output
        let mut args = vec![
            "-hwaccel".into(),
            "cuda".into(),
            "-hwaccel_output_format".into(),
            "cuda".into(),
            "-y".into(),
            "-ss".into(),
            (ctx.output_ctx.start_num * ctx.output_ctx.target_gop).to_string(),
            "-i".into(),
            ctx.file.clone(),
            "-copyts".into(),
            "-map".into(),
            stream,
            "-c:0".into(),
            "h264_nvenc".into(),
            "-bf".into(),
            "0".into(),
        ];

        if let Some(height) = ctx.output_ctx.height {
            let width = ctx.output_ctx.width.unwrap_or(-2); // defaults to scaling by 2
            args.push("-vf".into());
            args.push(format!("scale_cuda={}:{}", height, width));
        }

        if let Some(bitrate) = ctx.output_ctx.bitrate {
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

    /// This profile technically could work on any codec since the codec is just `copy` here, but
    /// the container doesnt support it, so we will be constricting it down.
    fn supports(&self, _ctx: &ProfileContext) -> Result<(), NightfallError> {
        // TODO: At runtime check which file formats are supported by the current gpu for enc/dec.
        Ok(())
    }

    fn tag(&self) -> &str {
        "h264_cuda"
    }
}
