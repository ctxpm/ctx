use std::fmt::Display;

/// Everything that can go wrong, phrased for the person at the terminal.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Shorthand for an error that is just a message.
pub fn err(message: impl Into<String>) -> Error {
    Error::Message(message.into())
}

/// Adds what was being attempted to an error, for example the file that could not be read.
pub trait Context<T> {
    fn context(self, what: impl Display) -> Result<T>;
}

impl<T, E: Display> Context<T> for std::result::Result<T, E> {
    fn context(self, what: impl Display) -> Result<T> {
        self.map_err(|error| err(format!("{what}: {error}")))
    }
}
