use nightfall::profiles::*;
use std::env;

fn main() {
    let mut args = env::args();
    args.next(); // skip exe name

    let profile = args.next().expect("Usage: dump_args <profile> <file>");
    let file = args.next().expect("Usage: dump_args <profile> <file>");

    profiles_init("/usr/bin/ffmpeg".to_string());

    let profile = get_active_profiles()
        .into_iter()
        .find(|x| x.tag() == &profile)
        .expect("Failed to find specified profile");

    let ctx = ProfileContext {
        file,
        pre_args: vec![],
        input_ctx: Default::default(),
        output_ctx: Default::default(),
        ffmpeg_bin: "/usr/bin/ffmpeg".into(),
    };

    println!("{}", profile.build(ctx).unwrap().join(" "));
}
