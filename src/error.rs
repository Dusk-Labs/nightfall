use err_derive::Error;
use serde::Serialize;

pub type Result<T> = std::result::Result<T, NightfallError>;

#[derive(Clone, Debug, Error, Serialize)]
pub enum NightfallError {
    #[error(display = "The requested session doesnt exist")]
    SessionDoesntExist,
    #[error(display = "Chunk requested is not ready yet")]
    ChunkNotDone,
    #[error(display = "Request aborted")]
    Aborted,
    #[error(display = "Session manager died")]
    SessionManagerDied,
    #[error(display = "Failed to patch segment {}", 0)]
    SegmentPatchError(String),
    #[error(display = "Io Error")]
    IoError,
    #[error(display = "Box missing in segment.")]
    MissingSegmentBox
}

impl From<mp4::Error> for NightfallError {
    fn from(e: mp4::Error) -> Self {
        Self::SegmentPatchError(e.to_string())
    }
}

impl From<std::io::Error> for NightfallError {
    fn from(_: std::io::Error) -> Self {
        Self::IoError
    }
}
