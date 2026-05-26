use thiserror::Error;

#[derive(Debug, Error)]
pub enum SlipstreamError {
    #[error("invalid config: {0}")]
    ConfigValidation(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
