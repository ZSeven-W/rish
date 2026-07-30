mod catalog;
mod context;
mod error;
mod filesystem;
mod misc;
mod output;
mod text;

use std::sync::Mutex;

use rish_core::{ExecutionOutcome, ExecutionPath, GuestCommand};

pub use catalog::{
    APPLETS, AppletCategory, AppletSpec, applet_spec, is_portable_applet, is_portable_command,
};
pub use context::{AppletContext, AppletLimits};
pub use error::{AppletError, Result};
pub use output::AppletOutput;

pub struct AppletExecutor {
    context: AppletContext,
}

// Portable applets are synchronous and intentionally serialize filesystem
// operations. This closes guest-controlled lstat/use races between concurrent
// applet requests; the embedding app must also keep the root private from
// unrelated host writers.
static APPLET_EXECUTION_LOCK: Mutex<()> = Mutex::new(());

impl AppletExecutor {
    #[must_use]
    pub const fn new(context: AppletContext) -> Self {
        Self { context }
    }

    #[must_use]
    pub const fn context(&self) -> &AppletContext {
        &self.context
    }

    pub fn execute(&self, command: &GuestCommand) -> Result<ExecutionOutcome> {
        let _execution_guard = APPLET_EXECUTION_LOCK
            .lock()
            .map_err(|_| AppletError::ExecutionLock)?;
        self.context.verify_root()?;

        let result = (|| {
            validate_command(&self.context, command)?;
            if command.stdin.len() > self.context.limits().max_input_bytes {
                return Err(AppletError::InputLimit {
                    limit: self.context.limits().max_input_bytes,
                });
            }
            if !is_portable_command(command) {
                return Err(AppletError::UnknownApplet(command.basename().to_owned()));
            }
            let Some(spec) = applet_spec(command.basename()) else {
                return Err(AppletError::UnknownApplet(command.basename().to_owned()));
            };
            if spec.mutates_filesystem {
                self.context.require_writable()?;
            }

            let output = match spec.category {
                AppletCategory::Text => text::execute(&self.context, command),
                AppletCategory::Filesystem => filesystem::execute(&self.context, command),
                AppletCategory::Core | AppletCategory::Identity | AppletCategory::Checksum => {
                    misc::execute(&self.context, command)
                }
            }?
            .enforce_limit(self.context.limits().max_output_bytes)?;

            Ok(ExecutionOutcome {
                exit_code: output.exit_code,
                stdout: output.stdout,
                stderr: output.stderr,
                path: ExecutionPath::PortableApplet {
                    name: spec.name.to_owned(),
                },
                warnings: Vec::new(),
            })
        })();

        self.context.verify_root()?;
        result
    }
}

fn validate_command(context: &AppletContext, command: &GuestCommand) -> Result<()> {
    const MAX_ARGUMENTS: usize = 256;
    const MAX_ARGUMENT_BYTES: usize = 1024 * 1024;
    const MAX_ENVIRONMENT_ENTRIES: usize = 1024;
    const MAX_ENVIRONMENT_BYTES: usize = 4 * 1024 * 1024;
    const MAX_SINGLE_VALUE_BYTES: usize = 64 * 1024;

    for (field, value) in [("user", context.user()), ("hostname", context.hostname())] {
        if value.is_empty()
            || value.len() > 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(AppletError::usage(
                "identity",
                format!("{field} must match [A-Za-z0-9._-] and be at most 64 bytes"),
            ));
        }
    }

    if command.program.is_empty()
        || command.program.len() > 4096
        || command.program.as_bytes().contains(&0)
    {
        return Err(AppletError::usage(
            "command",
            "program must be 1-4096 bytes and contain no NUL",
        ));
    }
    if command.cwd.is_empty()
        || !command.cwd.starts_with('/')
        || command.cwd.len() > 4096
        || command.cwd.as_bytes().contains(&0)
    {
        return Err(AppletError::usage(
            command.basename(),
            "cwd must be an absolute guest path of at most 4096 bytes",
        ));
    }
    context.guest_relative("/", &command.cwd)?;
    let argument_bytes = command.args.iter().try_fold(0usize, |total, value| {
        if value.len() > MAX_SINGLE_VALUE_BYTES || value.as_bytes().contains(&0) {
            return None;
        }
        total.checked_add(value.len())
    });
    if command.args.len() > MAX_ARGUMENTS
        || argument_bytes.is_none_or(|bytes| bytes > MAX_ARGUMENT_BYTES)
    {
        return Err(AppletError::usage(
            command.basename(),
            "argv exceeds the portable applet limits",
        ));
    }
    let environment_bytes = command.env.iter().try_fold(0usize, |total, (name, value)| {
        if name.is_empty()
            || name.contains('=')
            || name.len() > 4096
            || value.len() > MAX_SINGLE_VALUE_BYTES
            || name.as_bytes().contains(&0)
            || value.as_bytes().contains(&0)
        {
            return None;
        }
        total.checked_add(name.len())?.checked_add(value.len())
    });
    if command.env.len() > MAX_ENVIRONMENT_ENTRIES
        || environment_bytes.is_none_or(|bytes| bytes > MAX_ENVIRONMENT_BYTES)
    {
        return Err(AppletError::usage(
            command.basename(),
            "environment exceeds the portable applet limits",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
