use std::collections::BTreeSet;

use rish_content::{ContentStore, Sha256Digest};

use super::VerifiedImageRecordError;

pub(super) fn pin_graph_and_record(
    store: &ContentStore,
    graph_name: &str,
    graph: &BTreeSet<Sha256Digest>,
    record_name: &str,
    record_digest: Sha256Digest,
) -> Result<(Vec<Sha256Digest>, Vec<Sha256Digest>), VerifiedImageRecordError> {
    pin_graph_and_record_with(
        store,
        graph_name,
        graph,
        record_name,
        record_digest,
        pin_missing,
    )
}

fn pin_graph_and_record_with<F>(
    store: &ContentStore,
    graph_name: &str,
    graph: &BTreeSet<Sha256Digest>,
    record_name: &str,
    record_digest: Sha256Digest,
    pin_record: F,
) -> Result<(Vec<Sha256Digest>, Vec<Sha256Digest>), VerifiedImageRecordError>
where
    F: FnOnce(
        &ContentStore,
        &str,
        &BTreeSet<Sha256Digest>,
    ) -> Result<Vec<Sha256Digest>, VerifiedImageRecordError>,
{
    let created_graph = pin_missing(store, graph_name, graph)?;
    let created_record = match pin_record(store, record_name, &BTreeSet::from([record_digest])) {
        Ok(created) => created,
        Err(error) => {
            rollback_pins(store, graph_name, created_graph);
            return Err(error);
        }
    };
    Ok((created_graph, created_record))
}

pub(super) fn pin_missing(
    store: &ContentStore,
    name: &str,
    digests: &BTreeSet<Sha256Digest>,
) -> Result<Vec<Sha256Digest>, VerifiedImageRecordError> {
    let existing = pin_set(store, name)?;
    let mut created = Vec::new();
    for digest in digests.iter().copied() {
        if existing.contains(&digest) {
            continue;
        }
        if let Err(error) = store.pin(name, digest) {
            rollback_pins(store, name, created);
            return Err(error.into());
        }
        created.push(digest);
    }
    Ok(created)
}

pub(super) fn rollback_pins(store: &ContentStore, name: &str, digests: Vec<Sha256Digest>) {
    for digest in digests.into_iter().rev() {
        let _ = store.unpin(name, digest);
    }
}

pub(super) fn prune_record_pins(
    store: &ContentStore,
    name: &str,
    keep: Option<Sha256Digest>,
) -> Result<(), VerifiedImageRecordError> {
    for digest in pin_set(store, name)? {
        if Some(digest) != keep {
            store.unpin(name, digest)?;
        }
    }
    Ok(())
}

pub(super) fn ensure_exact_pin_set(
    store: &ContentStore,
    name: &str,
    expected: &BTreeSet<Sha256Digest>,
) -> Result<(), VerifiedImageRecordError> {
    if pin_set(store, name)? == *expected {
        Ok(())
    } else {
        Err(VerifiedImageRecordError::GraphPinMismatch)
    }
}

pub(super) fn ensure_pin_contains(
    store: &ContentStore,
    name: &str,
    digest: Sha256Digest,
) -> Result<(), VerifiedImageRecordError> {
    if pin_set(store, name)?.contains(&digest) {
        Ok(())
    } else {
        Err(VerifiedImageRecordError::MissingRecordPin)
    }
}

fn pin_set(
    store: &ContentStore,
    name: &str,
) -> Result<BTreeSet<Sha256Digest>, VerifiedImageRecordError> {
    Ok(store
        .pins()?
        .into_iter()
        .filter(|pin| pin.name == name)
        .map(|pin| pin.digest)
        .collect())
}

#[cfg(test)]
mod tests {
    use std::io;

    use rish_content::{ContentStore, StoreConfig};

    use super::*;

    #[test]
    fn record_pin_failure_rolls_back_graph_pins() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ContentStore::open(StoreConfig::new(temporary.path().join("content"))).unwrap();
        let graph_blob = store.ingest_bytes(b"graph").unwrap();
        let record_blob = store.ingest_bytes(b"record").unwrap();
        let graph = BTreeSet::from([graph_blob.digest]);

        let error = pin_graph_and_record_with(
            &store,
            "image-test",
            &graph,
            "record-test",
            record_blob.digest,
            |_store, _name, _digests| Err(io::Error::other("injected record pin failure").into()),
        )
        .unwrap_err();

        assert!(matches!(error, VerifiedImageRecordError::Io(_)));
        assert!(
            store
                .pins()
                .unwrap()
                .into_iter()
                .all(|pin| pin.name != "image-test" && pin.name != "record-test")
        );
    }
}
