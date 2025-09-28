use crate::os::OsErr;

#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    #[error("OS error: {0}")]
    OsErr(#[from] OsErr),
}

pub type Result<T> = core::result::Result<T, Error>;
