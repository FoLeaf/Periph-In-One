use std::{error::Error, fmt};

/// Machine-readable failure categories. Applications choose their own messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    Transport,
    Timeout,
    InvalidResponse,
    PermissionDenied,
    Unsupported,
    Unavailable,
    NoPairedDevice,
    Conflict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HardwareError {
    pub kind: ErrorKind,
    /// Diagnostic context, never intended as an end-user status message.
    pub detail: String,
}

impl HardwareError {
    pub fn transport(error: impl fmt::Display) -> Self {
        Self::new(ErrorKind::Transport, error.to_string())
    }

    pub fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for HardwareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.detail)
    }
}

impl Error for HardwareError {}
