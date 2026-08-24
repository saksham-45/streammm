use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use streamaid::config;
use streamaid::server::App;

#[derive(Parser, Debug)]
#[command(name = "streamaid", version, about = "Twitch-class 1080p30 screen origin")]
struct Args {
    /// Config file path (created with defaults if missing)
    #[arg(short, long, default_value = "./config.json")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let cfg = config::load(&args.config)?;
    let bind: SocketAddr = format!("{}:{}", cfg.host, cfg.port).parse()?;
    tracing::info!(
        "streamaid {} encoder={} bitrate={}k gop={} max={}x{} publish={}",
        env!("CARGO_PKG_VERSION"),
        cfg.encoder.mode,
        cfg.encoder.bitrate_kbps,
        cfg.encoder.gop_frames,
        cfg.encoder.max_width,
        cfg.encoder.max_height,
        if cfg.cloudflare.publish_url.is_empty() {
            "(none)"
        } else {
            "set"
        }
    );

    let app = App::new(cfg, args.config.clone());
    app.start_background();
    let router = app.router();

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!("listening on http://{bind}");
    tracing::info!("config file: {}", args.config.display());
    use axum::serve::ListenerExt;
    let listener = listener.tap_io(|stream| {
        if let Err(err) = stream.set_nodelay(true) {
            tracing::trace!("TCP_NODELAY: {err:#}");
        }
    });
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutting down");
}
