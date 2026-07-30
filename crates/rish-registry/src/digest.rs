use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest as ShaDigest, Sha256, Sha512};
use thiserror::Error;

/// A canonical OCI content digest.
///
/// Parsing is deliberately stricter than the OCI grammar for the algorithms
/// this crate can verify: SHA-256 and SHA-512 encodings must be lower-case hex
/// and exactly the algorithm's output length.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Digest {
    algorithm: String,
    encoded: String,
}

impl Digest {
    pub fn new(
        algorithm: impl Into<String>,
        encoded: impl Into<String>,
    ) -> Result<Self, DigestParseError> {
        format!("{}:{}", algorithm.into(), encoded.into()).parse()
    }

    #[must_use]
    pub fn algorithm(&self) -> &str {
        &self.algorithm
    }

    #[must_use]
    pub fn encoded(&self) -> &str {
        &self.encoded
    }

    #[must_use]
    pub fn sha256(payload: &[u8]) -> Self {
        Self {
            algorithm: "sha256".to_owned(),
            encoded: format!("{:x}", Sha256::digest(payload)),
        }
    }

    #[must_use]
    pub fn sha512(payload: &[u8]) -> Self {
        Self {
            algorithm: "sha512".to_owned(),
            encoded: format!("{:x}", Sha512::digest(payload)),
        }
    }

    pub fn verify(&self, payload: &[u8]) -> Result<(), DigestValidationError> {
        let actual = match self.algorithm.as_str() {
            "sha256" => Self::sha256(payload),
            "sha512" => Self::sha512(payload),
            algorithm => {
                return Err(DigestValidationError::UnsupportedAlgorithm(
                    algorithm.to_owned(),
                ));
            }
        };

        if *self == actual {
            Ok(())
        } else {
            Err(DigestValidationError::Mismatch {
                expected: self.clone(),
                actual,
            })
        }
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.algorithm, self.encoded)
    }
}

impl FromStr for Digest {
    type Err = DigestParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some((algorithm, encoded)) = value.split_once(':') else {
            return Err(DigestParseError::MissingSeparator);
        };
        if algorithm.is_empty() || encoded.is_empty() || encoded.contains(':') {
            return Err(DigestParseError::InvalidFormat);
        }
        if !valid_algorithm(algorithm) {
            return Err(DigestParseError::InvalidAlgorithm(algorithm.to_owned()));
        }

        match algorithm {
            "sha256" if encoded.len() != 64 => {
                return Err(DigestParseError::InvalidLength {
                    algorithm: algorithm.to_owned(),
                    expected: 64,
                    actual: encoded.len(),
                });
            }
            "sha512" if encoded.len() != 128 => {
                return Err(DigestParseError::InvalidLength {
                    algorithm: algorithm.to_owned(),
                    expected: 128,
                    actual: encoded.len(),
                });
            }
            "sha256" | "sha512" if !encoded.bytes().all(is_lower_hex) => {
                return Err(DigestParseError::InvalidEncoding(encoded.to_owned()));
            }
            _ if !encoded.bytes().all(is_digest_byte) => {
                return Err(DigestParseError::InvalidEncoding(encoded.to_owned()));
            }
            _ => {}
        }

        Ok(Self {
            algorithm: algorithm.to_owned(),
            encoded: encoded.to_owned(),
        })
    }
}

impl Serialize for Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

fn valid_algorithm(value: &str) -> bool {
    value.split(['+', '.', '_', '-']).all(|component| {
        !component.is_empty()
            && component
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    })
}

fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

fn is_digest_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'=' | b'_' | b'-')
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum DigestParseError {
    #[error("digest is missing the algorithm separator")]
    MissingSeparator,
    #[error("digest has an invalid format")]
    InvalidFormat,
    #[error("invalid digest algorithm: {0}")]
    InvalidAlgorithm(String),
    #[error("{algorithm} digest must be {expected} bytes of text, got {actual}")]
    InvalidLength {
        algorithm: String,
        expected: usize,
        actual: usize,
    },
    #[error("invalid digest encoding: {0}")]
    InvalidEncoding(String),
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum DigestValidationError {
    #[error("unsupported digest algorithm: {0}")]
    UnsupportedAlgorithm(String),
    #[error("content digest mismatch: expected {expected}, got {actual}")]
    Mismatch { expected: Digest, actual: Digest },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_and_verifies_sha256() {
        let digest = Digest::sha256(b"hello");
        assert_eq!(
            digest.to_string(),
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(digest.verify(b"hello"), Ok(()));
        assert!(matches!(
            digest.verify(b"goodbye"),
            Err(DigestValidationError::Mismatch { .. })
        ));
    }

    #[test]
    fn known_digests_are_canonical_and_exact_length() {
        for invalid in [
            "sha256",
            "SHA256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "sha256:AAAAaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "sha256:abc",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa!",
        ] {
            assert!(invalid.parse::<Digest>().is_err(), "{invalid}");
        }
    }

    #[test]
    fn extension_algorithms_parse_but_fail_closed_during_verification() {
        let digest: Digest = "example-v1:abc_123".parse().unwrap();
        assert_eq!(
            digest.verify(b"payload"),
            Err(DigestValidationError::UnsupportedAlgorithm(
                "example-v1".to_owned()
            ))
        );
    }

    #[test]
    fn digest_has_string_json_representation() {
        let digest = Digest::sha256(b"json");
        let encoded = serde_json::to_string(&digest).unwrap();
        assert_eq!(serde_json::from_str::<Digest>(&encoded).unwrap(), digest);
    }
}
