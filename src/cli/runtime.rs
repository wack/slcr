use miette::{IntoDiagnostic, Result};
use tokio::runtime::Runtime;

/// Build a multi-threaded `tokio` runtime and drive `future` to completion
/// on it.
///
/// Command handlers keep a synchronous `dispatch()`; an async command calls
/// this from its `dispatch()` to `block_on` its async pipeline:
///
/// ```ignore
/// pub fn dispatch(self) -> Result<()> {
///     block_on(self.run())?
/// }
/// ```
///
/// The runtime is entered for the duration of the call, so code that needs an
/// ambient runtime handle (e.g. `tokio::spawn`) works from within `future`.
///
/// Note: the runtime is dropped before this returns, and dropping a `Runtime`
/// blocks until every task it spawned has stopped. A command that may leave
/// slow-to-unwind background tasks behind should exit the process directly
/// once it has its result rather than relying on this returning promptly.
pub(crate) fn block_on<F: Future>(future: F) -> Result<F::Output> {
    let rt = Runtime::new().into_diagnostic()?;
    let _guard = rt.enter();
    Ok(rt.block_on(future))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_on_returns_the_future_output() {
        assert_eq!(block_on(async { 1 + 1 }).unwrap(), 2);
    }

    #[test]
    fn block_on_supports_spawning_tasks() {
        let output = block_on(async { tokio::spawn(async { "spawned" }).await.unwrap() });
        assert_eq!(output.unwrap(), "spawned");
    }
}
