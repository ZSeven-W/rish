use rish_registry::GuestPlatform;

use super::*;
use crate::VerifiedImageRecordStore;

fn direct_fixture_for_architecture(architecture: &str) -> Fixture {
    let parts = manifest_parts_with_config(
        &[MediaType::OciImageLayerGzip],
        1,
        false,
        architecture,
        ImageConfig {
            cmd: vec!["/bin/sh".to_owned()],
            working_dir: "/".to_owned(),
            ..ImageConfig::default()
        },
    );
    let reference: ImageReference = format!(
        "registry.test/team/{architecture}@{}",
        parts.descriptor.digest
    )
    .parse()
    .unwrap();
    let mut exchanges = vec![ExpectedExchange {
        path: reference.manifest_path(),
        response: response(&parts.descriptor, parts.body),
    }];
    exchanges.push(ExpectedExchange {
        path: reference.blob_path(&parts.config.digest),
        response: response(&parts.config, parts.config_body),
    });
    exchanges.extend(
        parts
            .layers
            .iter()
            .zip(parts.layer_bodies)
            .map(|(descriptor, body)| ExpectedExchange {
                path: reference.blob_path(&descriptor.digest),
                response: response(descriptor, body),
            }),
    );
    Fixture {
        reference,
        root: parts.descriptor.clone(),
        manifest: parts.descriptor,
        config: parts.config,
        layers: parts.layers,
        exchanges,
    }
}

#[test]
fn pulls_and_reopens_an_amd64_direct_manifest_for_a_linux_guest() {
    let fixture = direct_fixture_for_architecture("amd64");
    let resolved = fixture.root.digest.clone();
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();
    let image = Puller::new(&transport, &store)
        .with_guest_platform(GuestPlatform::LinuxAmd64)
        .pull(&fixture.reference)
        .unwrap();

    assert_eq!(image.image_configuration.os, "linux");
    assert_eq!(image.image_configuration.architecture, "amd64");
    let records = VerifiedImageRecordStore::open(&store).unwrap();
    let persisted = records.persist(&image).unwrap();
    assert_eq!(persisted.record.platform.architecture, "amd64");
    assert_eq!(persisted.record.platform.variant, None);
    drop(image);

    let reopened = records
        .reopen_for_platform(&resolved, GuestPlatform::LinuxAmd64)
        .unwrap();
    assert_eq!(reopened.record, persisted.record);
}

#[test]
fn direct_manifest_config_must_match_the_explicit_guest_architecture() {
    let fixture = direct_fixture_for_architecture("arm64");
    let transport = MockTransport::new(fixture.exchanges);
    let (_temporary, store) = content_store();

    let error = Puller::new(&transport, &store)
        .with_guest_platform(GuestPlatform::LinuxAmd64)
        .pull(&fixture.reference)
        .unwrap_err();

    assert!(matches!(
        error,
        PullError::ImagePlatformMismatch {
            expected_os,
            expected_architecture,
            actual_os,
            actual_architecture,
        } if expected_os == "linux"
            && expected_architecture == "amd64"
            && actual_os == "linux"
            && actual_architecture == "arm64"
    ));
}
