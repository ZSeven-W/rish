use std::io;

use rish_content::StoreError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VerifiedImageRecordError {
    #[error("verified image record I/O failed: {0}")]
    Io(#[from] io::Error),

    #[error("verified image content-store operation failed: {0}")]
    Store(#[from] StoreError),

    #[error("verified image record JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),

    #[error("verified image record JSON failed bounded structural validation")]
    JsonShape,

    #[error("unsupported verified image record schema version: {0}")]
    UnsupportedSchema(u32),

    #[error("verified image record exceeds its {maximum}-byte file limit")]
    RecordTooLarge { maximum: u64 },

    #[error("verified image locator exceeds its {maximum}-byte file limit")]
    LocatorTooLarge { maximum: u64 },

    #[error("verified image record field {field} exceeds its limit")]
    FieldLimit { field: &'static str },

    #[error("verified image record field {field} is invalid")]
    InvalidField { field: &'static str },

    #[error("verified image record does not match the requested immutable digest")]
    ResolvedDigestMismatch,

    #[error("verified image record graph is inconsistent at {component}")]
    GraphMismatch { component: &'static str },

    #[error("verified image record has no atomic locator")]
    MissingLocator,

    #[error("verified image record pin is missing")]
    MissingRecordPin,

    #[error("verified image graph pin set is incomplete or inconsistent")]
    GraphPinMismatch,
}
