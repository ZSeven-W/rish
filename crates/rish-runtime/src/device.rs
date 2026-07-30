use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Character,
    Block,
    Virtual,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeviceNode {
    pub path: String,
    pub kind: DeviceKind,
    pub major: u32,
    pub minor: u32,
    pub host_operation: Option<String>,
}

#[derive(Debug, Default)]
pub struct DeviceRegistry {
    nodes: BTreeMap<String, DeviceNode>,
}

impl DeviceRegistry {
    pub fn register(&mut self, node: DeviceNode) -> Result<(), String> {
        if !node.path.starts_with("/dev/") {
            return Err("device path must be under /dev".to_owned());
        }
        if self.nodes.contains_key(&node.path) {
            return Err(format!("device already registered: {}", node.path));
        }
        self.nodes.insert(node.path.clone(), node);
        Ok(())
    }

    #[must_use]
    pub fn resolve(&self, path: &str) -> Option<&DeviceNode> {
        self.nodes.get(path)
    }
}
