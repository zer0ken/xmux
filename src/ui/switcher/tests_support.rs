use super::*;
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
