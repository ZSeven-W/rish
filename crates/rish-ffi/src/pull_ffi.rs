use std::ffi::{CString, c_char, c_void};
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rish_content::{ContentStore, StoreConfig};
use rish_pull::{
    DEFAULT_MAX_CONFIG_BYTES, DEFAULT_MAX_LAYERS, DEFAULT_MAX_MANIFEST_BYTES, GuestPlatform,
    PullPolicy, PulledImage, Puller, VerifiedImageRecordStore,
};
use rish_registry::{
    HeaderMap, RegistryRequest, RegistryStreamResponse, RegistryTransport, TransportError,
};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

const PROTOCOL_VERSION: u32 = 1;
const MAX_PULL_REQUEST_BYTES: usize = 64 * 1024;
const MAX_FETCH_REQUEST_BYTES: usize = 256 * 1024;
const MAX_FETCH_METADATA_BYTES: usize = 256 * 1024;
const MAX_FETCH_ERROR_BYTES: usize = 4 * 1024;
const DEFAULT_FFI_MAX_LAYER_BYTES: u64 = 128 * 1024 * 1024;
const DEFAULT_FFI_MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

static PULL_LOCK: Mutex<()> = Mutex::new(());

/// Synchronous trusted-host HTTP callback.
///
/// The request and metadata pointers are borrowed for the duration of the
/// callback. `body_fd` is a borrowed writable descriptor and must not be
/// closed or retained. A callback must stream the response body into it and
/// write a versioned [`FetchResponseMetadata`] JSON object into `metadata`.
pub type RishRegistryFetchCallback = unsafe extern "C" fn(
    context: *mut c_void,
    request: *const u8,
    request_len: usize,
    body_fd: i32,
    metadata: *mut u8,
    metadata_capacity: usize,
    metadata_len: *mut usize,
) -> i32;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PullImageRequest {
    protocol_version: u32,
    reference: String,
    store_root: String,
    #[serde(default)]
    platform: GuestPlatform,
    #[serde(default)]
    limits: PullLimitRequest,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PullLimitRequest {
    max_manifest_bytes: Option<u64>,
    max_config_bytes: Option<u64>,
    max_layers: Option<usize>,
    max_layer_bytes: Option<u64>,
    max_total_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
struct PullImageResponse {
    protocol_version: u32,
    ok: bool,
    receipt: Option<PullReceipt>,
    error: Option<String>,
}

impl PullImageResponse {
    fn success(receipt: PullReceipt) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            ok: true,
            receipt: Some(receipt),
            error: None,
        }
    }

    fn failure(error: impl Into<String>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            ok: false,
            receipt: None,
            error: Some(error.into()),
        }
    }
}

#[derive(Debug, Serialize)]
struct PullReceipt {
    normalized_reference: String,
    resolved_digest: String,
    index_digest: Option<String>,
    manifest_digest: String,
    config_digest: String,
    os: String,
    architecture: String,
    variant: Option<String>,
    layers: Vec<LayerReceipt>,
    total_verified_bytes: u64,
    pin: String,
    content_store: &'static str,
}

#[derive(Debug, Serialize)]
struct LayerReceipt {
    digest: String,
    size: u64,
    media_type: String,
}

#[derive(Serialize)]
struct FetchRequestEnvelope<'request> {
    protocol_version: u32,
    request: &'request RegistryRequest,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FetchResponseMetadata {
    protocol_version: u32,
    ok: bool,
    status: Option<u16>,
    #[serde(default)]
    headers: HeaderMap,
    error: Option<String>,
    #[serde(default)]
    retryable: bool,
}

struct CallbackTransport {
    callback: RishRegistryFetchCallback,
    context: *mut c_void,
    temporary_directory: PathBuf,
}

impl RegistryTransport for CallbackTransport {
    type Body = BoundedTemporaryBody;

    fn execute(
        &self,
        request: &RegistryRequest,
    ) -> Result<RegistryStreamResponse<Self::Body>, TransportError> {
        let encoded = serde_json::to_vec(&FetchRequestEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request,
        })
        .map_err(|error| transport_error(format!("could not encode fetch request: {error}")))?;
        if encoded.len() > MAX_FETCH_REQUEST_BYTES {
            return Err(transport_error("fetch request exceeds bridge limit"));
        }

        let mut temporary = tempfile::Builder::new()
            .prefix(".registry-response-")
            .tempfile_in(&self.temporary_directory)
            .map_err(|error| transport_error(format!("could not create response file: {error}")))?;
        let mut metadata = vec![0_u8; MAX_FETCH_METADATA_BYTES];
        let mut metadata_len = 0_usize;
        // SAFETY: Every pointer remains valid for the callback duration. The
        // file descriptor is borrowed from `temporary`, which remains alive.
        let callback_status = unsafe {
            (self.callback)(
                self.context,
                encoded.as_ptr(),
                encoded.len(),
                temporary.as_file().as_raw_fd(),
                metadata.as_mut_ptr(),
                metadata.len(),
                &mut metadata_len,
            )
        };

        if metadata_len > metadata.len() {
            return Err(transport_error(format!(
                "fetch metadata exceeds bridge limit: maximum {}, got at least {}",
                metadata.len(),
                metadata_len
            )));
        }
        let decoded = decode_fetch_metadata(&metadata[..metadata_len]);
        if callback_status != 0 {
            let (message, retryable) = decoded.map_or_else(
                |_| {
                    (
                        format!("host fetch callback returned status {callback_status}"),
                        false,
                    )
                },
                |value| {
                    (
                        value.error.unwrap_or_else(|| {
                            format!("host fetch callback returned status {callback_status}")
                        }),
                        value.retryable,
                    )
                },
            );
            return Err(TransportError::new(message, retryable));
        }

        let metadata = decoded?;
        if !metadata.ok {
            return Err(TransportError::new(
                metadata
                    .error
                    .unwrap_or_else(|| "host fetch failed without an error message".to_owned()),
                metadata.retryable,
            ));
        }
        if metadata.error.is_some() {
            return Err(transport_error(
                "successful fetch metadata must not contain an error",
            ));
        }
        let status = metadata
            .status
            .ok_or_else(|| transport_error("successful fetch metadata is missing status"))?;
        reject_oversized_content_length(&metadata.headers, request.max_response_bytes)?;

        let file_size = temporary
            .as_file()
            .metadata()
            .map_err(|error| transport_error(format!("could not inspect response file: {error}")))?
            .len();
        if file_size > request.max_response_bytes {
            return Err(transport_error(format!(
                "response body exceeds {} byte request limit (received at least {file_size})",
                request.max_response_bytes
            )));
        }
        temporary
            .as_file_mut()
            .seek(SeekFrom::Start(0))
            .map_err(|error| transport_error(format!("could not rewind response body: {error}")))?;

        Ok(RegistryStreamResponse {
            status,
            headers: metadata.headers,
            body: BoundedTemporaryBody {
                temporary,
                maximum: request.max_response_bytes,
                consumed: 0,
            },
        })
    }
}

struct BoundedTemporaryBody {
    temporary: NamedTempFile,
    maximum: u64,
    consumed: u64,
}

impl Read for BoundedTemporaryBody {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let remaining = self.maximum.saturating_sub(self.consumed);
        if remaining == 0 {
            let mut trailing = [0_u8; 1];
            return match self.temporary.as_file_mut().read(&mut trailing)? {
                0 => Ok(0),
                _ => Err(std::io::Error::other(
                    "registry response exceeded its incremental byte limit",
                )),
            };
        }
        let allowed = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let count = self.temporary.as_file_mut().read(&mut buffer[..allowed])?;
        self.consumed = self
            .consumed
            .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        Ok(count)
    }
}

fn decode_fetch_metadata(bytes: &[u8]) -> Result<FetchResponseMetadata, TransportError> {
    if bytes.is_empty() {
        return Err(transport_error("host fetch returned empty metadata"));
    }
    let encoded = std::str::from_utf8(bytes)
        .map_err(|error| transport_error(format!("fetch metadata is not UTF-8: {error}")))?;
    let metadata = serde_json::from_str::<FetchResponseMetadata>(encoded)
        .map_err(|error| transport_error(format!("invalid fetch metadata JSON: {error}")))?;
    if metadata.protocol_version != PROTOCOL_VERSION {
        return Err(transport_error(format!(
            "unsupported fetch metadata protocol version: {}",
            metadata.protocol_version
        )));
    }
    if metadata
        .error
        .as_ref()
        .is_some_and(|error| error.len() > MAX_FETCH_ERROR_BYTES)
    {
        return Err(transport_error("fetch error message exceeds bridge limit"));
    }
    Ok(metadata)
}

fn reject_oversized_content_length(
    headers: &HeaderMap,
    maximum: u64,
) -> Result<(), TransportError> {
    let value = headers
        .get_single("content-length")
        .map_err(|error| transport_error(error.to_string()))?;
    let Some(value) = value else {
        return Ok(());
    };
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(transport_error(
            "Content-Length is not a canonical unsigned integer",
        ));
    }
    let content_length = value
        .parse::<u64>()
        .map_err(|_| transport_error("Content-Length is outside the u64 range"))?;
    if content_length > maximum {
        Err(transport_error(format!(
            "declared Content-Length {content_length} exceeds {maximum} byte request limit"
        )))
    } else {
        Ok(())
    }
}

fn transport_error(message: impl Into<String>) -> TransportError {
    TransportError::new(message, false)
}

/// Pulls one image using a synchronous trusted-host callback.
///
/// # Safety
///
/// `context` must satisfy the callback's contract for the complete call.
pub unsafe fn pull_image_json(
    input: &str,
    callback: Option<RishRegistryFetchCallback>,
    context: *mut c_void,
) -> String {
    let response = run_pull(input, callback, context)
        .map(PullImageResponse::success)
        .unwrap_or_else(PullImageResponse::failure);
    serde_json::to_string(&response).unwrap_or_else(|error| {
        format!(
            "{{\"protocol_version\":1,\"ok\":false,\"receipt\":null,\
             \"error\":\"response serialization failed: {error}\"}}"
        )
    })
}

fn run_pull(
    input: &str,
    callback: Option<RishRegistryFetchCallback>,
    context: *mut c_void,
) -> Result<PullReceipt, String> {
    if input.len() > MAX_PULL_REQUEST_BYTES {
        return Err("pull request exceeds ABI size limit".to_owned());
    }
    let request = serde_json::from_str::<PullImageRequest>(input)
        .map_err(|error| format!("invalid pull request JSON: {error}"))?;
    if request.protocol_version != PROTOCOL_VERSION {
        return Err(format!(
            "unsupported pull protocol version: {}",
            request.protocol_version
        ));
    }
    if request.reference.is_empty() || request.reference.len() > 1024 {
        return Err("image reference length is outside the accepted range".to_owned());
    }
    let root = validate_store_root(&request.store_root)?;
    let policy = pull_policy(request.limits)?;
    let callback = callback.ok_or_else(|| "registry fetch callback is null".to_owned())?;

    let _operation = PULL_LOCK
        .lock()
        .map_err(|_| "pull coordinator lock is poisoned".to_owned())?;
    let store = ContentStore::open(StoreConfig::new(root))
        .map_err(|error| format!("could not open content store: {error}"))?;
    let transport = CallbackTransport {
        callback,
        context,
        temporary_directory: store.root().join("tmp"),
    };
    let image = Puller::new(&transport, &store)
        .with_policy(policy)
        .with_guest_platform(request.platform)
        .pull_str(&request.reference)
        .map_err(|error| format!("image pull failed: {error}"))?;
    let records = VerifiedImageRecordStore::open(&store)
        .map_err(|error| format!("could not open verified image records: {error}"))?;
    let persisted = records
        .persist(&image)
        .map_err(|error| format!("could not persist verified image record: {error}"))?;
    let receipt = build_receipt(&image, persisted.graph_pin)?;
    Ok(receipt)
}

fn validate_store_root(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if value.is_empty() || !path.is_absolute() || path.parent().is_none() || path == Path::new("/")
    {
        return Err("content store root must be a non-root absolute path".to_owned());
    }
    Ok(path.to_owned())
}

fn pull_policy(limits: PullLimitRequest) -> Result<PullPolicy, String> {
    let max_manifest_bytes = tightened_u64(
        "max_manifest_bytes",
        limits.max_manifest_bytes,
        DEFAULT_MAX_MANIFEST_BYTES,
    )?;
    let max_config_bytes = tightened_u64(
        "max_config_bytes",
        limits.max_config_bytes,
        DEFAULT_MAX_CONFIG_BYTES,
    )?;
    let max_layers = tightened_usize("max_layers", limits.max_layers, DEFAULT_MAX_LAYERS)?;
    let max_layer_bytes = tightened_u64(
        "max_layer_bytes",
        limits.max_layer_bytes,
        DEFAULT_FFI_MAX_LAYER_BYTES,
    )?;
    let max_total_bytes = tightened_u64(
        "max_total_bytes",
        limits.max_total_bytes,
        DEFAULT_FFI_MAX_TOTAL_BYTES,
    )?;
    if max_layer_bytes > max_total_bytes {
        return Err("max_layer_bytes cannot exceed max_total_bytes".to_owned());
    }
    Ok(PullPolicy {
        max_manifest_bytes,
        max_config_bytes,
        max_layers,
        max_layer_bytes,
        max_total_bytes,
        json_limits: rish_pull::JsonLimits::default(),
    })
}

fn tightened_u64(name: &str, requested: Option<u64>, maximum: u64) -> Result<u64, String> {
    let value = requested.unwrap_or(maximum);
    if value == 0 || value > maximum {
        Err(format!("{name} must be between 1 and {maximum}"))
    } else {
        Ok(value)
    }
}

fn tightened_usize(name: &str, requested: Option<usize>, maximum: usize) -> Result<usize, String> {
    let value = requested.unwrap_or(maximum);
    if value == 0 || value > maximum {
        Err(format!("{name} must be between 1 and {maximum}"))
    } else {
        Ok(value)
    }
}

fn build_receipt(image: &PulledImage, pin: String) -> Result<PullReceipt, String> {
    let mut sizes = image
        .index
        .iter()
        .map(|blob| blob.content.size)
        .chain(std::iter::once(image.manifest.content.size))
        .chain(std::iter::once(image.config.content.size))
        .chain(image.layers.iter().map(|blob| blob.content.size));
    let total_verified_bytes = sizes
        .try_fold(0_u64, u64::checked_add)
        .ok_or_else(|| "verified byte count overflowed u64".to_owned())?;
    let variant = image
        .manifest
        .descriptor
        .platform
        .as_ref()
        .and_then(|platform| platform.variant.clone());
    let layers = image
        .layers
        .iter()
        .map(|blob| LayerReceipt {
            digest: blob.descriptor.digest.to_string(),
            size: blob.content.size,
            media_type: blob.descriptor.media_type.to_string(),
        })
        .collect();

    Ok(PullReceipt {
        normalized_reference: image.reference.to_string(),
        resolved_digest: image.resolved_digest().to_string(),
        index_digest: image
            .index
            .as_ref()
            .map(|blob| blob.descriptor.digest.to_string()),
        manifest_digest: image.manifest.descriptor.digest.to_string(),
        config_digest: image.config.descriptor.digest.to_string(),
        os: image.image_configuration.os.clone(),
        architecture: image.image_configuration.architecture.clone(),
        variant,
        layers,
        total_verified_bytes,
        pin,
        content_store: "app_private_cas",
    })
}

pub(crate) unsafe fn invoke_pull_image_abi(
    input: *const c_char,
    input_len: usize,
    callback: Option<RishRegistryFetchCallback>,
    context: *mut c_void,
) -> *mut c_char {
    let response = if input.is_null() {
        serde_json::to_string(&PullImageResponse::failure("null pull request"))
            .expect("static response is serializable")
    } else if input_len > MAX_PULL_REQUEST_BYTES {
        serde_json::to_string(&PullImageResponse::failure(
            "pull request exceeds ABI size limit",
        ))
        .expect("static response is serializable")
    } else {
        // SAFETY: The caller guarantees `input_len` readable bytes.
        let bytes = unsafe { std::slice::from_raw_parts(input.cast::<u8>(), input_len) };
        match std::str::from_utf8(bytes) {
            Ok(input) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // SAFETY: The exported function forwards the callback contract.
                unsafe { pull_image_json(input, callback, context) }
            }))
            .unwrap_or_else(|_| {
                serde_json::to_string(&PullImageResponse::failure(
                    "pull operation panicked and was aborted",
                ))
                .expect("static response is serializable")
            }),
            Err(error) => serde_json::to_string(&PullImageResponse::failure(format!(
                "pull request is not UTF-8: {error}"
            )))
            .expect("error response is serializable"),
        }
    };
    CString::new(response)
        .expect("serialized JSON cannot contain an interior NUL")
        .into_raw()
}

#[cfg(test)]
mod tests;
