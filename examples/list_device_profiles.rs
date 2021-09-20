use nightfall::profiles::*;

fn main() {
    let drain = slog::Discard;
    let log = slog::Logger::root(drain, slog::o!());

    profiles_init(log, "/usr/bin/ffmpeg".to_string());

    for profile in get_active_profiles() {
        println!("{:?}", profile);
    }
}
