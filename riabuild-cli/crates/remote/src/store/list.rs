//! The box `riabuild remote list` prints.

use anyhow::Result;
use riabuild_tasks::Ctx;

use super::{Origin, Record, Store};

/// `riabuild remote list`.
///
/// The same box the picker shows, minus the numbers nothing here is waiting to
/// read. An empty store keeps its one line rather than rendering a box with no
/// rows, whose hints would name no server — `render::hints` exists to keep a
/// developer from being shown a command that refuses when typed.
pub fn list(ctx: &Ctx, store: &Store) -> Result<i32> {
    if store.remotes.is_empty() {
        ctx.ui
            .info("No servers yet. Run `riabuild remote` to add one.");
        return Ok(0);
    }
    ctx.ui.info("");
    ctx.ui.info(&crate::render::servers_box(
        &listing_order(store),
        crate::render::Shown::Listing,
        ctx.ui.theme(),
    ));
    Ok(0)
}

/// Everything that can be connected to, and then everything that cannot.
///
/// A server the leads have removed is still shown — its session may still be
/// live, and a row nobody can see is a row nobody can clear — but it is shown
/// after the servers that work, marked `no longer shared`, so the list still
/// reads top-down as "what you can use".
fn listing_order(store: &Store) -> Vec<Record> {
    let (stale, usable): (Vec<Record>, Vec<Record>) = store
        .remotes
        .iter()
        .cloned()
        .partition(|record| record.origin() == Origin::Stale);
    usable.into_iter().chain(stale).collect()
}
