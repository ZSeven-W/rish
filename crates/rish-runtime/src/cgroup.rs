use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub memory_max_bytes: Option<u64>,
    pub process_max: Option<u64>,
    pub cpu_weight: Option<u16>,
    pub io_weight: Option<u16>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Cgroup {
    pub path: String,
    pub limits: ResourceLimits,
    pub members: BTreeSet<u64>,
}

#[derive(Debug)]
pub struct CgroupStore {
    groups: BTreeMap<String, Cgroup>,
}

impl Default for CgroupStore {
    fn default() -> Self {
        let root = Cgroup {
            path: "/".to_owned(),
            limits: ResourceLimits::default(),
            members: BTreeSet::new(),
        };
        Self {
            groups: BTreeMap::from([(root.path.clone(), root)]),
        }
    }
}

impl CgroupStore {
    pub fn create(
        &mut self,
        path: impl Into<String>,
        limits: ResourceLimits,
    ) -> Result<&Cgroup, String> {
        let path = normalize_path(path.into())?;
        if path == "/" {
            return Err("the root cgroup already exists".to_owned());
        }
        if self.groups.contains_key(&path) {
            return Err(format!("cgroup already exists: {path}"));
        }
        let parent = parent_path(&path);
        if !self.groups.contains_key(parent) {
            return Err(format!("parent cgroup does not exist: {parent}"));
        }

        let group = Cgroup {
            path: path.clone(),
            limits,
            members: BTreeSet::new(),
        };
        self.groups.insert(path.clone(), group);
        Ok(self.groups.get(&path).expect("inserted cgroup must exist"))
    }

    pub fn attach(&mut self, path: &str, task_id: u64) -> Result<(), String> {
        let path = normalize_path(path.to_owned())?;
        for group in self.groups.values_mut() {
            group.members.remove(&task_id);
        }
        self.groups
            .get_mut(&path)
            .ok_or_else(|| format!("cgroup does not exist: {path}"))?
            .members
            .insert(task_id);
        Ok(())
    }

    #[must_use]
    pub fn get(&self, path: &str) -> Option<&Cgroup> {
        self.groups.get(path)
    }
}

fn normalize_path(path: String) -> Result<String, String> {
    if !path.starts_with('/') {
        return Err("cgroup path must be absolute".to_owned());
    }
    if path.contains("..") {
        return Err("cgroup path must not contain '..'".to_owned());
    }
    let normalized = path.trim_end_matches('/');
    Ok(if normalized.is_empty() {
        "/".to_owned()
    } else {
        normalized.to_owned()
    })
}

fn parent_path(path: &str) -> &str {
    path.rsplit_once('/').map_or(
        "/",
        |(parent, _)| if parent.is_empty() { "/" } else { parent },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_can_only_belong_to_one_cgroup() {
        let mut store = CgroupStore::default();
        store
            .create("/foreground", ResourceLimits::default())
            .unwrap();
        store
            .create("/background", ResourceLimits::default())
            .unwrap();

        store.attach("/foreground", 7).unwrap();
        store.attach("/background", 7).unwrap();

        assert!(!store.get("/foreground").unwrap().members.contains(&7));
        assert!(store.get("/background").unwrap().members.contains(&7));
    }
}
