use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// A known OCI/Docker media type, or a syntactically valid extension type.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MediaType {
    OctetStream,
    OciImageManifest,
    OciImageIndex,
    OciImageConfig,
    OciImageLayer,
    OciImageLayerGzip,
    OciImageLayerZstd,
    DockerManifest,
    DockerManifestList,
    DockerImageConfig,
    DockerLayerGzip,
    DockerForeignLayerGzip,
    Other(String),
}

impl MediaType {
    pub const OCTET_STREAM: &'static str = "application/octet-stream";
    pub const OCI_IMAGE_MANIFEST: &'static str = "application/vnd.oci.image.manifest.v1+json";
    pub const OCI_IMAGE_INDEX: &'static str = "application/vnd.oci.image.index.v1+json";
    pub const OCI_IMAGE_CONFIG: &'static str = "application/vnd.oci.image.config.v1+json";
    pub const OCI_IMAGE_LAYER: &'static str = "application/vnd.oci.image.layer.v1.tar";
    pub const OCI_IMAGE_LAYER_GZIP: &'static str = "application/vnd.oci.image.layer.v1.tar+gzip";
    pub const OCI_IMAGE_LAYER_ZSTD: &'static str = "application/vnd.oci.image.layer.v1.tar+zstd";
    pub const DOCKER_MANIFEST: &'static str =
        "application/vnd.docker.distribution.manifest.v2+json";
    pub const DOCKER_MANIFEST_LIST: &'static str =
        "application/vnd.docker.distribution.manifest.list.v2+json";
    pub const DOCKER_IMAGE_CONFIG: &'static str = "application/vnd.docker.container.image.v1+json";
    pub const DOCKER_LAYER_GZIP: &'static str = "application/vnd.docker.image.rootfs.diff.tar.gzip";
    pub const DOCKER_FOREIGN_LAYER_GZIP: &'static str =
        "application/vnd.docker.image.rootfs.foreign.diff.tar.gzip";

    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::OctetStream => Self::OCTET_STREAM,
            Self::OciImageManifest => Self::OCI_IMAGE_MANIFEST,
            Self::OciImageIndex => Self::OCI_IMAGE_INDEX,
            Self::OciImageConfig => Self::OCI_IMAGE_CONFIG,
            Self::OciImageLayer => Self::OCI_IMAGE_LAYER,
            Self::OciImageLayerGzip => Self::OCI_IMAGE_LAYER_GZIP,
            Self::OciImageLayerZstd => Self::OCI_IMAGE_LAYER_ZSTD,
            Self::DockerManifest => Self::DOCKER_MANIFEST,
            Self::DockerManifestList => Self::DOCKER_MANIFEST_LIST,
            Self::DockerImageConfig => Self::DOCKER_IMAGE_CONFIG,
            Self::DockerLayerGzip => Self::DOCKER_LAYER_GZIP,
            Self::DockerForeignLayerGzip => Self::DOCKER_FOREIGN_LAYER_GZIP,
            Self::Other(value) => value,
        }
    }

    #[must_use]
    pub fn is_manifest(&self) -> bool {
        matches!(self, Self::OciImageManifest | Self::DockerManifest)
    }

    #[must_use]
    pub fn is_index(&self) -> bool {
        matches!(self, Self::OciImageIndex | Self::DockerManifestList)
    }

    #[must_use]
    pub fn manifest_accept_header() -> String {
        [
            Self::OCI_IMAGE_INDEX,
            Self::DOCKER_MANIFEST_LIST,
            Self::OCI_IMAGE_MANIFEST,
            Self::DOCKER_MANIFEST,
        ]
        .join(", ")
    }
}

impl fmt::Display for MediaType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for MediaType {
    type Err = MediaTypeParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if !valid_media_type(value) {
            return Err(MediaTypeParseError(value.to_owned()));
        }

        Ok(match value {
            Self::OCTET_STREAM => Self::OctetStream,
            Self::OCI_IMAGE_MANIFEST => Self::OciImageManifest,
            Self::OCI_IMAGE_INDEX => Self::OciImageIndex,
            Self::OCI_IMAGE_CONFIG => Self::OciImageConfig,
            Self::OCI_IMAGE_LAYER => Self::OciImageLayer,
            Self::OCI_IMAGE_LAYER_GZIP => Self::OciImageLayerGzip,
            Self::OCI_IMAGE_LAYER_ZSTD => Self::OciImageLayerZstd,
            Self::DOCKER_MANIFEST => Self::DockerManifest,
            Self::DOCKER_MANIFEST_LIST => Self::DockerManifestList,
            Self::DOCKER_IMAGE_CONFIG => Self::DockerImageConfig,
            Self::DOCKER_LAYER_GZIP => Self::DockerLayerGzip,
            Self::DOCKER_FOREIGN_LAYER_GZIP => Self::DockerForeignLayerGzip,
            other => Self::Other(other.to_owned()),
        })
    }
}

impl Serialize for MediaType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for MediaType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

fn valid_media_type(value: &str) -> bool {
    let Some((kind, subtype)) = value.split_once('/') else {
        return false;
    };
    !kind.is_empty()
        && !subtype.is_empty()
        && !subtype.contains('/')
        && kind.bytes().all(is_token_byte)
        && subtype.bytes().all(is_token_byte)
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
#[error("invalid media type: {0}")]
pub struct MediaTypeParseError(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_and_extension_media_types_round_trip() {
        let known: MediaType = MediaType::OCI_IMAGE_MANIFEST.parse().unwrap();
        let extension: MediaType = "application/vnd.example.layer+zstd".parse().unwrap();

        assert_eq!(known, MediaType::OciImageManifest);
        assert_eq!(
            extension,
            MediaType::Other("application/vnd.example.layer+zstd".to_owned())
        );
        assert_eq!(
            serde_json::from_str::<MediaType>(&serde_json::to_string(&known).unwrap()).unwrap(),
            known
        );
    }

    #[test]
    fn parameters_and_control_characters_are_not_descriptor_media_types() {
        for invalid in [
            "",
            "application",
            "application/",
            "/json",
            "application/json; charset=utf-8",
            "application/\njson",
        ] {
            assert!(invalid.parse::<MediaType>().is_err(), "{invalid}");
        }
    }
}
