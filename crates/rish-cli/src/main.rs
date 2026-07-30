use std::env;
use std::fs;
use std::process::ExitCode;

use rish_core::{GuestCommand, Platform, PrivilegeMode};
use rish_oci::{ImageConfiguration, plan_image};
use rish_registry::ImageReference;
use rish_runtime::{OffloadRegistry, Planner, portable_offload_profile};
use serde_json::json;

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
    let args = env::args().skip(1).collect::<Vec<_>>();
    let Some(action) = args.first().map(String::as_str) else {
        return Err(usage());
    };

    match action {
        "plan" => plan_command(&args[1..]),
        "image-plan" => plan_image_config(&args[1..]),
        "image-ref" => inspect_image_reference(&args[1..]),
        _ => plan_command(&args),
    }
}

fn plan_command(args: &[String]) -> Result<(), String> {
    let platform = parse_platform(args.first())?;
    let program = args.get(1).ok_or_else(usage)?;
    let command = GuestCommand::new(program.clone(), args[2..].iter().cloned());
    let profile = portable_offload_profile(platform, PrivilegeMode::AppSandbox);
    let planner = Planner::new(profile, OffloadRegistry::portable_defaults());
    let plan = planner.plan(&command).map_err(|error| error.to_string())?;

    print_json(&plan)
}

fn plan_image_config(args: &[String]) -> Result<(), String> {
    let platform = parse_platform(args.first())?;
    let path = args.get(1).ok_or_else(usage)?;
    if args.len() != 2 {
        return Err(usage());
    }
    let bytes = fs::read(path).map_err(|error| format!("cannot read {path}: {error}"))?;
    let image = serde_json::from_slice::<ImageConfiguration>(&bytes)
        .map_err(|error| format!("invalid OCI image config: {error}"))?;
    let profile = portable_offload_profile(platform, PrivilegeMode::AppSandbox);
    let plan = plan_image(&image, &profile);
    print_json(&plan)
}

fn inspect_image_reference(args: &[String]) -> Result<(), String> {
    let value = args.first().ok_or_else(usage)?;
    if args.len() != 1 {
        return Err(usage());
    }
    let reference = value
        .parse::<ImageReference>()
        .map_err(|error| error.to_string())?;
    print_json(&json!({
        "normalized": reference.to_string(),
        "registry": reference.registry(),
        "repository": reference.repository(),
        "tag": reference.tag(),
        "digest": reference.digest().map(ToString::to_string),
        "manifest_path": reference.manifest_path(),
        "pull_scope": reference.pull_scope(),
    }))
}

fn parse_platform(value: Option<&String>) -> Result<Platform, String> {
    value
        .ok_or_else(usage)?
        .parse::<PlatformArgument>()
        .map(|argument| argument.0)
}

fn print_json(value: &impl serde::Serialize) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
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
    [
        "usage:",
        "  rish-cli plan <ios|android|harmony|linux> <program> [args...]",
        "  rish-cli image-plan <ios|android|harmony|linux> <config.json>",
        "  rish-cli image-ref <registry/repository[:tag|@digest]>",
        "",
        "The legacy form `rish-cli <platform> <program> [args...]` is also accepted.",
    ]
    .join("\n")
}
