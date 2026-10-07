//! The gnerkulfloot binary: API server, migration runner and admin CLI in one.

use std::{io::BufRead, net::SocketAddr, path::PathBuf};

use anyhow::Context;
use clap::{Parser, Subcommand};
use gnerkulfloot::{
    app::{self, AppState},
    auth::{Role, password, setup, users},
    config::{Config, LogFormat},
    db,
};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(version, about = "Headless shop backend")]
struct Cli {
    /// TOML config file. Optional: everything can come from GNK__* env vars instead.
    #[arg(long, short, env = "GNK_CONFIG", default_value = "gnerkulfloot.toml", global = true)]
    config: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP API (the default when no command is given).
    Serve,
    /// Apply pending database migrations and exit.
    Migrate,
    /// Print the first-time setup token (if setup hasn't been completed yet).
    SetupToken,
    /// Manage staff and admin accounts.
    #[command(subcommand)]
    Admin(AdminCommand),
}

#[derive(Subcommand)]
enum AdminCommand {
    /// Create an admin or staff account. Prompts for the password.
    Create {
        #[arg(long)]
        email: String,
        #[arg(long, value_enum, default_value_t = AccountRole::Admin)]
        role: AccountRole,
        /// Read the password from stdin instead of prompting (for scripts).
        #[arg(long)]
        password_stdin: bool,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum AccountRole {
    Admin,
    Staff,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = Config::load(&cli.config).context("loading config")?;
    init_tracing(config.server.log_format);

    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(config).await,
        Command::Migrate => {
            let pool = db::connect(&config.database).await.context("connecting to database")?;
            db::migrate(&pool).await.context("running migrations")?;
            tracing::info!("migrations applied");
            Ok(())
        }
        Command::SetupToken => {
            let pool = ready_pool(&config).await?;
            match setup::token(&pool, &config.setup).await? {
                Some(token) => println!("{token}"),
                None => println!("setup already completed; create more accounts with `gnerkulfloot admin create`"),
            }
            Ok(())
        }
        Command::Admin(AdminCommand::Create {
            email,
            role,
            password_stdin,
        }) => {
            let pool = ready_pool(&config).await?;
            create_account(&pool, &config, &email, role, password_stdin).await
        }
    }
}

/// Connects and applies migrations, for CLI commands that need the schema.
async fn ready_pool(config: &Config) -> anyhow::Result<sqlx::PgPool> {
    let pool = db::connect(&config.database).await.context("connecting to database")?;
    db::migrate(&pool).await.context("running migrations")?;
    Ok(pool)
}

async fn create_account(
    pool: &sqlx::PgPool,
    config: &Config,
    email: &str,
    role: AccountRole,
    password_stdin: bool,
) -> anyhow::Result<()> {
    let email = users::normalize_email(email)?;
    let pw = if password_stdin {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        line.trim_end_matches(['\r', '\n']).to_owned()
    } else {
        let pw = rpassword::prompt_password("Password: ")?;
        anyhow::ensure!(
            pw == rpassword::prompt_password("Repeat password: ")?,
            "passwords don't match"
        );
        pw
    };
    password::validate(&pw, config.auth.min_password_length)?;
    let hash = password::hash(pw).await?;
    let role = match role {
        AccountRole::Admin => Role::Admin,
        AccountRole::Staff => Role::Staff,
    };
    let mut tx = pool.begin().await?;
    let user = users::create(&mut tx, &email, Some(&hash), role).await?;
    // Someone with server access has created an account, so web setup is no longer needed.
    setup::mark_complete(&mut tx).await?;
    tx.commit().await?;
    println!("created {role:?} account {} ({})", user.email, user.id);
    Ok(())
}

async fn serve(config: Config) -> anyhow::Result<()> {
    let pool = db::connect(&config.database).await.context("connecting to database")?;
    if config.server.migrate_on_start {
        db::migrate(&pool).await.context("running migrations")?;
    }
    if let Some(token) = setup::token(&pool, &config.setup).await.context("preparing setup")? {
        tracing::warn!(
            setup_token = %token,
            "first-time setup pending: POST /v1/setup with this token, an email and a password to create the admin account"
        );
    }

    let bind = config.server.bind;
    let state = AppState::new(pool, config).context("setting up adapters")?;
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(%bind, "listening");

    // Connect info gives the rate limiter the peer address.
    axum::serve(
        listener,
        app::router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("server error")
}

/// Resolves on Ctrl-C or SIGTERM (what Docker and systemd send), letting
/// in-flight requests finish before the process exits.
async fn shutdown_signal() {
    let ctrl_c = async { tokio::signal::ctrl_c().await.ok() };
    #[cfg(unix)]
    let term = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("installing SIGTERM handler")
            .recv()
            .await
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<Option<()>>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
    tracing::info!("shutting down");
}

fn init_tracing(format: LogFormat) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx::postgres::notice=warn"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    match format {
        LogFormat::Json => builder.json().init(),
        LogFormat::Text => builder.init(),
    }
}
