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
}
pub(crate) fn noop_ops() -> Arc<dyn Ops> {
    Arc::new(NoopOps)
}

/// The nav the default settings resolve for `area`: the position the loop would pick
/// with no [ui] overrides and nothing pinned (a left column wide, a band narrow).
/// The render-path tests use this so a portrait backend paints the band without any
/// test having to state the placement.
pub(crate) fn auto_nav(width: u16, area: Rect) -> NavSize {
    NavSize::visible(width).with_position(resolve_nav_position(
        &NavPositionSetting::default(),
        None,
        area,
        width,
    ))
}
