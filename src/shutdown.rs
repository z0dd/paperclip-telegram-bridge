//! SIGTERM / SIGINT → `CancellationToken` (architecture §7.5).

use tokio_util::sync::CancellationToken;

/// Installs signal handlers explicitly (the binary is PID 1 in the container)
/// and cancels `token` on the first signal.
pub fn install(token: CancellationToken) -> std::io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    tokio::spawn(async move {
        let name = tokio::select! {
            _ = term.recv() => "SIGTERM",
            _ = int.recv() => "SIGINT",
            _ = token.cancelled() => return,
        };
        tracing::info!(signal = name, "shutdown requested");
        token.cancel();
    });
    Ok(())
}
