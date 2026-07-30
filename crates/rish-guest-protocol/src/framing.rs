use crate::{DEFAULT_MAX_FRAME_SIZE, Envelope, FrameError, PROTOCOL_ID, ProtocolVersion};

const PREFIX_SIZE: usize = size_of::<u32>();
const BUFFERED_FRAME_BUDGET: usize = 2;

#[derive(Clone, Debug)]
pub struct FrameEncoder {
    max_frame_size: usize,
}

impl FrameEncoder {
    pub fn new(max_frame_size: usize) -> Result<Self, FrameError> {
        validate_maximum(max_frame_size)?;
        Ok(Self { max_frame_size })
    }

    #[must_use]
    pub fn max_frame_size(&self) -> usize {
        self.max_frame_size
    }

    pub fn set_max_frame_size(&mut self, max_frame_size: usize) -> Result<(), FrameError> {
        validate_maximum(max_frame_size)?;
        self.max_frame_size = max_frame_size;
        Ok(())
    }

    pub fn encode(&self, envelope: &Envelope) -> Result<Vec<u8>, FrameError> {
        if envelope.protocol != PROTOCOL_ID {
            return Err(FrameError::ProtocolMismatch {
                expected: PROTOCOL_ID,
                received: envelope.protocol.clone(),
            });
        }

        let payload = serde_json::to_vec(envelope).map_err(FrameError::Serialization)?;
        if payload.len() > self.max_frame_size {
            return Err(FrameError::FrameTooLarge {
                size: payload.len(),
                max: self.max_frame_size,
            });
        }

        let payload_len = u32::try_from(payload.len()).map_err(|_| FrameError::FrameTooLarge {
            size: payload.len(),
            max: self.max_frame_size,
        })?;
        let mut frame = Vec::with_capacity(PREFIX_SIZE + payload.len());
        frame.extend_from_slice(&payload_len.to_be_bytes());
        frame.extend_from_slice(&payload);
        Ok(frame)
    }
}

impl Default for FrameEncoder {
    fn default() -> Self {
        Self {
            max_frame_size: DEFAULT_MAX_FRAME_SIZE,
        }
    }
}

/// Incremental decoder suitable for arbitrary stream read boundaries.
///
/// A malformed, oversized, or version-incompatible frame poisons the decoder.
/// A transport should normally close the session at that point. Tests or a
/// reconnect loop may call [`Self::reset`] before feeding a new byte stream.
#[derive(Debug)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
    read_position: usize,
    initial_max_frame_size: usize,
    max_frame_size: usize,
    max_buffer_size: usize,
    expected_version: Option<ProtocolVersion>,
    poisoned: bool,
}

impl FrameDecoder {
    pub fn new(max_frame_size: usize) -> Result<Self, FrameError> {
        validate_maximum(max_frame_size)?;
        Ok(Self {
            buffer: Vec::new(),
            read_position: 0,
            initial_max_frame_size: max_frame_size,
            max_frame_size,
            max_buffer_size: buffer_limit(max_frame_size),
            expected_version: None,
            poisoned: false,
        })
    }

    pub fn with_expected_version(
        max_frame_size: usize,
        expected_version: ProtocolVersion,
    ) -> Result<Self, FrameError> {
        let mut decoder = Self::new(max_frame_size)?;
        decoder.expected_version = Some(expected_version);
        Ok(decoder)
    }

    /// Changes post-handshake version validation for subsequently decoded frames.
    pub fn set_expected_version(&mut self, expected_version: Option<ProtocolVersion>) {
        self.expected_version = expected_version;
    }

    #[must_use]
    pub fn max_frame_size(&self) -> usize {
        self.max_frame_size
    }

    /// Applies a negotiated limit without discarding already-buffered bytes.
    ///
    /// Any buffered next frame is checked against the new limit when decoded.
    pub fn set_max_frame_size(&mut self, max_frame_size: usize) -> Result<(), FrameError> {
        validate_maximum(max_frame_size)?;
        let max_buffer_size = buffer_limit(max_frame_size);
        if self.buffered_len() > max_buffer_size {
            return Err(self.poison(FrameError::BufferTooLarge {
                size: self.buffered_len(),
                max: max_buffer_size,
            }));
        }
        self.max_frame_size = max_frame_size;
        self.max_buffer_size = max_buffer_size;
        Ok(())
    }

    #[must_use]
    pub fn max_buffer_size(&self) -> usize {
        self.max_buffer_size
    }

    #[must_use]
    pub fn remaining_buffer_capacity(&self) -> usize {
        self.max_buffer_size.saturating_sub(self.buffered_len())
    }

    #[must_use]
    pub fn buffered_len(&self) -> usize {
        self.buffer.len().saturating_sub(self.read_position)
    }

    #[must_use]
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<(), FrameError> {
        if self.poisoned {
            return Err(FrameError::DecoderPoisoned);
        }
        self.compact_if_useful();
        let buffered_size = self.buffered_len().saturating_add(bytes.len());
        if buffered_size > self.max_buffer_size {
            return Err(self.poison(FrameError::BufferTooLarge {
                size: buffered_size,
                max: self.max_buffer_size,
            }));
        }
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    /// Decodes one complete frame, or returns `None` when more bytes are needed.
    ///
    /// Call repeatedly after each [`Self::push`] to consume multiple frames
    /// delivered by one transport read.
    pub fn next_frame(&mut self) -> Result<Option<Envelope>, FrameError> {
        if self.poisoned {
            return Err(FrameError::DecoderPoisoned);
        }
        if self.buffered_len() < PREFIX_SIZE {
            return Ok(None);
        }

        let header_start = self.read_position;
        let payload_size = u32::from_be_bytes(
            self.buffer[header_start..header_start + PREFIX_SIZE]
                .try_into()
                .expect("four-byte length prefix"),
        ) as usize;

        if payload_size > self.max_frame_size {
            return Err(self.poison(FrameError::FrameTooLarge {
                size: payload_size,
                max: self.max_frame_size,
            }));
        }

        let frame_size = PREFIX_SIZE + payload_size;
        if self.buffered_len() < frame_size {
            return Ok(None);
        }

        let payload_start = header_start + PREFIX_SIZE;
        let payload_end = payload_start + payload_size;
        let decoded = serde_json::from_slice::<Envelope>(&self.buffer[payload_start..payload_end])
            .map_err(FrameError::Deserialization);
        self.read_position = payload_end;

        let envelope = match decoded {
            Ok(envelope) => envelope,
            Err(error) => return Err(self.poison(error)),
        };

        if envelope.protocol != PROTOCOL_ID {
            let received = envelope.protocol;
            return Err(self.poison(FrameError::ProtocolMismatch {
                expected: PROTOCOL_ID,
                received,
            }));
        }

        if let Some(expected) = self.expected_version {
            if envelope.version != expected {
                return Err(self.poison(FrameError::VersionMismatch {
                    expected,
                    received: envelope.version,
                }));
            }
        }

        self.compact_if_useful();
        Ok(Some(envelope))
    }

    /// Clears session state and permits reuse for a new bootstrap handshake.
    ///
    /// This restores the constructor's frame limit and removes any negotiated
    /// expected version. It never carries policy from one session to another.
    pub fn reset(&mut self) {
        self.buffer = Vec::new();
        self.read_position = 0;
        self.max_frame_size = self.initial_max_frame_size;
        self.max_buffer_size = buffer_limit(self.initial_max_frame_size);
        self.expected_version = None;
        self.poisoned = false;
    }

    fn poison(&mut self, error: FrameError) -> FrameError {
        self.buffer = Vec::new();
        self.read_position = 0;
        self.poisoned = true;
        error
    }

    fn compact_if_useful(&mut self) {
        if self.read_position == self.buffer.len() {
            self.buffer.clear();
            self.read_position = 0;
        } else if self.read_position >= 4096 && self.read_position * 2 >= self.buffer.len() {
            self.buffer.drain(..self.read_position);
            self.read_position = 0;
        }
    }
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self {
            buffer: Vec::new(),
            read_position: 0,
            initial_max_frame_size: DEFAULT_MAX_FRAME_SIZE,
            max_frame_size: DEFAULT_MAX_FRAME_SIZE,
            max_buffer_size: buffer_limit(DEFAULT_MAX_FRAME_SIZE),
            expected_version: None,
            poisoned: false,
        }
    }
}

fn validate_maximum(max_frame_size: usize) -> Result<(), FrameError> {
    if max_frame_size == 0
        || u32::try_from(max_frame_size).is_err()
        || max_frame_size.checked_add(PREFIX_SIZE).is_none()
    {
        return Err(FrameError::InvalidMaximum(max_frame_size));
    }
    Ok(())
}

fn buffer_limit(max_frame_size: usize) -> usize {
    let one_frame = max_frame_size + PREFIX_SIZE;
    one_frame
        .checked_mul(BUFFERED_FRAME_BUDGET)
        .unwrap_or(one_frame)
}

#[cfg(test)]
mod tests {
    use crate::{CURRENT_PROTOCOL_VERSION, Event, EventKind, Message, ProtocolVersion, RequestId};

    use super::*;

    fn heartbeat(sequence: u64) -> Envelope {
        Envelope::new(Message::Event(Event {
            sequence,
            timestamp_ms: 123,
            request_id: Some(RequestId::new(format!("request-{sequence}")).unwrap()),
            event: EventKind::Heartbeat,
        }))
    }

    #[test]
    fn decoder_handles_every_possible_fragment_boundary() {
        let expected = heartbeat(1);
        let frame = FrameEncoder::default().encode(&expected).unwrap();

        for split in 0..=frame.len() {
            let mut decoder = FrameDecoder::default();
            decoder.push(&frame[..split]).unwrap();
            let first = decoder.next_frame().unwrap();
            let decoded = if split < frame.len() {
                assert!(first.is_none(), "split {split} produced a frame too early");
                decoder.push(&frame[split..]).unwrap();
                decoder.next_frame().unwrap()
            } else {
                first
            };
            assert_eq!(decoded, Some(expected.clone()));
            assert_eq!(decoder.next_frame().unwrap(), None);
        }
    }

    #[test]
    fn decoder_handles_single_byte_fragments() {
        let expected = heartbeat(2);
        let frame = FrameEncoder::default().encode(&expected).unwrap();
        let mut decoder = FrameDecoder::default();

        for byte in frame {
            decoder.push(&[byte]).unwrap();
        }

        assert_eq!(decoder.next_frame().unwrap(), Some(expected));
    }

    #[test]
    fn decoder_returns_multiple_coalesced_frames() {
        let first = heartbeat(1);
        let second = heartbeat(2);
        let encoder = FrameEncoder::default();
        let mut bytes = encoder.encode(&first).unwrap();
        bytes.extend_from_slice(&encoder.encode(&second).unwrap());

        let mut decoder = FrameDecoder::default();
        decoder.push(&bytes).unwrap();
        assert_eq!(decoder.next_frame().unwrap(), Some(first));
        assert_eq!(decoder.next_frame().unwrap(), Some(second));
        assert_eq!(decoder.next_frame().unwrap(), None);
    }

    #[test]
    fn oversized_header_is_rejected_before_payload_arrives() {
        let mut decoder = FrameDecoder::new(64).unwrap();
        decoder.push(&65_u32.to_be_bytes()).unwrap();

        assert!(matches!(
            decoder.next_frame(),
            Err(FrameError::FrameTooLarge { size: 65, max: 64 })
        ));
        assert!(decoder.is_poisoned());
        assert!(matches!(
            decoder.push(b"new stream"),
            Err(FrameError::DecoderPoisoned)
        ));
    }

    #[test]
    fn encoder_applies_the_same_size_limit() {
        let error = FrameEncoder::new(4)
            .unwrap()
            .encode(&heartbeat(1))
            .unwrap_err();
        assert!(matches!(
            error,
            FrameError::FrameTooLarge { size, max: 4 } if size > 4
        ));
    }

    #[test]
    fn decoder_rejects_post_handshake_version_mismatch() {
        let received = ProtocolVersion::new(2, 0);
        let envelope = Envelope::with_version(
            received,
            Message::Event(Event {
                sequence: 1,
                timestamp_ms: 123,
                request_id: None,
                event: EventKind::Heartbeat,
            }),
        );
        let bytes = FrameEncoder::default().encode(&envelope).unwrap();
        let mut decoder =
            FrameDecoder::with_expected_version(DEFAULT_MAX_FRAME_SIZE, CURRENT_PROTOCOL_VERSION)
                .unwrap();
        decoder.push(&bytes).unwrap();

        assert!(matches!(
            decoder.next_frame(),
            Err(FrameError::VersionMismatch {
                expected: CURRENT_PROTOCOL_VERSION,
                received
            }) if received == ProtocolVersion::new(2, 0)
        ));
    }

    #[test]
    fn decoder_can_be_reset_after_protocol_failure() {
        let mut decoder = FrameDecoder::new(8).unwrap();
        decoder.set_expected_version(Some(CURRENT_PROTOCOL_VERSION));
        decoder.push(&9_u32.to_be_bytes()).unwrap();
        assert!(decoder.next_frame().is_err());
        decoder.set_max_frame_size(4).unwrap();
        decoder.reset();
        assert!(!decoder.is_poisoned());
        assert_eq!(decoder.buffered_len(), 0);
        assert_eq!(decoder.max_frame_size(), 8);
        assert_eq!(decoder.expected_version, None);
    }

    #[test]
    fn negotiated_limit_applies_to_an_already_buffered_next_frame() {
        let first = heartbeat(1);
        let second = heartbeat(2);
        let encoder = FrameEncoder::default();
        let first_bytes = encoder.encode(&first).unwrap();
        let second_bytes = encoder.encode(&second).unwrap();
        let second_payload_size = second_bytes.len() - PREFIX_SIZE;
        let mut bytes = first_bytes;
        bytes.extend_from_slice(&second_bytes);

        let mut decoder = FrameDecoder::default();
        decoder.push(&bytes).unwrap();
        assert_eq!(decoder.next_frame().unwrap(), Some(first));
        decoder.set_max_frame_size(second_payload_size - 1).unwrap();
        assert!(matches!(
            decoder.next_frame(),
            Err(FrameError::FrameTooLarge { .. })
        ));
    }

    #[test]
    fn decoder_rejects_unbounded_coalesced_input_and_releases_it() {
        let mut decoder = FrameDecoder::new(64).unwrap();
        let attempted = decoder.max_buffer_size() + 1;
        let bytes = vec![0_u8; attempted];

        assert!(matches!(
            decoder.push(&bytes),
            Err(FrameError::BufferTooLarge { size, max })
                if size == attempted && max + 1 == attempted
        ));
        assert!(decoder.is_poisoned());
        assert_eq!(decoder.buffered_len(), 0);
        assert_eq!(decoder.buffer.capacity(), 0);

        decoder.reset();
        assert_eq!(decoder.buffer.capacity(), 0);
    }
}
