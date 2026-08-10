use thiserror::Error;

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("requested device not found")]
    DeviceNotFound,
    #[error("audio backend thread closed unexpectedly")]
    BackendClosed,
    #[error("unsupported sample format: `{0:?}`")]
    UnsupportedSampleFormat(cpal::SampleFormat),
    #[error("no supported f32 output configuration")]
    NoSupportedConfig,
    #[error(transparent)]
    Cpal(#[from] cpal::Error),
    #[error(transparent)]
    Opus(#[from] opus::Error),
    #[error(transparent)]
    ResamplerConstruction(#[from] rubato::ResamplerConstructionError),
    #[error(transparent)]
    Resample(#[from] rubato::ResampleError),
    #[cfg(windows)]
    #[error(transparent)]
    Wasapi(#[from] wasapi::WasapiError),
}
