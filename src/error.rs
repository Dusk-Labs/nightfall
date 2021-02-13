pub type Result<T> = std::result::Result<T, NightfallError>;

#[derive(Debug)]
pub enum NightfallError {
    SessionDoesntExist,
    ChunkNotDone,
    Timeout,
    EarlyTimeout,
    Aborted,
}
