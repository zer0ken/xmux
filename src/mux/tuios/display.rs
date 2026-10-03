//! The tuios display driver: one daemon owns every session, but no external command
//! can retarget a particular client, so display selection uses per-session reattach
//! semantics.

use std::sync::{Arc, Mutex};

use crate::app::runtime::{host_selection_key, request_attach, terminal_view_size};
use crate::display::grid::Grid;
use crate::driver::{DriverCtx, MuxDriver};
use crate::model::Selection;

/// tuios display orchestration through one per-source PTY, reattached whenever a
/// session is selected.
pub struct TuiosDriver;

impl MuxDriver for TuiosDriver {
    fn kind(&self) -> &str {
        "tuios"
    }

    fn show(&mut self, sel: &Selection, ctx: &mut DriverCtx) -> bool {
        if sel.is_empty() {
            return false;
        }
        let (cols, rows) = terminal_view_size(ctx.cols, ctx.body_rows, ctx.nav);
        let Some(host) = ctx.hosts.get_mut(&sel.source) else {
            return false;
        };
        let key = host_selection_key(host);
        let live = ctx.registry.contains(&key);
        let pre_mismatch = host.display.shows(&key) != Some(sel.session.as_str());

        let reason = if live { "reshow" } else { "no-live-client" };
        tracing::info!(
            host = %sel.source,
            model = "per-session",
            decision = "reattach",
            reason,
            session = %sel.session,
            "display_show"
        );
        host.display.clear(&key);
        let mux_argv = host.mux.attach_plan(&sel.session);
        let command = host.transport.exec_argv(true, &mux_argv);
        let id = request_attach(
            ctx.registry,
            ctx.worker,
            &mut host.display,
            ctx.attach_seq,
            &key,
            command,
            (cols, rows),
        );
        tracing::info!(addr = %key, id, count = ctx.registry.len(), "attach_created");
        host.display.set_shows(&key, &sel.session);
        crate::driver::log_display_inventory!(ctx, sel.session, pre_mismatch);
        true
    }

    fn grid(&self, sel: &Selection, ctx: &DriverCtx) -> Option<Arc<Mutex<Grid>>> {
        ctx.registry
            .grid(&crate::app::runtime::display_key(ctx.hosts, sel))
    }

    fn input(&mut self, sel: &Selection, bytes: Vec<u8>, ctx: &DriverCtx) {
        ctx.registry
            .input(&crate::app::runtime::display_key(ctx.hosts, sel), bytes);
    }

    fn sync(&mut self, source: &str, sessions: &[crate::session::Session], ctx: &mut DriverCtx) {
        if sessions.is_empty() {
            ctx.registry.remove(source);
            if let Some(host) = ctx.hosts.get_mut(source) {
                host.display.clear(source);
            }
        }
    }
}
