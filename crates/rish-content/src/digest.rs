use std::fmt;
use std::str::FromStr;

use sha2::{Digest as _, Sha256};

/// A validated OCI `sha256` digest.
///
/// Construction from text accepts upper- or lower-case hexadecimal and
/// canonicalizes it to lower case when formatted.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Sha256Digest([u8; Self::BYTE_LEN]);

impl Sha256Digest {
    pub const ALGORITHM: &'static str = "sha256";
    pub const BYTE_LEN: usize = 32;
    pub const ENCODED_LEN: usize = Self::BYTE_LEN * 2;

    #[must_use]
    pub const fn from_bytes(bytes: [u8; Self::BYTE_LEN]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; Self::BYTE_LEN] {
        &self.0
    }

    #[must_use]
    pub fn calculate(bytes: &[u8]) -> Self {
        let hash: [u8; Self::BYTE_LEN] = Sha256::digest(bytes).into();
        Self(hash)
    }

    /// Parses the hexadecimal portion without an algorithm prefix.
    pub fn from_encoded(encoded: &str) -> Result<Self, DigestParseError> {
        if encoded.len() != Self::ENCODED_LEN {
            return Err(DigestParseError::InvalidLength {
                expected: Self::ENCODED_LEN,
                actual: encoded.len(),
            });
        }

        let mut output = [0_u8; Self::BYTE_LEN];
        let bytes = encoded.as_bytes();
        for (index, pair) in bytes.chunks_exact(2).enumerate() {
            let high = hex_nibble(pair[0], index * 2)?;
            let low = hex_nibble(pair[1], index * 2 + 1)?;
            output[index] = (high << 4) | low;
        }
        Ok(Self(output))
    }

    /// Returns the canonical lower-case hexadecimal portion.
    #[must_use]
    pub fn encoded(&self) -> String {
        let mut output = String::with_capacity(Self::ENCODED_LEN);
        for byte in self.0 {
            use fmt::Write as _;
            write!(output, "{byte:02x}").expect("writing to String cannot fail");
        }
        output
    }
}

fn hex_nibble(byte: u8, index: usize) -> Result<u8, DigestParseError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(DigestParseError::InvalidCharacter {
            index,
            character: char::from(byte),
        }),
    }
}

impl FromStr for Sha256Digest {
    type Err = DigestParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some((algorithm, encoded)) = value.split_once(':') else {
            return Err(DigestParseError::MissingAlgorithm);
        };
        if algorithm != Self::ALGORITHM {
            return Err(DigestParseError::UnsupportedAlgorithm(algorithm.to_owned()));
        }
        Self::from_encoded(encoded)
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", Self::ALGORITHM, self.encoded())
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

/// Error returned when an OCI SHA-256 digest is not canonicalizable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DigestParseError {
    MissingAlgorithm,
    UnsupportedAlgorithm(String),
    InvalidLength { expected: usize, actual: usize },
    InvalidCharacter { index: usize, character: char },
}

impl fmt::Display for DigestParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingAlgorithm => write!(formatter, "digest has no algorithm prefix"),
            Self::UnsupportedAlgorithm(algorithm) => {
                write!(formatter, "unsupported digest algorithm: {algorithm}")
            }
            Self::InvalidLength { expected, actual } => write!(
                formatter,
                "invalid SHA-256 encoded length: expected {expected}, got {actual}"
            ),
            Self::InvalidCharacter { index, character } => write!(
                formatter,
                "invalid hexadecimal character {character:?} at byte {index}"
            ),
        }
    }
}

impl std::error::Error for DigestParseError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_canonicalizes_digest() {
        let upper = "A".repeat(Sha256Digest::ENCODED_LEN);
        let parsed: Sha256Digest = format!("sha256:{upper}").parse().unwrap();

        assert_eq!(parsed.encoded(), "a".repeat(Sha256Digest::ENCODED_LEN));
        assert_eq!(parsed.to_string(), format!("sha256:{}", parsed.encoded()));
    }

    #[test]
    fn rejects_non_hex_and_path_like_values() {
        let path_like = format!("{}../etc", "0".repeat(58));
        assert!(Sha256Digest::from_encoded(&path_like).is_err());
        assert!("sha512:abcd".parse::<Sha256Digest>().is_err());
        assert!("sha256/abcd".parse::<Sha256Digest>().is_err());
    }

    #[test]
    fn calculates_known_digest() {
        let digest = Sha256Digest::calculate(b"abc");
        assert_eq!(
            digest.to_string(),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
