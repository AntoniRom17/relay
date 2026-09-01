use std::{convert::Infallible, sync::Arc, time::Duration};

use hyper::service::service_fn;
use hyper_util::{
    rt::{TokioExecutor, TokioIo, TokioTimer},
    server::conn::auto,
};
use relay_config::ListenerConfig;
use relay_core::Shutdown;
use relay_observability::Metrics;
use relay_tls::ReloadingTlsAcceptor;
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
    serve(config, proxy, metrics, shutdown, None).await
}

/// Serves one rustls HTTP listener until coordinated shutdown completes.
pub async fn serve_tls(
    config: ListenerConfig,
    proxy: ProxyService,
    metrics: Metrics,
    shutdown: Shutdown,
    acceptor: ReloadingTlsAcceptor,
) -> Result<(), ServeError> {
    serve(config, proxy, metrics, shutdown, Some(acceptor)).await
}

async fn serve(
    config: ListenerConfig,
    proxy: ProxyService,
    metrics: Metrics,
    shutdown: Shutdown,
    tls: Option<ReloadingTlsAcceptor>,
) -> Result<(), ServeError> {
    let listener = TcpListener::bind(config.address)
        .await
        .map_err(|source| ServeError::Bind {
            address: config.address,
            source,
        })?;
    let connection_limit = Arc::new(tokio::sync::Semaphore::new(config.max_connections));
    let max_header_bytes = config.max_header_bytes;
    let header_timeout = Duration::from_millis(config.request_timeout_ms.min(10_000));
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
                let tls = tls.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    let _metric = ConnectionMetricGuard::new(metrics);
                    if let Some(acceptor) = tls {
                        match acceptor.accept(stream).await {
                            Ok(stream) => {
                                run_connection(stream, proxy, peer, "https", connection_shutdown, max_header_bytes, header_timeout).await;
                            }
                            Err(error) => {
                                debug!(peer = %peer, %error, "TLS handshake failed");
                            }
                        }
                    } else {
                        run_connection(stream, proxy, peer, "http", connection_shutdown, max_header_bytes, header_timeout).await;
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

async fn run_connection<IO>(
    stream: IO,
    proxy: ProxyService,
    peer: std::net::SocketAddr,
    protocol: &'static str,
    shutdown: tokio_util::sync::CancellationToken,
    max_header_bytes: usize,
    header_timeout: Duration,
) where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service = service_fn(move |request| {
        let proxy = proxy.clone();
        async move { Ok::<_, Infallible>(proxy.handle(request, peer.ip(), protocol).await) }
    });
    let io = TokioIo::new(stream);
    let mut builder = auto::Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(header_timeout)
        .max_buf_size(max_header_bytes.max(8 * 1024));
    builder
        .http2()
        .max_header_list_size(u32::try_from(max_header_bytes).unwrap_or(u32::MAX));
    let connection = builder.serve_connection_with_upgrades(io, service);
    tokio::pin!(connection);
    tokio::select! {
        result = &mut connection => {
            if let Err(error) = result {
                debug!(peer = %peer, %error, "client connection ended with an error");
            }
        }
        () = shutdown.cancelled() => {
            connection.as_mut().graceful_shutdown();
            if let Err(error) = connection.await {
                debug!(peer = %peer, %error, "connection drain ended with an error");
            }
        }
    }
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
