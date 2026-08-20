use super::*;

#[test]
fn digest_addressed_root_accepts_missing_registry_digest() {
    let mut fixture = direct_fixture();
    replace_with_descriptor_headers(
        &mut fixture.exchanges[0].response,
        &fixture.root,
        &fixture.root.media_type.to_string(),
    );
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();

    let image = Puller::new(&transport, &store)
        .pull(&fixture.reference)
        .unwrap();

    assert_eq!(image.resolved_digest(), &fixture.root.digest);
    transport.assert_finished();
}

#[test]
fn descriptor_requests_accept_docker_cdn_blob_headers() {
    let mut fixture = index_fixture();
    replace_with_descriptor_headers(
        &mut fixture.exchanges[1].response,
        &fixture.manifest,
        &fixture.manifest.media_type.to_string(),
    );
    replace_with_descriptor_headers(
        &mut fixture.exchanges[2].response,
        &fixture.config,
        MediaType::OCTET_STREAM,
    );
    for (exchange, descriptor) in fixture.exchanges[3..].iter_mut().zip(&fixture.layers) {
        replace_with_descriptor_headers(
            &mut exchange.response,
            descriptor,
            MediaType::OCTET_STREAM,
        );
    }
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();

    let image = Puller::new(&transport, &store)
        .pull(&fixture.reference)
        .unwrap();

    assert_eq!(image.manifest.descriptor.digest, fixture.manifest.digest);
    assert_eq!(image.config.descriptor.digest, fixture.config.digest);
    assert_eq!(image.layers.len(), fixture.layers.len());
    transport.assert_finished();
}

#[test]
fn tagged_root_still_requires_registry_digest() {
    let mut fixture = tagged_manifest_fixture();
    replace_with_descriptor_headers(
        &mut fixture.exchanges[0].response,
        &fixture.root,
        &fixture.root.media_type.to_string(),
    );
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();

    let error = Puller::new(&transport, &store)
        .pull(&fixture.reference)
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::Response(ResponseValidationError::MissingRegistryDigest)
    ));
}

#[test]
fn descriptor_request_rejects_mismatched_optional_registry_digest() {
    let mut fixture = direct_fixture();
    fixture.exchanges[1]
        .response
        .headers
        .insert(
            "docker-content-digest",
            Digest::sha256(b"not-the-config").to_string(),
        )
        .unwrap();
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();

    let error = Puller::new(&transport, &store)
        .pull(&fixture.reference)
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::Response(ResponseValidationError::RegistryDigestMismatch { .. })
    ));
}

#[test]
fn descriptor_digest_is_verified_without_a_registry_digest_header() {
    let mut fixture = direct_fixture();
    replace_with_descriptor_headers(
        &mut fixture.exchanges[0].response,
        &fixture.root,
        &fixture.root.media_type.to_string(),
    );
    fixture.exchanges[0].response.body[0] ^= 1;
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();

    let error = Puller::new(&transport, &store)
        .pull(&fixture.reference)
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::Response(ResponseValidationError::DigestValidation(
            DigestValidationError::Mismatch { .. }
        ))
    ));
    assert!(!store.contains(cas_digest(&fixture.root.digest)).unwrap());
}
