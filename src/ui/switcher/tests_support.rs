use super::*;
use ratatui::layout::Rect;
use std::sync::Arc;
/// A do-nothing [`Ops`] for apply-site tests. `Switch`/`Focus`/`Width`/`Quit`
/// never call into `Ops`, so its methods are never reached; constructing it is
/// all a app action-dispatch effect test needs.
struct NoopOps;
#[async_trait::async_trait]
impl Ops for NoopOps {
    fn sources(&self) -> Vec<String> {
        unreachable!("noop_ops is only constructed, never called")
    }
    async fn list_sessions(&self, _source: &str) -> anyhow::Result<Vec<Session>> {
        unreachable!("noop_ops is only constructed, never called")
    }
    async fn new_session(&self, _source: &str, _name: &str) -> anyhow::Result<Session> {
        unreachable!("noop_ops is only constructed, never called")
    }
    async fn unlock(
        &self,
        _source: &str,
        _user: &str,
        _password: &str,
    ) -> crate::link::unlock::UnlockOutcome {
        unreachable!("noop_ops is only constructed, never called")
    }
}
pub(crate) fn noop_ops() -> Arc<dyn Ops> {
    Arc::new(NoopOps)
}

/// The stacking that fits `area`, chosen for the render tests: a left column when a side
/// column leaves the terminal view wider than tall, a top band otherwise. Test-only: the
/// production position never derives from the screen (a `prefix p` pin or the `[ui]
/// nav-position` default decides), so the render tests ask for the fitting stacking here
/// explicitly and a portrait backend still paints the band without stating the placement.
pub(crate) fn auto_nav(width: u16, area: Rect) -> NavSize {
    let band = area.width.saturating_sub(width.saturating_add(1)) as u32 <= area.height as u32 * 2;
    let position = if band {
        NavPosition::Top
    } else {
        NavPosition::Left
    };
    NavSize::visible(width).with_position(position)
}
