//! Typed errors for the decode/geometry boundary. Errors are explicit: the
//! crate never substitutes a preview or a default image for a failed decode.

#[derive(Debug)]
pub enum DevelopError {
    /// A registered [`crate::CancelToken`] observed cancellation.
    Cancelled,
    /// The RAW decoder rejected the input (unknown container, unsupported
    /// compression, truncated file, ...). The message is the decoder's own
    /// error text, kept verbatim for visibility.
    Decode(String),
    /// The decode produced data this crate cannot own (for example a
    /// four-color intermediate).
    Unsupported(String),
    /// The input bytes cannot even be attempted (empty input).
    InvalidInput(String),
    /// Geometry input violated the oriented-frame contract.
    Geometry(String),
}

impl std::fmt::Display for DevelopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DevelopError::Cancelled => write!(f, "decode cancelled"),
            DevelopError::Decode(message) => write!(f, "RAW decode failed: {message}"),
            DevelopError::Unsupported(message) => write!(f, "unsupported decode result: {message}"),
            DevelopError::InvalidInput(message) => write!(f, "invalid decode input: {message}"),
            DevelopError::Geometry(message) => write!(f, "invalid geometry: {message}"),
        }
    }
}

impl std::error::Error for DevelopError {}
