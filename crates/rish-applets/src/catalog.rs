use serde::{Deserialize, Serialize};

use rish_core::GuestCommand;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppletCategory {
    Core,
    Text,
    Filesystem,
    Identity,
    Checksum,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AppletSpec {
    pub name: &'static str,
    pub category: AppletCategory,
    pub mutates_filesystem: bool,
}

pub const APPLETS: &[AppletSpec] = &[
    AppletSpec::new("[", AppletCategory::Core, false),
    AppletSpec::new("base64", AppletCategory::Checksum, false),
    AppletSpec::new("basename", AppletCategory::Core, false),
    AppletSpec::new("cat", AppletCategory::Text, false),
    AppletSpec::new("chmod", AppletCategory::Filesystem, true),
    AppletSpec::new("cksum", AppletCategory::Checksum, false),
    AppletSpec::new("cp", AppletCategory::Filesystem, true),
    AppletSpec::new("cut", AppletCategory::Text, false),
    AppletSpec::new("date", AppletCategory::Identity, false),
    AppletSpec::new("dirname", AppletCategory::Core, false),
    AppletSpec::new("du", AppletCategory::Filesystem, false),
    AppletSpec::new("echo", AppletCategory::Core, false),
    AppletSpec::new("env", AppletCategory::Identity, false),
    AppletSpec::new("false", AppletCategory::Core, false),
    AppletSpec::new("find", AppletCategory::Filesystem, false),
    AppletSpec::new("grep", AppletCategory::Text, false),
    AppletSpec::new("groups", AppletCategory::Identity, false),
    AppletSpec::new("head", AppletCategory::Text, false),
    AppletSpec::new("hostname", AppletCategory::Identity, false),
    AppletSpec::new("id", AppletCategory::Identity, false),
    AppletSpec::new("ln", AppletCategory::Filesystem, true),
    AppletSpec::new("ls", AppletCategory::Filesystem, false),
    AppletSpec::new("mkdir", AppletCategory::Filesystem, true),
    AppletSpec::new("mktemp", AppletCategory::Filesystem, true),
    AppletSpec::new("mv", AppletCategory::Filesystem, true),
    AppletSpec::new("printenv", AppletCategory::Identity, false),
    AppletSpec::new("printf", AppletCategory::Core, false),
    AppletSpec::new("pwd", AppletCategory::Core, false),
    AppletSpec::new("readlink", AppletCategory::Filesystem, false),
    AppletSpec::new("realpath", AppletCategory::Filesystem, false),
    AppletSpec::new("rm", AppletCategory::Filesystem, true),
    AppletSpec::new("rmdir", AppletCategory::Filesystem, true),
    AppletSpec::new("seq", AppletCategory::Core, false),
    AppletSpec::new("sha256sum", AppletCategory::Checksum, false),
    AppletSpec::new("sha512sum", AppletCategory::Checksum, false),
    AppletSpec::new("sort", AppletCategory::Text, false),
    AppletSpec::new("stat", AppletCategory::Filesystem, false),
    AppletSpec::new("tail", AppletCategory::Text, false),
    AppletSpec::new("tee", AppletCategory::Text, true),
    AppletSpec::new("test", AppletCategory::Core, false),
    AppletSpec::new("touch", AppletCategory::Filesystem, true),
    AppletSpec::new("tr", AppletCategory::Text, false),
    AppletSpec::new("true", AppletCategory::Core, false),
    AppletSpec::new("uname", AppletCategory::Identity, false),
    AppletSpec::new("uniq", AppletCategory::Text, false),
    AppletSpec::new("wc", AppletCategory::Text, false),
    AppletSpec::new("whoami", AppletCategory::Identity, false),
];

impl AppletSpec {
    const fn new(name: &'static str, category: AppletCategory, mutates_filesystem: bool) -> Self {
        Self {
            name,
            category,
            mutates_filesystem,
        }
    }
}

#[must_use]
pub fn applet_spec(name: &str) -> Option<&'static AppletSpec> {
    APPLETS
        .binary_search_by_key(&name, |spec| spec.name)
        .ok()
        .map(|index| &APPLETS[index])
}

#[must_use]
pub fn is_portable_applet(name: &str) -> bool {
    applet_spec(name).is_some()
}

#[must_use]
pub fn is_portable_command(command: &GuestCommand) -> bool {
    command.program == command.basename() && is_portable_applet(command.basename())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_sorted_and_unique() {
        for pair in APPLETS.windows(2) {
            assert!(pair[0].name < pair[1].name);
        }
    }

    #[test]
    fn executable_paths_never_gain_applet_provenance_from_their_basename() {
        assert!(is_portable_command(&GuestCommand::new(
            "grep",
            Vec::<String>::new()
        )));
        assert!(!is_portable_command(&GuestCommand::new(
            "/app/grep",
            Vec::<String>::new()
        )));
        assert!(!is_portable_command(&GuestCommand::new(
            "./rm",
            Vec::<String>::new()
        )));
    }
}
