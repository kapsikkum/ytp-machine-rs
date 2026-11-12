use std::io::{BufRead, IsTerminal};
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use ytp_auth::{secret, AuthStore};
use ytp_server::{app, AppState, Config};

#[derive(Parser)]
#[command(name = "ytp-machine", version, about = "YTP / YTPMV generator server")]
struct Cli {
    /// Where app.db and installed corpora live.
    #[arg(long, global = true, env = "YTP_DATA_DIR", default_value = "./data")]
    data_dir: PathBuf,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the web server.
    Serve(ServeArgs),
    /// Manage accounts from the command line (admin escape hatch).
    #[command(subcommand)]
    User(UserCmd),
}

#[derive(clap::Args)]
struct ServeArgs {
    #[arg(long, env = "YTP_BIND", default_value = "127.0.0.1:8765")]
    bind: SocketAddr,
    /// ffmpeg binary used for rendering (default: `ffmpeg` on PATH).
    #[arg(long, env = "YTP_FFMPEG")]
    ffmpeg: Option<PathBuf>,
    /// How many jobs may render at once, across all users.
    #[arg(long, env = "YTP_JOB_WORKERS", default_value_t = 2)]
    job_workers: usize,
    /// Extra origin allowed to open the WebSocket, e.g. http://localhost:5173.
    #[arg(
        long = "allowed-origin",
        env = "YTP_ALLOWED_ORIGINS",
        value_delimiter = ','
    )]
    allowed_origins: Vec<String>,
    /// Trust X-Forwarded-For / X-Forwarded-Host (only behind a reverse proxy).
    #[arg(long, env = "YTP_TRUST_PROXY")]
    trust_proxy: bool,
    /// Always mark the session cookie Secure.
    #[arg(long, env = "YTP_SECURE_COOKIES")]
    secure_cookies: bool,
    /// Size cap of the cut-clip cache in MB (0 turns it off).
    #[arg(long, env = "YTP_CLIP_CACHE_MB", default_value_t = 500)]
    clip_cache_mb: u64,
}

#[derive(Subcommand)]
enum UserCmd {
    /// Create a user. Prints a generated password unless --password-stdin.
    Create {
        name: String,
        /// Role to grant (repeatable).
        #[arg(long = "role", default_value = "creator")]
        roles: Vec<String>,
        /// Read the password from the first line of stdin.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Set a new password and sign the user out everywhere.
    ResetPassword {
        name: String,
        #[arg(long)]
        password_stdin: bool,
    },
}

fn password(from_stdin: bool) -> anyhow::Result<(String, bool)> {
    if from_stdin {
        if std::io::stdin().is_terminal() {
            eprint!("password: ");
        }
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        let pw = line.trim_end_matches(['\r', '\n']).to_string();
        Ok((pw, false))
    } else {
        Ok((secret::random_secret()[..20].to_string(), true))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn,tower_http=info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Serve(a) => serve(cli.data_dir, a).await,
        Cmd::User(u) => user_cmd(cli.data_dir, u).await,
    }
}

async fn serve(data_dir: PathBuf, a: ServeArgs) -> anyhow::Result<()> {
    let config = Config {
        data_dir,
        bind: a.bind,
        ffmpeg: a.ffmpeg,
        job_workers: a.job_workers,
        allowed_origins: a.allowed_origins,
        trust_proxy: a.trust_proxy,
        secure_cookies: a.secure_cookies,
        clip_cache_mb: a.clip_cache_mb,
    };
    let state = AppState::new(config.clone())
        .await
        .context("opening data directory")?;
    state.spawn_maintenance();
    ytp_server::bots::start(&state);
    match state.renderer.check() {
        Ok(v) => tracing::info!(ffmpeg = %v, "rendering enabled"),
        Err(e) => tracing::warn!("rendering unavailable until ffmpeg is installed or --ffmpeg is set: {e}"),
    }

    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("binding {}", config.bind))?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, data_dir = %config.data_dir.display(), "listening");
    if !ytp_server::web::has_ui() {
        tracing::warn!("web UI not embedded (web/build missing at compile time)");
    }
    if let Some(token) = state.setup_token().await {
        let host = if addr.ip().is_unspecified() {
            format!("localhost:{}", addr.port())
        } else {
            addr.to_string()
        };
        println!();
        println!("  No users yet. Create the admin account at:");
        println!("    http://{host}/setup?token={token}");
        println!();
    }

    axum::serve(
        listener,
        app(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("shutting down");
    })
    .await?;
    Ok(())
}

async fn user_cmd(data_dir: PathBuf, cmd: UserCmd) -> anyhow::Result<()> {
    std::fs::create_dir_all(&data_dir)?;
    let store = AuthStore::open(&data_dir.join("app.db")).await?;
    match cmd {
        UserCmd::Create {
            name,
            roles,
            password_stdin,
        } => {
            let (pw, generated) = password(password_stdin)?;
            let user = store.create_user(&name, &pw, &roles).await?;
            store
                .audit(None, "cli.user_create", Some(&user.username), None, None)
                .await;
            println!(
                "created {} (roles: {})",
                user.username,
                user.roles.join(", ")
            );
            if generated {
                println!("password: {pw}");
            }
        }
        UserCmd::ResetPassword {
            name,
            password_stdin,
        } => {
            let Some(user) = store.find_user(&name).await? else {
                bail!("no user called {name}");
            };
            let (pw, generated) = password(password_stdin)?;
            store.set_password(user.id, &pw).await?;
            store
                .audit(None, "cli.reset_password", Some(&user.username), None, None)
                .await;
            println!(
                "password reset for {}; their sessions were signed out",
                user.username
            );
            if generated {
                println!("password: {pw}");
            }
        }
    }
    Ok(())
}
