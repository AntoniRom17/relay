use std::{convert::Infallible, sync::Arc};

use hyper::service::service_fn;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto,
};
use relay_config::ListenerConfig;
use relay_core::Shutdown;
use relay_observability::Metrics;
use thiserror::Error;
use tokio::{net::TcpListener, task::JoinSet};
use tracing::{debug, info, warn};

use crate::ProxyService;

/// Serves one plaintext HTTP listener until coordinated shutdown completes.
pub async fn serve_http(
    config: ListenerConfig,
    proxy: ProxyService,
    metrics: Metrics,
    shutdown: Shutdown,
) -> Result<(), ServeError> {
    let listener = TcpListener::bind(config.address)
        .await
        .map_err(|source| ServeError::Bind {
            address: config.address,
            source,
        })?;
    let connection_limit = Arc::new(tokio::sync::Semaphore::new(config.max_connections));
    let mut connections = JoinSet::new();
    info!(address = %config.address, "data-plane listener started");

    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            accepted = listener.accept() => {
                let (stream, peer) = accepted.map_err(ServeError::Accept)?;
                let Ok(permit) = Arc::clone(&connection_limit).try_acquire_owned() else {
                    warn!(peer = %peer, "connection rejected because listener capacity is exhausted");
                    continue;
                };
                let proxy = proxy.clone();
                let metrics = metrics.clone();
                let connection_shutdown = shutdown.child_token();
                connections.spawn(async move {
                    let _permit = permit;
                    let _metric = ConnectionMetricGuard::new(metrics);
                    let service = service_fn(move |request| {
                        let proxy = proxy.clone();
                        async move {
                            Ok::<_, Infallible>(proxy.handle(request, peer.ip(), "http").await)
                        }
                    });
                    let io = TokioIo::new(stream);
                    let builder = auto::Builder::new(TokioExecutor::new());
                    let connection = builder.serve_connection_with_upgrades(io, service);
                    tokio::pin!(connection);
                    tokio::select! {
                        result = &mut connection => {
                            if let Err(error) = result {
                                debug!(peer = %peer, %error, "client connection ended with an error");
                            }
                        }
                        () = connection_shutdown.cancelled() => {
                            connection.as_mut().graceful_shutdown();
                            if let Err(error) = connection.await {
                                debug!(peer = %peer, %error, "connection drain ended with an error");
                            }
                        }
                    }
                });
            }
        }
    }

    info!(address = %config.address, "data-plane listener draining");
    let drain = async {
        while let Some(result) = connections.join_next().await {
            if let Err(error) = result {
                warn!(%error, "connection task failed while draining");
            }
        }
    };
    if tokio::time::timeout(shutdown.grace_period(), drain)
        .await
        .is_err()
    {
        warn!(address = %config.address, "shutdown grace period elapsed; aborting connections");
        connections.abort_all();
    }
    info!(address = %config.address, "data-plane listener stopped");
    Ok(())
}

struct ConnectionMetricGuard(Metrics);

impl ConnectionMetricGuard {
    fn new(metrics: Metrics) -> Self {
        metrics.connection_opened();
        Self(metrics)
    }
}

impl Drop for ConnectionMetricGuard {
    fn drop(&mut self) {
        self.0.connection_closed();
    }
}

/// Listener startup or accept failure.
#[derive(Debug, Error)]
pub enum ServeError {
    /// The configured address could not be bound.
    #[error("failed to bind data-plane listener {address}: {source}")]
    Bind {
        /// Requested socket address.
        address: std::net::SocketAddr,
        /// Operating-system error.
        source: std::io::Error,
    },
    /// The operating system failed while accepting a connection.
    #[error("data-plane accept failed: {0}")]
    Accept(std::io::Error),
}
