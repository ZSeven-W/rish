use std::collections::BTreeMap;
use std::str::FromStr;

use thiserror::Error;

use crate::HeaderMap;

/// Parameters from an OCI Distribution Bearer authentication challenge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BearerChallenge {
    pub realm: String,
    pub service: Option<String>,
    pub scope: Option<String>,
    pub parameters: BTreeMap<String, String>,
}

impl BearerChallenge {
    pub fn from_headers(headers: &HeaderMap) -> Result<Self, AuthChallengeError> {
        let challenges = headers.get_all("www-authenticate");
        if challenges.is_empty() {
            return Err(AuthChallengeError::MissingBearerChallenge);
        }

        let challenge = challenges.iter().find(|value| {
            value
                .split_ascii_whitespace()
                .next()
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("bearer"))
        });
        challenge.map_or(Err(AuthChallengeError::MissingBearerChallenge), |value| {
            value.parse()
        })
    }
}

impl FromStr for BearerChallenge {
    type Err = AuthChallengeError;

    fn from_str(header: &str) -> Result<Self, Self::Err> {
        if !header.is_ascii()
            || header
                .bytes()
                .any(|byte| byte.is_ascii_control() && byte != b'\t')
        {
            return Err(AuthChallengeError::InvalidSyntax);
        }

        let header = header.trim();
        let Some(scheme_end) = header.find(char::is_whitespace) else {
            return Err(AuthChallengeError::InvalidSyntax);
        };
        if !header[..scheme_end].eq_ignore_ascii_case("bearer") {
            return Err(AuthChallengeError::UnsupportedScheme(
                header[..scheme_end].to_owned(),
            ));
        }

        let mut parser = ParameterParser::new(&header[scheme_end..]);
        let mut parsed = BTreeMap::new();
        while let Some((name, value)) = parser.next_parameter()? {
            let name = name.to_ascii_lowercase();
            if parsed.insert(name.clone(), value).is_some() {
                return Err(AuthChallengeError::DuplicateParameter(name));
            }
        }

        let realm = parsed
            .remove("realm")
            .filter(|value| !value.is_empty())
            .ok_or(AuthChallengeError::MissingRealm)?;
        let service = parsed.remove("service");
        let scope = parsed.remove("scope");

        Ok(Self {
            realm,
            service,
            scope,
            parameters: parsed,
        })
    }
}

struct ParameterParser<'a> {
    source: &'a [u8],
    offset: usize,
    parsed_any: bool,
}

impl<'a> ParameterParser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source: source.as_bytes(),
            offset: 0,
            parsed_any: false,
        }
    }

    fn next_parameter(&mut self) -> Result<Option<(String, String)>, AuthChallengeError> {
        self.skip_whitespace();
        if self.offset == self.source.len() {
            if self.parsed_any {
                return Ok(None);
            }
            return Err(AuthChallengeError::InvalidSyntax);
        }
        if self.parsed_any {
            if self.source[self.offset] != b',' {
                return Err(AuthChallengeError::InvalidSyntax);
            }
            self.offset += 1;
            self.skip_whitespace();
            if self.offset == self.source.len() {
                return Err(AuthChallengeError::InvalidSyntax);
            }
        }

        let name_start = self.offset;
        while self
            .source
            .get(self.offset)
            .is_some_and(|byte| is_token_byte(*byte))
        {
            self.offset += 1;
        }
        if name_start == self.offset {
            return Err(AuthChallengeError::InvalidSyntax);
        }
        let name = std::str::from_utf8(&self.source[name_start..self.offset])
            .map_err(|_| AuthChallengeError::InvalidSyntax)?
            .to_owned();

        self.skip_whitespace();
        if self.source.get(self.offset) != Some(&b'=') {
            return Err(AuthChallengeError::InvalidSyntax);
        }
        self.offset += 1;
        self.skip_whitespace();

        let value = if self.source.get(self.offset) == Some(&b'"') {
            self.quoted_value()?
        } else {
            self.token_value()?
        };
        self.skip_whitespace();
        if self
            .source
            .get(self.offset)
            .is_some_and(|byte| *byte != b',')
        {
            return Err(AuthChallengeError::InvalidSyntax);
        }
        self.parsed_any = true;
        Ok(Some((name, value)))
    }

    fn quoted_value(&mut self) -> Result<String, AuthChallengeError> {
        self.offset += 1;
        let mut output = Vec::new();
        loop {
            let byte = *self
                .source
                .get(self.offset)
                .ok_or(AuthChallengeError::UnterminatedQuote)?;
            self.offset += 1;
            match byte {
                b'"' => break,
                b'\\' => {
                    let escaped = *self
                        .source
                        .get(self.offset)
                        .ok_or(AuthChallengeError::UnterminatedQuote)?;
                    if escaped.is_ascii_control() && escaped != b'\t' {
                        return Err(AuthChallengeError::InvalidSyntax);
                    }
                    output.push(escaped);
                    self.offset += 1;
                }
                byte if byte.is_ascii_control() && byte != b'\t' => {
                    return Err(AuthChallengeError::InvalidSyntax);
                }
                byte => output.push(byte),
            }
        }
        String::from_utf8(output).map_err(|_| AuthChallengeError::InvalidSyntax)
    }

    fn token_value(&mut self) -> Result<String, AuthChallengeError> {
        let start = self.offset;
        while self
            .source
            .get(self.offset)
            .is_some_and(|byte| is_token_byte(*byte))
        {
            self.offset += 1;
        }
        if start == self.offset {
            return Err(AuthChallengeError::InvalidSyntax);
        }
        std::str::from_utf8(&self.source[start..self.offset])
            .map(str::to_owned)
            .map_err(|_| AuthChallengeError::InvalidSyntax)
    }

    fn skip_whitespace(&mut self) {
        while self
            .source
            .get(self.offset)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
        {
            self.offset += 1;
        }
    }
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AuthChallengeError {
    #[error("WWW-Authenticate is not a Bearer challenge: {0}")]
    UnsupportedScheme(String),
    #[error("Bearer challenge has invalid syntax")]
    InvalidSyntax,
    #[error("Bearer challenge has an unterminated quoted value")]
    UnterminatedQuote,
    #[error("Bearer challenge repeats parameter: {0}")]
    DuplicateParameter(String),
    #[error("Bearer challenge does not contain a non-empty realm")]
    MissingRealm,
    #[error("response does not contain a parseable Bearer challenge")]
    MissingBearerChallenge,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_distribution_bearer_challenge() {
        let challenge: BearerChallenge = concat!(
            "Bearer realm=\"https://auth.example/token\",",
            "service=\"registry.example\",",
            "scope=\"repository:team/api:pull\",",
            "error=insufficient_scope"
        )
        .parse()
        .unwrap();

        assert_eq!(challenge.realm, "https://auth.example/token");
        assert_eq!(challenge.service.as_deref(), Some("registry.example"));
        assert_eq!(challenge.scope.as_deref(), Some("repository:team/api:pull"));
        assert_eq!(
            challenge.parameters.get("error").map(String::as_str),
            Some("insufficient_scope")
        );
    }

    #[test]
    fn scheme_and_parameter_names_are_case_insensitive() {
        let challenge: BearerChallenge = "bEaReR ReAlM=\"https://auth\", SERVICE=registry"
            .parse()
            .unwrap();
        assert_eq!(challenge.realm, "https://auth");
        assert_eq!(challenge.service.as_deref(), Some("registry"));
    }

    #[test]
    fn quoted_pairs_are_unescaped() {
        let challenge: BearerChallenge =
            r#"Bearer realm="https://auth/\"tenant\"""#.parse().unwrap();
        assert_eq!(challenge.realm, "https://auth/\"tenant\"");
    }

    #[test]
    fn rejects_missing_realm_duplicates_and_malformed_values() {
        for invalid in [
            "Basic realm=\"https://auth\"",
            "Bearer service=registry",
            "Bearer realm=\"a\",realm=\"b\"",
            "Bearer realm=\"unterminated",
            "Bearer realm=\"a\",",
            "Bearer realm = ",
            "Bearer realm=\"a\"\r\nInjected: true",
        ] {
            assert!(invalid.parse::<BearerChallenge>().is_err(), "{invalid}");
        }
    }

    #[test]
    fn finds_bearer_among_repeated_authentication_headers() {
        let mut headers = HeaderMap::default();
        headers
            .append("www-authenticate", "Basic realm=\"registry\"")
            .unwrap();
        headers
            .append(
                "www-authenticate",
                "Bearer realm=\"https://auth\",service=\"registry\"",
            )
            .unwrap();

        assert_eq!(
            BearerChallenge::from_headers(&headers).unwrap().service,
            Some("registry".to_owned())
        );
    }
}
