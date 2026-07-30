mod common;
mod extra;
mod inspect;
mod list;
mod mutate;

use rish_core::GuestCommand;

use crate::{AppletContext, AppletError, AppletOutput, Result};

pub(crate) fn execute(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    match command.basename() {
        "du" | "find" | "ls" => list::execute(context, command),
        "chmod" | "mktemp" => extra::execute(context, command),
        "readlink" | "realpath" | "stat" => inspect::execute(context, command),
        "cp" | "ln" | "mkdir" | "mv" | "rm" | "rmdir" | "touch" => {
            mutate::execute(context, command)
        }
        name => Err(AppletError::UnknownApplet(name.to_owned())),
    }
}
