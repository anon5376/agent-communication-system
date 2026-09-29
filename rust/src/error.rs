use std::fmt;

/// Error codes mirror BusError in src/core/types.ts. The CLI maps them to the
/// same exit codes (unauthorized/forbidden -> 3, anything else -> 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    Unauthorized,
    Forbidden,
    NotFound,
    Invalid,
    Conflict,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::Unauthorized => "unauthorized",
            Code::Forbidden => "forbidden",
            Code::NotFound => "not_found",
            Code::Invalid => "invalid",
            Code::Conflict => "conflict",
        }
    }
}

#[derive(Debug)]
pub struct BusError {
    pub code: Code,
    pub message: String,
}

impl BusError {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        BusError {
            code,
            message: message.into(),
        }
    }
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(Code::Unauthorized, message)
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(Code::Forbidden, message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(Code::NotFound, message)
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(Code::Invalid, message)
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(Code::Conflict, message)
    }
}

impl fmt::Display for BusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for BusError {}

pub type Result<T> = std::result::Result<T, BusError>;

impl From<rusqlite::Error> for BusError {
    fn from(error: rusqlite::Error) -> Self {
        match error {
            // SQLITE_BUSY after the timeout still surfaces as a conflict, matching the
            // 20-way claim-race expectation (losers see conflict, not a crash).
            rusqlite::Error::SqliteFailure(err, _)
                if err.code == rusqlite::ErrorCode::DatabaseBusy =>
            {
                BusError::conflict("database is busy")
            }
            _ => BusError::new(Code::Invalid, error.to_string()),
        }
    }
}

impl From<serde_json::Error> for BusError {
    fn from(error: serde_json::Error) -> Self {
        BusError::invalid(error.to_string())
    }
}

impl From<std::io::Error> for BusError {
    fn from(error: std::io::Error) -> Self {
        BusError::invalid(error.to_string())
    }
}
