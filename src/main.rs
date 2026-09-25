#![allow(dead_code)]

mod api;
mod credentials;
mod jobs;
mod markdown;
mod model;
mod pending;
mod persist;
mod preview;
mod protocol;
mod tray;
mod ui;

use clap::Parser;
use cosmic::app::Settings;

#[derive(Clone, Debug, Parser)]
#[command(version, about)]
pub struct Args {
    /// OpenCode server URL.
    #[arg(long, env = "OPENCODE_SERVER_URL")]
    pub server: Option<String>,

    /// HTTP Basic Auth username used when a password is configured.
    #[arg(long, env = "OPENCODE_SERVER_USERNAME")]
    pub username: Option<String>,

    /// HTTP Basic Auth password.
    #[arg(long, env = "OPENCODE_SERVER_PASSWORD")]
    pub password: Option<String>,

    /// Cloudflare Access service-token client ID.
    #[arg(long, env = "OPENCODE_CF_ACCESS_CLIENT_ID")]
    pub cf_access_client_id: Option<String>,

    /// Cloudflare Access service-token secret.
    #[arg(long, env = "OPENCODE_CF_ACCESS_CLIENT_SECRET")]
    pub cf_access_client_secret: Option<String>,

    /// Show canned UI without contacting a server.
    #[arg(long)]
    pub preview: bool,

    /// Initial drawer page to show (jobs, sessions, settings)
    #[arg(long)]
    pub drawer: Option<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let mut settings = Settings::default();
    settings = settings.size_limits(
        cosmic::iced::Limits::NONE
            .min_width(480.0)
            .min_height(360.0),
    );
    cosmic::app::run::<ui::OpenCodeCosmic>(settings, args)?;
    Ok(())
}
