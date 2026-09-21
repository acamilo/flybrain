//! One error type carrying the oracle's message text.
//!
//! The TypeScript library signals every rejection by throwing an `Error` whose message the tests
//! match against. Those messages are part of the behaviour being ported, so they are reproduced
//! verbatim rather than restructured into a Rust error taxonomy.

use std::fmt;

/// An error with the same message the TypeScript oracle throws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: String,
}

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// `throw new Error(...)`.
macro_rules! bail {
    ($($argument:tt)*) => {
        return Err($crate::error::Error::new(format!($($argument)*)))
    };
}

pub(crate) use bail;
