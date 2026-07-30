use crate::{AppletError, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppletOutput {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl AppletOutput {
    #[must_use]
    pub fn success(stdout: impl Into<Vec<u8>>) -> Self {
        Self {
            exit_code: 0,
            stdout: stdout.into(),
            stderr: Vec::new(),
        }
    }

    #[must_use]
    pub fn status(exit_code: i32) -> Self {
        Self {
            exit_code,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    #[must_use]
    pub fn failure(exit_code: i32, stderr: impl Into<Vec<u8>>) -> Self {
        Self {
            exit_code,
            stdout: Vec::new(),
            stderr: stderr.into(),
        }
    }

    pub(crate) fn enforce_limit(self, limit: usize) -> Result<Self> {
        let size = self
            .stdout
            .len()
            .checked_add(self.stderr.len())
            .ok_or(AppletError::OutputLimit { limit })?;
        if size > limit {
            return Err(AppletError::OutputLimit { limit });
        }
        Ok(self)
    }
}

pub(crate) fn push_bounded(output: &mut Vec<u8>, bytes: &[u8], limit: usize) -> Result<()> {
    let next = output
        .len()
        .checked_add(bytes.len())
        .ok_or(AppletError::OutputLimit { limit })?;
    if next > limit {
        return Err(AppletError::OutputLimit { limit });
    }
    output.extend_from_slice(bytes);
    Ok(())
}
