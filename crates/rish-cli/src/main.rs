use std::env;
use std::process::ExitCode;

use rish_core::{GuestCommand, Platform, PrivilegeMode};
use rish_runtime::{OffloadRegistry, Planner, portable_offload_profile};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rish: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let platform = args
        .next()
        .ok_or_else(usage)?
        .parse::<PlatformArgument>()?
        .0;
    let program = args.next().ok_or_else(usage)?;
    let command = GuestCommand::new(program, args);
    let profile = portable_offload_profile(platform, PrivilegeMode::AppSandbox);
    let planner = Planner::new(profile, OffloadRegistry::portable_defaults());
    let plan = planner.plan(&command).map_err(|error| error.to_string())?;

    println!(
        "{}",
        serde_json::to_string_pretty(&plan).map_err(|error| error.to_string())?
    );
    Ok(())
}

struct PlatformArgument(Platform);

impl std::str::FromStr for PlatformArgument {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "ios" => Ok(Self(Platform::Ios)),
            "android" => Ok(Self(Platform::Android)),
            "harmony" => Ok(Self(Platform::Harmony)),
            "linux" => Ok(Self(Platform::Linux)),
            _ => Err(format!("unknown platform: {value}\n{}", usage())),
        }
    }
}

fn usage() -> String {
    "usage: rish-cli <ios|android|harmony|linux> <program> [args...]".to_owned()
}
