use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// Coordinates cancellation and bounded draining across Relay tasks.
#[derive(Debug, Clone)]
pub struct Shutdown {
    token: CancellationToken,
    grace_period: Duration,
}

impl Shutdown {
    /// Creates a new shutdown coordinator.
    pub fn new(grace_period: Duration) -> Self {
        Self {
            token: CancellationToken::new(),
            grace_period,
        }
    }

    /// Returns a child token cancelled when the gateway begins shutdown.
    pub fn child_token(&self) -> CancellationToken {
        self.token.child_token()
    }

    /// Signals all participants to stop accepting work and begin draining.
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// Resolves when shutdown has been requested.
    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }

    /// Maximum duration allocated to draining in-flight work.
    pub const fn grace_period(&self) -> Duration {
        self.grace_period
    }

    /// Returns whether shutdown has already begun.
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_propagates_to_children() {
        let shutdown = Shutdown::new(Duration::from_secs(10));
        let child = shutdown.child_token();

        shutdown.cancel();

        child.cancelled().await;
        assert!(shutdown.is_cancelled());
        assert_eq!(shutdown.grace_period(), Duration::from_secs(10));
    }
}
