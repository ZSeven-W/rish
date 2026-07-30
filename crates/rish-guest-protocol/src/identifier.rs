use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::IdentifierError;

/// Caller-generated identifier used to correlate requests and responses.
///
/// IDs are strings instead of JSON numbers so Swift, ArkTS, Java, and Rust can
/// exchange them without integer precision differences.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RequestId(String);

impl RequestId {
    pub const MAX_LEN: usize = 128;

    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        if value.is_empty() {
            return Err(IdentifierError::Empty);
        }
        if value.len() > Self::MAX_LEN {
            return Err(IdentifierError::TooLong {
                length: value.len(),
                max: Self::MAX_LEN,
            });
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
        {
            return Err(IdentifierError::InvalidCharacter);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for RequestId {
    type Error = IdentifierError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<RequestId> for String {
    fn from(value: RequestId) -> Self {
        value.0
    }
}

impl Serialize for RequestId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RequestId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_json_strings() {
        let id = RequestId::new("host:42").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"host:42\"");
        assert_eq!(serde_json::from_str::<RequestId>(&json).unwrap(), id);
    }

    #[test]
    fn invalid_request_id_is_rejected_during_deserialization() {
        let error = serde_json::from_str::<RequestId>("\"contains space\"").unwrap_err();
        assert!(error.to_string().contains("invalid character"));
    }
}
