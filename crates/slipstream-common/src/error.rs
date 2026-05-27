use thiserror::Error;

#[derive(Debug, Error)]
pub enum SlipstreamError {
    #[error("invalid config: {0}")]
    ConfigValidation(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("connection error: {0}")]
    Connection(String),

    #[error("certificate error: {0}")]
    Certificate(String),

    #[error("transport error: {0}")]
    Transport(#[from] quinn::ConnectionError),

    #[error("write error: {0}")]
    Write(#[from] quinn::WriteError),

    #[error("stream closed: {0}")]
    ClosedStream(#[from] quinn::ClosedStream),
}
