mod api;
mod credentials;
mod markdown;
mod model;
mod persist;
mod ui;

use clap::Parser;
use gtk::prelude::*;

#[derive(Clone, Debug, Parser)]
#[command(version, about)]
struct Args {
    /// OpenCode server URL.
    #[arg(long, env = "OPENCODE_SERVER_URL")]
    server: Option<String>,

    /// HTTP Basic Auth username used when a password is configured.
    #[arg(long, env = "OPENCODE_SERVER_USERNAME")]
    username: Option<String>,

    /// HTTP Basic Auth password. Prefer OPENCODE_SERVER_PASSWORD over this flag.
    #[arg(long, env = "OPENCODE_SERVER_PASSWORD")]
    password: Option<String>,

    /// Cloudflare Access service-token client ID.
    #[arg(long, env = "OPENCODE_CF_ACCESS_CLIENT_ID")]
    cf_access_client_id: Option<String>,

    /// Cloudflare Access service-token secret. Prefer the system keyring or environment variable.
    #[arg(long, env = "OPENCODE_CF_ACCESS_CLIENT_SECRET")]
    cf_access_client_secret: Option<String>,
}

fn main() -> gtk::glib::ExitCode {
    let args = Args::parse();
    let application = gtk::Application::builder()
        .application_id("ai.opencode.Gtk")
        .build();

    application.connect_activate(move |application| {
        if let Some(window) = application.active_window() {
            window.present();
            return;
        }
        ui::launch(
            application,
            args.server.clone(),
            args.username.clone(),
            args.password.clone(),
            args.cf_access_client_id.clone(),
            args.cf_access_client_secret.clone(),
        );
    });

    application.run_with_args(&["opencode-gtk"])
}
