use std::fmt;

use serde::de::{self, DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};

/// Structural limits checked before OCI JSON is materialized into Rust models.
///
/// The response byte limit alone does not bound allocation: a small JSON
/// document can contain hundreds of thousands of empty strings, arrays, or
/// objects. This preflight walks the serde token stream without retaining the
/// tree, so model deserialization only starts after the shape is known to fit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JsonLimits {
    pub max_depth: usize,
    pub max_total_values: usize,
    pub max_array_items: usize,
    pub max_object_members: usize,
    pub max_string_bytes: usize,
    pub max_total_string_bytes: usize,
}

impl Default for JsonLimits {
    fn default() -> Self {
        Self {
            max_depth: 64,
            max_total_values: 65_536,
            max_array_items: 4_096,
            max_object_members: 4_096,
            max_string_bytes: 1024 * 1024,
            max_total_string_bytes: 8 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JsonPreflightError {
    message: String,
}

impl fmt::Display for JsonPreflightError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for JsonPreflightError {}

pub(crate) fn validate_json_shape(
    bytes: &[u8],
    limits: JsonLimits,
) -> Result<(), JsonPreflightError> {
    validate_limits(limits)?;
    let mut budget = Budget {
        limits,
        depth: 0,
        total_values: 0,
        total_string_bytes: 0,
    };
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    ShapeSeed {
        budget: &mut budget,
    }
    .deserialize(&mut deserializer)
    .map_err(json_error)?;
    deserializer.end().map_err(json_error)
}

fn validate_limits(limits: JsonLimits) -> Result<(), JsonPreflightError> {
    if limits.max_depth == 0
        || limits.max_total_values == 0
        || limits.max_array_items == 0
        || limits.max_object_members == 0
        || limits.max_string_bytes == 0
        || limits.max_total_string_bytes == 0
    {
        return Err(limit_error("all JSON structural limits must be non-zero"));
    }
    Ok(())
}

fn json_error(error: serde_json::Error) -> JsonPreflightError {
    JsonPreflightError {
        message: error.to_string(),
    }
}

fn limit_error(message: impl Into<String>) -> JsonPreflightError {
    JsonPreflightError {
        message: message.into(),
    }
}

struct Budget {
    limits: JsonLimits,
    depth: usize,
    total_values: usize,
    total_string_bytes: usize,
}

impl Budget {
    fn observe_value<E: de::Error>(&mut self) -> Result<(), E> {
        self.total_values = self.total_values.saturating_add(1);
        if self.total_values > self.limits.max_total_values {
            return Err(E::custom(format!(
                "JSON value count exceeds limit {}",
                self.limits.max_total_values
            )));
        }
        Ok(())
    }

    fn observe_string<E: de::Error>(&mut self, value: &str) -> Result<(), E> {
        if value.len() > self.limits.max_string_bytes {
            return Err(E::custom(format!(
                "JSON string length {} exceeds limit {}",
                value.len(),
                self.limits.max_string_bytes
            )));
        }
        self.total_string_bytes = self.total_string_bytes.saturating_add(value.len());
        if self.total_string_bytes > self.limits.max_total_string_bytes {
            return Err(E::custom(format!(
                "cumulative JSON string bytes exceed limit {}",
                self.limits.max_total_string_bytes
            )));
        }
        Ok(())
    }

    fn enter_container<E: de::Error>(&mut self) -> Result<(), E> {
        self.depth = self.depth.saturating_add(1);
        if self.depth > self.limits.max_depth {
            self.depth = self.depth.saturating_sub(1);
            return Err(E::custom(format!(
                "JSON nesting depth exceeds limit {}",
                self.limits.max_depth
            )));
        }
        Ok(())
    }

    fn leave_container(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }
}

struct ShapeSeed<'a> {
    budget: &'a mut Budget,
}

impl<'de> DeserializeSeed<'de> for ShapeSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        self.budget.observe_value()?;
        deserializer.deserialize_any(ShapeVisitor {
            budget: self.budget,
        })
    }
}

struct ShapeVisitor<'a> {
    budget: &'a mut Budget,
}

impl<'de> Visitor<'de> for ShapeVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value within the configured structural limits")
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        ShapeSeed {
            budget: self.budget,
        }
        .deserialize(deserializer)
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.budget.observe_string(value)
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.budget.observe_string(value)
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.budget.observe_string(&value)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        self.budget.enter_container()?;
        let result = (|| {
            let mut items = 0_usize;
            while sequence
                .next_element_seed(ShapeSeed {
                    budget: self.budget,
                })?
                .is_some()
            {
                items = items.saturating_add(1);
                if items > self.budget.limits.max_array_items {
                    return Err(A::Error::custom(format!(
                        "JSON array item count exceeds limit {}",
                        self.budget.limits.max_array_items
                    )));
                }
            }
            Ok(())
        })();
        self.budget.leave_container();
        result
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        self.budget.enter_container()?;
        let result = (|| {
            let mut members = 0_usize;
            while map
                .next_key_seed(StringSeed {
                    budget: self.budget,
                })?
                .is_some()
            {
                members = members.saturating_add(1);
                if members > self.budget.limits.max_object_members {
                    return Err(A::Error::custom(format!(
                        "JSON object member count exceeds limit {}",
                        self.budget.limits.max_object_members
                    )));
                }
                map.next_value_seed(ShapeSeed {
                    budget: self.budget,
                })?;
            }
            Ok(())
        })();
        self.budget.leave_container();
        result
    }
}

struct StringSeed<'a> {
    budget: &'a mut Budget,
}

impl<'de> DeserializeSeed<'de> for StringSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_string(StringVisitor {
            budget: self.budget,
        })
    }
}

struct StringVisitor<'a> {
    budget: &'a mut Budget,
}

impl<'de> Visitor<'de> for StringVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object key")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.budget.observe_string(value)
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.budget.observe_string(value)
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.budget.observe_string(&value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tight_limits() -> JsonLimits {
        JsonLimits {
            max_depth: 4,
            max_total_values: 8,
            max_array_items: 3,
            max_object_members: 3,
            max_string_bytes: 8,
            max_total_string_bytes: 16,
        }
    }

    #[test]
    fn accepts_normal_nested_json_and_unicode_escapes() {
        validate_json_shape(br#"{"a":[1,true,"\u4f60\u597d"],"b":null}"#, tight_limits()).unwrap();
    }

    #[test]
    fn rejects_each_structural_amplification_axis() {
        let limits = tight_limits();
        assert!(validate_json_shape(br#"[[[[[]]]]]"#, limits).is_err());
        assert!(validate_json_shape(br#"[0,1,2,3]"#, limits).is_err());
        assert!(validate_json_shape(br#"{"a":0,"b":1,"c":2,"d":3}"#, limits).is_err());
        assert!(validate_json_shape(br#""123456789""#, limits).is_err());
        assert!(validate_json_shape(br#"["12345678","12345678","x"]"#, limits).is_err());
        assert!(
            validate_json_shape(
                br#"[0,1,2]"#,
                JsonLimits {
                    max_total_values: 3,
                    ..limits
                }
            )
            .is_err()
        );
    }

    #[test]
    fn malformed_json_and_zero_limits_fail_closed() {
        assert!(validate_json_shape(br#"{"unfinished":"#, tight_limits()).is_err());
        assert!(
            validate_json_shape(
                b"null",
                JsonLimits {
                    max_depth: 0,
                    ..tight_limits()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn default_preflight_stops_a_large_tiny_value_array() {
        let amplified = format!("[{}]", vec!["null"; 100_000].join(","));
        let error = validate_json_shape(amplified.as_bytes(), JsonLimits::default()).unwrap_err();

        assert!(error.to_string().contains("array item count"));
    }
}
