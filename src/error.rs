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
}
