mod cat_head;
mod common;
mod cut_tr;
mod grep_tee;
mod wc_sort;

use rish_core::GuestCommand;

use crate::{AppletContext, AppletError, AppletOutput, Result};

pub(crate) fn execute(context: &AppletContext, command: &GuestCommand) -> Result<AppletOutput> {
    match command.basename() {
        "cat" => cat_head::cat(context, command),
        "head" => cat_head::head_tail(context, command, false),
        "tail" => cat_head::head_tail(context, command, true),
        "wc" => wc_sort::wc(context, command),
        "sort" => wc_sort::sort(context, command),
        "uniq" => wc_sort::uniq(context, command),
        "cut" => cut_tr::cut(context, command),
        "tr" => cut_tr::tr(context, command),
        "grep" => grep_tee::grep(context, command),
        "tee" => grep_tee::tee(context, command),
        name => Err(AppletError::UnknownApplet(name.to_owned())),
    }
}
