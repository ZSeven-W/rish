# rish-pull

`rish-pull` connects the HTTP-neutral `rish-registry` protocol to the
`rish-content` SHA-256 content store. It resolves a tag or digest, selects
Linux/ARM64 from an image index, validates the selected manifest and image
configuration, and returns one RAII lease covering the resulting OCI graph.

Status, canonical `Content-Length`, media type, and registry digest headers are
checked before the first body read. Manifests and image configuration use
strict 8 MiB buffers and pass structural and semantic validation. Layers flow
directly into a temporary CAS ingress file and are committed atomically only
after CAS verifies their exact descriptor size and SHA-256 digest.

Before serde materializes a manifest or config, a non-retaining token walk
caps nesting, total values, per-container entries, individual strings, and
cumulative string bytes. This prevents a byte-bounded document from expanding
into millions of small heap allocations. The limits are configurable through
`PullPolicy::json_limits`.

## Default mobile budgets

The default policy accepts at most 512 MiB of compressed data per layer and
2 GiB across one pull. The default content store admits 4 GiB of committed
blobs, so one default-sized pull fits an empty default store. Applications
should lower these limits when device capacity requires it. Raising a pull
limit also requires a compatible `StoreConfig` quota and a platform free-space
check.

## Trusted transport boundary

`RegistryTransport` implementations must enforce each request's
`max_response_bytes` while streaming and must never build a complete layer
`Vec`. An over-limit stream must fail rather than masquerade as EOF. Transports
are also responsible for bounded redirect and authentication handling. In
particular, a transport must not forward `Authorization` across origins or
follow an HTTPS-to-HTTP downgrade.

The puller validates all returned response metadata again. This catches a
faulty transport returning wrong bytes, but it cannot recover memory already
allocated by a transport that ignored the streaming limit.
