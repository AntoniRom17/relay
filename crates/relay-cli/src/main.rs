//! Relay command-line interface and process orchestration.

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context as _, bail};
use clap::{Parser, Subcommand};
use relay_config::{GatewayConfig, load_from_path};
use relay_control::ControlPlane;
use relay_core::Shutdown;
use relay_limits::{LocalRateLimiter, RedisRateLimiter};
use relay_observability::{Metrics, init_logging};
use relay_proxy::{ProxyService, RuntimeState, serve_http};
use tokio::task::JoinSet;
use tracing::{error, info};

#[derive(Debug, Parser)]
#[command(
    name = "relay",
    version,
    about = "A high-performance, production-minded API gateway",
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the Relay gateway until SIGINT or SIGTERM.
    Run {
        /// YAML configuration file.
        #[arg(
            short,
            long,
            env = "RELAY_CONFIG",
            default_value = "configs/basic.yaml"
        )]
        config: PathBuf,
    },
    /// Parse and semantically validate a configuration file.
    Validate {
        /// YAML configuration file.
        #[arg(
            short,
            long,
            env = "RELAY_CONFIG",
            default_value = "configs/basic.yaml"
        )]
        config: PathBuf,
    },
    /// Print a canonical, secret-redacted configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Print build and runtime capability information.
    Check,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Validate and print a canonical form of the configuration.
    Print {
        /// YAML configuration file.
        #[arg(
            short,
            long,
            env = "RELAY_CONFIG",
            default_value = "configs/basic.yaml"
        )]
        config: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Run { config } => run(config).await,
        Command::Validate { config } => {
            let config = load_from_path(&config)?;
            println!(
                "valid: {} listener(s), {} route(s), {} upstream pool(s)",
                config.listeners.len(),
                config.routes.len(),
                config.upstreams.len()
            );
            Ok(())
        }
        Command::Config {
            command: ConfigCommand::Print { config },
        } => {
            let mut config = load_from_path(&config)?;
            redact(&mut config);
            print!("{}", serde_yaml::to_string(&config)?);
            Ok(())
        }
        Command::Check => {
            println!(
                "relay {}\nhttp: 1.1, 2\ntls: rustls\nruntime: tokio",
                env!("CARGO_PKG_VERSION")
            );
            Ok(())
        }
    }
}

async fn run(path: PathBuf) -> anyhow::Result<()> {
    let config = load_from_path(&path)
        .with_context(|| format!("unable to start from {}", path.display()))?;
    init_logging(&config.observability)?;
    reject_unavailable_listener_features(&config)?;
    let metrics = Metrics::new().context("failed to create metrics registry")?;
    let shutdown = Shutdown::new(Duration::from_millis(config.shutdown_grace_ms));
    let runtime = Arc::new(RuntimeState::build(config.clone())?);
    let local_limiter = Arc::new(LocalRateLimiter::new(100_000));
    let redis_limiter = if config
        .routes
        .iter()
        .filter_map(|route| route.rate_limit.as_ref())
        .any(|limit| limit.distributed)
    {
        let redis = config
            .redis
            .as_ref()
            .context("distributed rate limiting requires Redis configuration")?;
        Some(
            RedisRateLimiter::connect(&redis.url, redis.key_prefix.clone())
                .await
                .context("failed to connect distributed rate limiter")?,
        )
    } else {
        None
    };
    let connect_timeout = config
        .upstreams
        .values()
        .map(|upstream| upstream.connect_timeout_ms)
        .min()
        .map_or(Duration::from_secs(3), Duration::from_millis);
    let mut servers = JoinSet::new();

    for listener in config.listeners.clone() {
        let proxy = ProxyService::new(
            Arc::clone(&runtime),
            metrics.clone(),
            Duration::from_millis(listener.request_timeout_ms),
            connect_timeout,
            Arc::clone(&local_limiter),
            redis_limiter.clone(),
        );
        let metrics = metrics.clone();
        let shutdown = shutdown.clone();
        servers.spawn(async move {
            serve_http(listener, proxy, metrics, shutdown)
                .await
                .map_err(anyhow::Error::from)
        });
    }
    if config.admin.enabled {
        let control = ControlPlane::new(Arc::clone(&runtime), metrics.clone());
        let address = config.admin.address;
        let shutdown = shutdown.clone();
        servers.spawn(async move {
            control
                .serve(address, shutdown)
                .await
                .map_err(anyhow::Error::from)
        });
    }

    info!(config = %path.display(), "Relay started");
    tokio::select! {
        signal = shutdown_signal() => {
            signal?;
            info!("shutdown signal received");
        }
        result = servers.join_next() => {
            match result {
                Some(Ok(Ok(()))) => bail!("gateway server stopped unexpectedly"),
                Some(Ok(Err(error))) => return Err(error).context("gateway server failed"),
                Some(Err(error)) => return Err(error).context("gateway server task failed"),
                None => bail!("no gateway servers are running"),
            }
        }
    }
    shutdown.cancel();
    while let Some(result) = servers.join_next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => error!(%error, "server failed during shutdown"),
            Err(error) => error!(%error, "server task failed during shutdown"),
        }
    }
    info!("Relay stopped cleanly");
    Ok(())
}

fn reject_unavailable_listener_features(config: &GatewayConfig) -> anyhow::Result<()> {
    if config
        .listeners
        .iter()
        .any(|listener| listener.tls.is_some())
    {
        bail!("TLS listener configuration requires the relay-tls feature, not yet enabled")
    }
    Ok(())
}

fn redact(config: &mut GatewayConfig) {
    let key_count = config.authentication.api_key_sha256.len();
    config.authentication.api_key_sha256 = if key_count == 0 {
        Vec::new()
    } else {
        vec![format!("<redacted:{key_count}-digests>")]
    };
    if let Some(redis) = &mut config.redis {
        redis.url = "<redacted>".to_owned();
    }
}

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}
