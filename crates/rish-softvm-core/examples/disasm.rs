//! Diagnostic disassembler: prints decoded instructions for a raw binary
//! image (e.g. the uncompressed vmlinux.bin payload of a bzImage).
//!
//! Usage:
//!   cargo run -p rish-softvm-core --example disasm -- \
//!     --file payload.bin --base 0x2ffd000 --start 0x35bd8c0 --len 0x1a0

use std::{env, fs, process::ExitCode};

use iced_x86::{Decoder, DecoderOptions, Formatter, IntelFormatter};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("disasm: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut file = None;
    let mut base = 0_u64;
    let mut start = None;
    let mut len = 0x100_usize;
    let args = env::args().skip(1).collect::<Vec<_>>();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--file" => {
                index += 1;
                file = Some(args.get(index).ok_or("--file needs a path")?.clone());
            }
            "--base" => {
                index += 1;
                base = parse_u64(args.get(index), "--base")?;
            }
            "--start" => {
                index += 1;
                start = Some(parse_u64(args.get(index), "--start")?);
            }
            "--len" => {
                index += 1;
                len = parse_u64(args.get(index), "--len")? as usize;
            }
            other => return Err(format!("unknown argument {other}")),
        }
        index += 1;
    }
    let file = file.ok_or("--file is required")?;
    let image = fs::read(&file).map_err(|error| error.to_string())?;
    let start = start.unwrap_or(base);
    let offset = start.checked_sub(base).ok_or("start is below base")? as usize;
    if offset + len > image.len() {
        return Err(format!("range {:#x}+{len:#x} exceeds image", start));
    }
    let bytes = &image[offset..offset + len];
    let mut decoder = Decoder::with_ip(64, bytes, start, DecoderOptions::NONE);
    let mut formatter = IntelFormatter::new();
    let mut output = String::new();
    let end = start + len as u64;
    while decoder.can_decode() && decoder.ip() < end {
        let instruction = decoder.decode();
        if instruction.is_invalid() {
            println!("{:#x}: invalid", decoder.ip());
            break;
        }
        output.clear();
        formatter.format(&instruction, &mut output);
        let pos = (instruction.ip() - start) as usize;
        let bytes = &bytes[pos..pos + instruction.len()];
        let hex: Vec<String> = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        println!("{:#x}: {:<40} {}", instruction.ip(), output, hex.join(" "));
    }
    Ok(())
}

fn parse_u64(value: Option<&String>, label: &str) -> Result<u64, String> {
    let text = value.ok_or(format!("{label} needs a number"))?;
    let text = text.strip_prefix("0x").unwrap_or(text);
    u64::from_str_radix(text, 16).map_err(|error| error.to_string())
}
