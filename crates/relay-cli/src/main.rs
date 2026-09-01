//! Relay command-line interface and process orchestration.

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context as _, bail};
use clap::{Parser, Subcommand};
use relay_config::{GatewayConfig, load_from_path};
use relay_control::ControlPlane;
use relay_core::Shutdown;
use relay_limits::{LocalRateLimiter, RedisRateLimiter};
use relay_observability::{Metrics, init_logging};
use relay_proxy::{ProxyService, RuntimeState, RuntimeStore, serve_http};
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
    let runtime = Arc::new(RuntimeStore::new(RuntimeState::build(config.clone())?));
    let local_limiter = Arc::new(LocalRateLimiter::new(100_000));
    let redis_limiter = if let Some(redis) = config.redis.as_ref() {
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
    {
        let runtime = Arc::clone(&runtime);
        let metrics = metrics.clone();
        let shutdown = shutdown.clone();
        let config_path = path.clone();
        servers.spawn(
            async move { watch_configuration(config_path, runtime, metrics, shutdown).await },
        );
    }

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

async fn watch_configuration(
    path: PathBuf,
    runtime: Arc<RuntimeStore>,
    metrics: Metrics,
    shutdown: Shutdown,
) -> anyhow::Result<()> {
    let mut stamp = file_stamp(&path).await?;
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => return Ok(()),
            _ = interval.tick() => {
                let current_stamp = match file_stamp(&path).await {
                    Ok(stamp) => stamp,
                    Err(error) => {
                        metrics.config_reload("read_error");
                        error!(%error, config = %path.display(), "failed to inspect configuration for reload");
                        continue;
                    }
                };
                if current_stamp == stamp {
                    continue;
                }
                stamp = current_stamp;
                let load_path = path.clone();
                let candidate = tokio::task::spawn_blocking(move || load_from_path(load_path)).await;
                let candidate = match candidate {
                    Ok(Ok(candidate)) => candidate,
                    Ok(Err(error)) => {
                        metrics.config_reload("validation_error");
                        error!(%error, config = %path.display(), "configuration reload rejected");
                        continue;
                    }
                    Err(error) => {
                        metrics.config_reload("task_error");
                        error!(%error, "configuration reload task failed");
                        continue;
                    }
                };
                let current = runtime.load();
                if let Err(error) = ensure_reload_compatible(current.config(), &candidate) {
                    metrics.config_reload("restart_required");
                    error!(%error, "configuration reload requires a process restart");
                    continue;
                }
                drop(current);
                match runtime.reload(candidate) {
                    Ok(()) => {
                        metrics.config_reload("success");
                        info!(config = %path.display(), "configuration reloaded atomically");
                    }
                    Err(error) => {
                        metrics.config_reload("compile_error");
                        error!(%error, "configuration reload could not be compiled");
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    modified: std::time::SystemTime,
    length: u64,
}

async fn file_stamp(path: &PathBuf) -> std::io::Result<FileStamp> {
    let metadata = tokio::fs::metadata(path).await?;
    Ok(FileStamp {
        modified: metadata.modified()?,
        length: metadata.len(),
    })
}

fn ensure_reload_compatible(
    current: &GatewayConfig,
    candidate: &GatewayConfig,
) -> anyhow::Result<()> {
    let current_immutable = serde_yaml::to_string(&(
        &current.listeners,
        &current.admin,
        &current.redis,
        &current.observability,
        current.shutdown_grace_ms,
    ))?;
    let candidate_immutable = serde_yaml::to_string(&(
        &candidate.listeners,
        &candidate.admin,
        &candidate.redis,
        &candidate.observability,
        candidate.shutdown_grace_ms,
    ))?;
    if current_immutable != candidate_immutable {
        bail!(
            "listeners, admin, Redis, observability, and shutdown settings are immutable at runtime"
        );
    }
    Ok(())
}
