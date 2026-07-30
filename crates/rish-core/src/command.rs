use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::CapabilityRequirement;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GuestCommand {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_cwd")]
    pub cwd: String,
    #[serde(default)]
    pub stdin: Vec<u8>,
}

fn default_cwd() -> String {
    "/".to_owned()
}

impl GuestCommand {
    #[must_use]
    pub fn new(program: impl Into<String>, args: impl IntoIterator<Item = String>) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().collect(),
            env: BTreeMap::new(),
            cwd: default_cwd(),
            stdin: Vec::new(),
        }
    }

    #[must_use]
    pub fn basename(&self) -> &str {
        self.program
            .rsplit_once('/')
            .map_or(self.program.as_str(), |(_, name)| name)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostCall {
    pub protocol_version: u32,
    pub id: u64,
    pub operation: String,
    pub command: GuestCommand,
    #[serde(default)]
    pub requirements: Vec<CapabilityRequirement>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostReply {
    pub exit_code: i32,
    #[serde(default)]
    pub stdout: Vec<u8>,
    #[serde(default)]
    pub stderr: Vec<u8>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputChunk {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ExecutionPath {
    Builtin,
    NativeOffload { operation: String },
    NativeLinux,
    VirtualMachine,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionOutcome {
    pub exit_code: i32,
    #[serde(default)]
    pub stdout: Vec<u8>,
    #[serde(default)]
    pub stderr: Vec<u8>,
    pub path: ExecutionPath,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl ExecutionOutcome {
    #[must_use]
    pub fn success(stdout: impl Into<Vec<u8>>, path: ExecutionPath) -> Self {
        Self {
            exit_code: 0,
            stdout: stdout.into(),
            stderr: Vec::new(),
            path,
            warnings: Vec::new(),
        }
    }
}
