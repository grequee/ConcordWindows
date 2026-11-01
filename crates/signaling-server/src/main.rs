use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use concord_signaling_server::{Config, Server, build_router};
use tokio::net::TcpListener;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

/// Concord signaling server.
#[derive(Debug, Parser)]
#[command(name = "concord-signaling", version, about)]
struct Cli {
    /// Read hosting configuration/secrets from environment instead of TOML.
    #[arg(long, env = "CONCORD_ENV_CONFIG")]
    env_config: bool,
    /// Path to the TOML configuration file.
    #[arg(short, long, env = "CONCORD_CONFIG", default_value = "concord.toml")]
    config: PathBuf,

    /// Tracing filter, used when RUST_LOG is not set.
    #[arg(
        long,
        env = "CONCORD_LOG",
        default_value = "info,concord_signaling_server=debug"
    )]
    log: String,

    /// Validate the configuration and exit without binding a socket.
    #[arg(long)]
    check: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(&cli.log);

    let mut config = if cli.env_config {
        Config::from_env()?
    } else {
        Config::load(&cli.config)?
    };
    config.apply_port(std::env::var("PORT").ok().as_deref())?;
    let server = Server::new(config.clone()).context("could not build signaling state")?;

    if cli.check {
        let _ = concord_signaling_server::storage::StorageSettings::from_env()?;
        println!(
            "configuration OK: {} room(s), {} invite(s), bind {}",
            config.rooms.len(),
            config.invites.len(),
            config.bind
        );
        return Ok(());
    }

    let storage = match concord_signaling_server::storage::StorageSettings::from_env()? {
        Some(settings) => {
            let storage = std::sync::Arc::new(
                concord_signaling_server::storage::ChatStorage::connect(settings).await?,
            );
            let maintenance = storage.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                    maintenance.maintenance().await;
                }
            });
            Some(storage)
        }
        None => None,
    };
    let server = if storage.is_some() {
        Server::with_storage(config.clone(), storage)?
    } else {
        server
    };

    info!(
        bind = %config.bind,
        server = %config.server_name,
        rooms = config.rooms.len(),
        invites = config.invites.len(),
        capacity = config.room_capacity,
        max_clients = config.max_clients,
        "configuration loaded"
    );
    if !config
        .ice_servers
        .iter()
        .any(concord_protocol::IceServerConfig::is_turn)
    {
        warn!("no TURN server configured; calls across restrictive NAT/firewalls may fail");
    }

    let listener = TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("could not bind {}", config.bind))?;
    let local_addr = listener.local_addr().context("listener has no address")?;

    info!(
        %local_addr,
        protocol = concord_protocol::PROTOCOL_VERSION,
        "Concord signaling server ready"
    );
    if local_addr.ip().is_unspecified() {
        info!("listening on all interfaces; open the port in your LAN firewall");
    }

    let app = build_router(server).into_make_service_with_connect_info::<SocketAddr>();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("signaling server stopped with an error")?;

    info!("signaling server stopped cleanly");
    Ok(())
}

fn init_tracing(default_filter: &str) {
    let filter = match EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(_) => EnvFilter::new(default_filter),
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            warn!(%error, "could not install Ctrl+C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                warn!(%error, "could not install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    info!("shutdown signal received");
}
