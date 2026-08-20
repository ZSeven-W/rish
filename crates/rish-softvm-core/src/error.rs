//! Interpreter error surface.

use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CpuError {
    InvalidConfig(String),
    GuestFault(String),
    UnimplementedInstruction {
        code: String,
        address: u64,
        bytes: Vec<u8>,
    },
    TripleFault,
    Halted,
}

impl fmt::Display for CpuError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(message) => write!(formatter, "invalid config: {message}"),
            Self::GuestFault(message) => write!(formatter, "guest fault: {message}"),
            Self::UnimplementedInstruction { code, address, .. } => {
                write!(
                    formatter,
                    "unimplemented instruction {code} at {address:#x}"
                )
            }
            Self::TripleFault => formatter.write_str("triple fault"),
            Self::Halted => formatter.write_str("guest halted"),
        }
    }
}

impl std::error::Error for CpuError {}
