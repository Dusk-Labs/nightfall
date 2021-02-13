#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum StreamType {
    Video,
    Audio,
}

#[derive(Clone, Copy)]
pub enum Profile {
    Direct,
    High,
    Medium,
    Low,
    Audio,
}

impl Profile {
    pub fn to_params(&self) -> (Vec<&str>, &str) {
        match self {
            Self::Direct => (vec!["-c:0", "copy"], "direct"),
            Self::High => (
                vec![
                    "-c:0",
                    "libx264",
                    "-b:v",
                    "5M",
                    "-preset:0",
                    "veryfast",
                    "-vf",
                    "scale=1280:-2",
                ],
                "5000kb",
            ),
            Self::Medium => (
                vec![
                    "-c:0",
                    "libx264",
                    "-b:v",
                    "2M",
                    "-preset",
                    "ultrafast",
                    "-vf",
                    "scale=720:-2",
                ],
                "2000kb",
            ),
            Self::Low => (
                vec![
                    "-c:0",
                    "libx264",
                    "-b:v",
                    "1M",
                    "-preset",
                    "ultrafast",
                    "-vf",
                    "scale=480:-2",
                ],
                "1000kb",
            ),
            Self::Audio => (vec![], "120kb"),
        }
    }

    pub fn from_string<T: AsRef<str>>(profile: T) -> Result<Self, ()> {
        Ok(match profile.as_ref() {
            "direct" => Self::Direct,
            "5000kb" => Self::High,
            "2000kb" => Self::Medium,
            "1000kb" => Self::Low,
            _ => return Err(()),
        })
    }
}
