use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    Inactive,
    Activating,
    Active,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceUnit {
    pub name: String,
    pub command: Vec<String>,
    pub state: ServiceState,
    pub restart_count: u32,
}

#[derive(Debug, Default)]
pub struct ServiceManager {
    units: BTreeMap<String, ServiceUnit>,
}

impl ServiceManager {
    pub fn define(&mut self, name: impl Into<String>, command: Vec<String>) -> Result<(), String> {
        let name = normalize_unit_name(name.into())?;
        if command.is_empty() {
            return Err("service command must not be empty".to_owned());
        }
        self.units.insert(
            name.clone(),
            ServiceUnit {
                name,
                command,
                state: ServiceState::Inactive,
                restart_count: 0,
            },
        );
        Ok(())
    }

    pub fn transition(&mut self, name: &str, state: ServiceState) -> Result<&ServiceUnit, String> {
        let name = normalize_unit_name(name.to_owned())?;
        let unit = self
            .units
            .get_mut(&name)
            .ok_or_else(|| format!("unknown service: {name}"))?;
        if state == ServiceState::Activating && unit.state == ServiceState::Failed {
            unit.restart_count += 1;
        }
        unit.state = state;
        Ok(unit)
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ServiceUnit> {
        self.units.get(name)
    }
}

fn normalize_unit_name(mut name: String) -> Result<String, String> {
    if name.contains('/') || name.contains("..") || name.is_empty() {
        return Err("invalid service name".to_owned());
    }
    if !name.ends_with(".service") {
        name.push_str(".service");
    }
    Ok(name)
}
