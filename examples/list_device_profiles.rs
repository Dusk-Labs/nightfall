use nightfall::profiles::*;

fn main() {
    profiles_init("/usr/bin/ffmpeg".to_string());

    for profile in get_active_profiles() {
        println!("{:?}", profile);
    }
}
