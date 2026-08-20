//! Diagnostic host-side boot oracle for the rish x86_64 Linux guest.
//!
//! This tool boots the pinned guest assets under a stock QEMU process and
//! speaks the production guest protocol over a second serial port. It is a
//! development oracle: it does not gate on the TCTI provider contract and its
//! output must never be presented as a verified Full VM capability profile.

mod manifest;

use std::{
    env, fs,
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    process::{Child, Command, ExitCode, Stdio},
    thread,
    time::Duration,
};

use manifest::{LoadedManifest, sha256_file};
use rish_guest_protocol::{
    DEFAULT_MAX_FRAME_SIZE, Envelope, Hello, Message, PeerInfo, RequestId, SessionClient, SessionIo,
};

const ADVANCE_POLL: Duration = Duration::from_millis(50);

struct Options {
    manifest_path: PathBuf,
    qemu: PathBuf,
    socket_path: PathBuf,
    timeout: Duration,
    command: Vec<String>,
    stdin_file: Option<PathBuf>,
    skip_evidence: bool,
}

fn usage() -> &'static str {
    "usage: rish-guest-boot [--manifest PATH] [--qemu PATH] [--socket PATH]
            [--timeout SECONDS] [--stdin FILE] [--skip-evidence] --exec PROGRAM [ARGS...]
"
}

fn parse_args() -> Result<Options, String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let mut manifest_path = PathBuf::from("guest/x86_64/boot-manifest.json");
    let mut qemu = PathBuf::from("qemu-system-x86_64");
    let mut socket_path = PathBuf::from("/tmp/rish-guest-boot-control.sock");
    let mut timeout = Duration::from_secs(300);
    let mut command = Vec::new();
    let mut stdin_file = None;
    let mut skip_evidence = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--manifest" => {
                index += 1;
                manifest_path = args.get(index).ok_or("--manifest requires a path")?.into();
            }
            "--qemu" => {
                index += 1;
                qemu = args.get(index).ok_or("--qemu requires a path")?.into();
            }
            "--socket" => {
                index += 1;
                socket_path = args.get(index).ok_or("--socket requires a path")?.into();
            }
            "--timeout" => {
                index += 1;
                let seconds = args
                    .get(index)
                    .ok_or("--timeout requires seconds")?
                    .parse::<u64>()
                    .map_err(|error: std::num::ParseIntError| error.to_string())?;
                timeout = Duration::from_secs(seconds);
            }
            "--stdin" => {
                index += 1;
                stdin_file = Some(args.get(index).ok_or("--stdin requires a file")?.into());
            }
            "--skip-evidence" => skip_evidence = true,
            "--exec" => {
                command = args[index + 1..].to_vec();
                if command.is_empty() {
                    return Err("--exec requires at least a program".to_owned());
                }
                break;
            }
            "--help" | "-h" => return Err(usage().to_owned()),
            other => return Err(format!("unknown argument {other}")),
        }
        index += 1;
    }
    Ok(Options {
        manifest_path,
        qemu,
        socket_path,
        timeout,
        command,
        stdin_file,
        skip_evidence,
    })
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("rish-guest-boot: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<u8, String> {
    println!("rish-guest-boot: DIAGNOSTIC ORACLE");
    println!("This harness is a development tool. It does not issue a verified");
    println!("Full VM capability profile and is not a product backend.");
    let options = parse_args()?;
    let loaded = LoadedManifest::load(&options.manifest_path)?;
    let manifest = &loaded.manifest;
    println!(
        "guest: {} {} status={}",
        manifest.architecture.cpu, manifest.architecture.oci_platform, manifest.status
    );
    println!(
        "kernel: {} (sha256 {})",
        loaded.kernel_path.display(),
        sha256_file(&loaded.kernel_path)?
    );
    println!(
        "initramfs: {} (sha256 {})",
        loaded.initramfs_path.display(),
        sha256_file(&loaded.initramfs_path)?
    );

    let _ = fs::remove_file(&options.socket_path);
    let socket_dir = options
        .socket_path
        .parent()
        .ok_or_else(|| "socket path has no parent".to_owned())?;
    fs::create_dir_all(socket_dir).map_err(|error| error.to_string())?;
    let listener = UnixListener::bind(&options.socket_path).map_err(|error| error.to_string())?;

    let memory_mib = manifest.machine.minimum_memory_bytes / (1024 * 1024);
    let mut qemu_command = Command::new(&options.qemu);
    qemu_command
        .arg("-accel")
        .arg("tcg,thread=single")
        .arg("-machine")
        .arg("pc")
        .arg("-cpu")
        .arg("qemu64")
        .arg("-m")
        .arg(memory_mib.to_string())
        .arg("-smp")
        .arg(manifest.machine.vcpu_count.to_string())
        .arg("-kernel")
        .arg(&loaded.kernel_path)
        .arg("-initrd")
        .arg(&loaded.initramfs_path)
        .arg("-append")
        .arg(&manifest.boot.command_line)
        .arg("-serial")
        .arg("stdio")
        .arg("-serial")
        .arg(format!("unix:{}", options.socket_path.display()))
        .arg("-netdev")
        .arg("user,id=n0,hostfwd=tcp:127.0.0.1:2375-:2375")
        .arg("-device")
        .arg("virtio-net-pci,netdev=n0")
        .arg("-no-reboot")
        .arg("-display")
        .arg("none")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut child = qemu_command
        .spawn()
        .map_err(|error| format!("failed to spawn {}: {error}", options.qemu.display()))?;

    let stream = wait_for_guest_connection(&listener, &options.timeout, &mut child)?;
    let socket_io = SocketIo::new(stream);
    let advances = (options.timeout.as_millis() / ADVANCE_POLL.as_millis()).max(1) as u64;
    let mut client = SessionClient::new(advances).map_err(|error| error.to_string())?;

    let hello = Envelope::new(Message::Hello(Hello::host(
        RequestId::new("rish-guest-boot-1").expect("request id is valid"),
        PeerInfo {
            name: "rish-guest-boot".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            platform: "host".to_owned(),
            architecture: manifest.architecture.cpu.clone(),
        },
        Vec::new(),
        DEFAULT_MAX_FRAME_SIZE as u32,
    )));
    let ack = client
        .bootstrap(&hello, &socket_io)
        .map_err(|error| error.to_string())?;
    println!(
        "handshake: {}",
        match &ack.message {
            Message::HelloAck(ack) => match &ack.outcome {
                rish_guest_protocol::HandshakeOutcome::Accepted { session_id, .. } => {
                    format!("accepted session={session_id}")
                }
                rish_guest_protocol::HandshakeOutcome::Rejected { error } => {
                    format!("rejected: {}", error.message)
                }
            },
            _ => "unexpected frame".to_owned(),
        }
    );

    if !options.skip_evidence {
        let release = run_text(&mut client, &socket_io, &["uname", "-r"])?;
        println!("guest kernel release: {}", release.trim());
        let config = run_text(&mut client, &socket_io, &["zcat", "/proc/config.gz"])?;
        let enabled = config
            .lines()
            .filter(|line| line.ends_with("=y") && line.starts_with("CONFIG_"))
            .count();
        println!("guest /proc/config.gz: {enabled} built-in symbols enabled");
    }

    if options.command.is_empty() {
        println!("no --exec requested; guest keeps running until timeout");
        let _ = child.wait();
        return Ok(0);
    }

    let stdin = match &options.stdin_file {
        Some(path) => fs::read(path).map_err(|error| error.to_string())?,
        None => Vec::new(),
    };
    let outcome = client
        .execute(
            options.command.clone(),
            Default::default(),
            None,
            stdin,
            &socket_io,
        )
        .map_err(|error| error.to_string())?;
    std::io::stdout()
        .write_all(&outcome.stdout)
        .map_err(|error| error.to_string())?;
    std::io::stderr()
        .write_all(&outcome.stderr)
        .map_err(|error| error.to_string())?;
    let _ = child.kill();
    Ok(u8::try_from(outcome.exit_code.unwrap_or(1) & 0xff).unwrap_or(1))
}

fn run_text(client: &mut SessionClient, io: &SocketIo, argv: &[&str]) -> Result<String, String> {
    let outcome = client
        .execute(
            argv.iter().map(|value| (*value).to_owned()).collect(),
            Default::default(),
            None,
            Vec::new(),
            io,
        )
        .map_err(|error| error.to_string())?;
    if outcome.exit_code != Some(0) {
        return Err(format!(
            "{} exited with {:?}: {}",
            argv[0],
            outcome.exit_code,
            String::from_utf8_lossy(&outcome.stderr)
        ));
    }
    String::from_utf8(outcome.stdout).map_err(|error| error.to_string())
}

fn wait_for_guest_connection(
    listener: &UnixListener,
    timeout: &Duration,
    child: &mut Child,
) -> Result<UnixStream, String> {
    listener
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let deadline = std::time::Instant::now() + *timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_read_timeout(Some(ADVANCE_POLL))
                    .map_err(|error| error.to_string())?;
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .map_err(|error| error.to_string())?;
                return Ok(stream);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.to_string()),
        }
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return Err(format!(
                "qemu exited before the guest connected (status {status})"
            ));
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            return Err("timed out waiting for the guest control connection".to_owned());
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// SessionIo adapter over one connected control socket.
struct SocketIo {
    stream: std::sync::Mutex<UnixStream>,
}

impl SocketIo {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream: std::sync::Mutex::new(stream),
        }
    }
}

impl SessionIo for SocketIo {
    fn write(&self, bytes: &[u8]) -> usize {
        let mut stream = self
            .stream
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        stream.write(bytes).unwrap_or_default()
    }

    fn advance(&self) -> Result<Vec<u8>, String> {
        let mut stream = self
            .stream
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut buffer = [0_u8; 65536];
        match stream.read(&mut buffer) {
            Ok(read) => Ok(buffer[..read].to_vec()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(Vec::new())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn dropped_output(&self) -> u64 {
        0
    }
}
