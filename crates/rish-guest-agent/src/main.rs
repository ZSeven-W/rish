use std::io::{self, Read as _, Write as _};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::Duration;

use rish_guest_agent::{GuestAgent, NativeOperationHandler, bootstrap_agent};
use rish_guest_protocol::{Envelope, FrameDecoder, FrameEncoder};

const INPUT_CHUNK_SIZE: usize = 64 * 1024;
const INPUT_QUEUE_CAPACITY: usize = 2;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rish-guest-agent: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let input = spawn_input_reader()?;
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut decoder = FrameDecoder::default();
    let mut encoder = FrameEncoder::default();
    let mut agent = bootstrap_agent();

    loop {
        let mut wrote_output = false;
        match input.recv_timeout(PROCESS_POLL_INTERVAL) {
            Ok(InputMessage::Bytes(bytes)) => {
                wrote_output |=
                    process_input(&bytes, &mut decoder, &mut encoder, &mut agent, &mut output)?;
            }
            Ok(InputMessage::Eof) => {
                if decoder.buffered_len() != 0 {
                    return Err("control stream ended in the middle of a frame".into());
                }
                return Ok(());
            }
            Ok(InputMessage::Error(error)) => return Err(error.into()),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err("control input reader stopped unexpectedly".into());
            }
        }

        wrote_output |= write_envelopes(&mut output, &encoder, agent.poll())?;
        if wrote_output {
            output.flush()?;
        }
    }
}

fn spawn_input_reader() -> io::Result<Receiver<InputMessage>> {
    let (sender, receiver) = mpsc::sync_channel(INPUT_QUEUE_CAPACITY);
    thread::Builder::new()
        .name("rish-control-input".to_owned())
        .spawn(move || read_control_input(sender))?;
    Ok(receiver)
}

fn read_control_input(sender: SyncSender<InputMessage>) {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    loop {
        let mut bytes = vec![0_u8; INPUT_CHUNK_SIZE];
        match input.read(&mut bytes) {
            Ok(0) => {
                let _ = sender.send(InputMessage::Eof);
                return;
            }
            Ok(read) => {
                bytes.truncate(read);
                if sender.send(InputMessage::Bytes(bytes)).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = sender.send(InputMessage::Error(error));
                return;
            }
        }
    }
}

fn process_input(
    bytes: &[u8],
    decoder: &mut FrameDecoder,
    encoder: &mut FrameEncoder,
    agent: &mut GuestAgent<NativeOperationHandler>,
    output: &mut impl io::Write,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut offset = 0;
    let mut wrote_output = false;
    while offset < bytes.len() {
        if decoder.remaining_buffer_capacity() == 0 {
            let drained = drain_frames(decoder, encoder, agent, output)?;
            wrote_output |= drained;
            if !drained && decoder.remaining_buffer_capacity() == 0 {
                return Err("guest protocol decoder has no remaining buffer capacity".into());
            }
        }
        let accepted = decoder
            .remaining_buffer_capacity()
            .min(bytes.len() - offset);
        decoder.push(&bytes[offset..offset + accepted])?;
        offset += accepted;
        wrote_output |= drain_frames(decoder, encoder, agent, output)?;
    }
    Ok(wrote_output)
}

fn drain_frames(
    decoder: &mut FrameDecoder,
    encoder: &mut FrameEncoder,
    agent: &mut GuestAgent<NativeOperationHandler>,
    output: &mut impl io::Write,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut wrote_output = false;
    while let Some(envelope) = decoder.next_frame()? {
        wrote_output |= write_envelopes(output, encoder, agent.handle(envelope)?)?;
        apply_negotiated_limits(decoder, encoder, agent)?;
    }
    Ok(wrote_output)
}

fn write_envelopes(
    output: &mut impl io::Write,
    encoder: &FrameEncoder,
    envelopes: impl IntoIterator<Item = Envelope>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut wrote_output = false;
    for envelope in envelopes {
        output.write_all(&encoder.encode(&envelope)?)?;
        wrote_output = true;
    }
    Ok(wrote_output)
}

fn apply_negotiated_limits(
    decoder: &mut FrameDecoder,
    encoder: &mut FrameEncoder,
    agent: &GuestAgent<NativeOperationHandler>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(version) = agent.negotiated_version() {
        decoder.set_expected_version(Some(version));
    }
    if let Some(max_frame_size) = agent.negotiated_max_frame_size() {
        let max_frame_size = max_frame_size as usize;
        decoder.set_max_frame_size(max_frame_size)?;
        encoder.set_max_frame_size(max_frame_size)?;
    }
    Ok(())
}

enum InputMessage {
    Bytes(Vec<u8>),
    Eof,
    Error(io::Error),
}
