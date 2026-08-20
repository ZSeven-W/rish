use rish_guest_protocol::{ErrorCode, RemoteError};

pub const DEFAULT_STREAM_CHUNK_SIZE: u32 = 32 * 1024;
pub const DEFAULT_STREAM_OUTPUT_LIMIT: u64 = 4 * 1024 * 1024;
pub const DEFAULT_MAX_CONCURRENT_EXEC: u32 = 4;

const ABSOLUTE_MAX_CONCURRENT_EXEC: u32 = 16;
pub(super) const ABSOLUTE_MAX_STREAM_CHUNK_SIZE: u32 = 32 * 1024;
pub(super) const ABSOLUTE_MAX_STREAM_OUTPUT: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeExecutionConfig {
    pub max_concurrent_exec: u32,
    pub max_stream_chunk_size: u32,
    pub max_stdout_bytes: u64,
    pub max_stderr_bytes: u64,
}

impl Default for NativeExecutionConfig {
    fn default() -> Self {
        Self {
            max_concurrent_exec: DEFAULT_MAX_CONCURRENT_EXEC,
            max_stream_chunk_size: DEFAULT_STREAM_CHUNK_SIZE,
            max_stdout_bytes: DEFAULT_STREAM_OUTPUT_LIMIT,
            max_stderr_bytes: DEFAULT_STREAM_OUTPUT_LIMIT,
        }
    }
}

impl NativeExecutionConfig {
    pub(super) fn validate(&self) -> Result<(), RemoteError> {
        if !(1..=ABSOLUTE_MAX_CONCURRENT_EXEC).contains(&self.max_concurrent_exec) {
            return Err(invalid_config(format!(
                "max_concurrent_exec must be between 1 and {ABSOLUTE_MAX_CONCURRENT_EXEC}"
            )));
        }
        if !(1..=ABSOLUTE_MAX_STREAM_CHUNK_SIZE).contains(&self.max_stream_chunk_size) {
            return Err(invalid_config(format!(
                "max_stream_chunk_size must be between 1 and {ABSOLUTE_MAX_STREAM_CHUNK_SIZE}"
            )));
        }
        if self.max_stdout_bytes > ABSOLUTE_MAX_STREAM_OUTPUT
            || self.max_stderr_bytes > ABSOLUTE_MAX_STREAM_OUTPUT
        {
            return Err(invalid_config(format!(
                "stream output limits must not exceed {ABSOLUTE_MAX_STREAM_OUTPUT} bytes"
            )));
        }
        Ok(())
    }
}

fn invalid_config(message: impl Into<String>) -> RemoteError {
    RemoteError::new(ErrorCode::InvalidRequest, message)
}
