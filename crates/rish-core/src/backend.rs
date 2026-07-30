use crate::{CapabilityProfile, ExecutionOutcome, GuestCommand, HostCall, HostReply, RuntimeError};

pub trait HostBridge: Send + Sync {
    fn invoke(&self, call: &HostCall) -> Result<HostReply, RuntimeError>;
}

pub trait ExecutionBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> &CapabilityProfile;
    fn execute(&self, command: &GuestCommand) -> Result<ExecutionOutcome, RuntimeError>;
}
