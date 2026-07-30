use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamespaceKind {
    Process,
    User,
    Mount,
    Network,
    Uts,
    Ipc,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct NamespaceId(pub u64);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NamespaceSet {
    ids: BTreeMap<NamespaceKind, NamespaceId>,
}

impl NamespaceSet {
    #[must_use]
    pub fn id(&self, kind: NamespaceKind) -> Option<NamespaceId> {
        self.ids.get(&kind).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (NamespaceKind, NamespaceId)> + '_ {
        self.ids.iter().map(|(kind, id)| (*kind, *id))
    }
}

#[derive(Debug)]
pub struct NamespaceStore {
    next_id: u64,
    root: NamespaceSet,
}

impl Default for NamespaceStore {
    fn default() -> Self {
        let mut store = Self {
            next_id: 1,
            root: NamespaceSet {
                ids: BTreeMap::new(),
            },
        };
        for kind in [
            NamespaceKind::Process,
            NamespaceKind::User,
            NamespaceKind::Mount,
            NamespaceKind::Network,
            NamespaceKind::Uts,
            NamespaceKind::Ipc,
        ] {
            let id = store.allocate();
            store.root.ids.insert(kind, id);
        }
        store
    }
}

impl NamespaceStore {
    #[must_use]
    pub fn root(&self) -> NamespaceSet {
        self.root.clone()
    }

    #[must_use]
    pub fn unshare(
        &mut self,
        parent: &NamespaceSet,
        kinds: impl IntoIterator<Item = NamespaceKind>,
    ) -> NamespaceSet {
        let mut child = parent.clone();
        for kind in kinds {
            let id = self.allocate();
            child.ids.insert(kind, id);
        }
        child
    }

    fn allocate(&mut self) -> NamespaceId {
        let id = NamespaceId(self.next_id);
        self.next_id += 1;
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unshare_replaces_only_requested_namespaces() {
        let mut store = NamespaceStore::default();
        let parent = store.root();
        let child = store.unshare(&parent, [NamespaceKind::Mount, NamespaceKind::Network]);

        assert_ne!(
            parent.id(NamespaceKind::Mount),
            child.id(NamespaceKind::Mount)
        );
        assert_ne!(
            parent.id(NamespaceKind::Network),
            child.id(NamespaceKind::Network)
        );
        assert_eq!(
            parent.id(NamespaceKind::Process),
            child.id(NamespaceKind::Process)
        );
    }
}
