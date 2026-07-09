mod ast_bridge;
mod config;
mod dap_bridge;
mod extension_host_bridge;
mod fs;
mod lsp;
mod lsp_bridge;
mod mcp_bridge;
mod ports;
mod proto_frame;
mod protocol;
mod relay;
mod sysmon;
mod terminal;
mod updates;
mod utils;

use clap::{Parser, Subcommand};
use colored::Colorize;
use config::DstermConfig;
use lsp::{start_lsp_server, LspBridgeConfig};
use relay::{crypto::Secretbox, pairing};
use std::net::Ipv4Addr;
use terminal::{init_config, set_default_command, start_server};
use updates::UpdateChecker;
use utils::get_ip_address;

const DEFAULT_PORT: u16 = 8767;
const LOCAL_IP: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 1);

#[derive(Parser)]
#[command(name = "dsterm", version, author = "Darkian Studio <contact@darkian.io>", about = "CLI/Server backend to serve pty over socket", long_about = None)]
struct Cli {
    /// Port to start the server
    #[arg(short, long, default_value_t = DEFAULT_PORT, value_parser = clap::value_parser!(u16).range(1..), global = true)]
    port: u16,
    /// Start the server on local network (ip)
    #[arg(short, long, global = true)]
    ip: bool,
    /// Custom command or shell for interactive PTY (e.g. "/usr/bin/bash")
    #[arg(short = 'c', long = "command")]
    command_override: Option<String>,
    /// Allow all origins for CORS (dangerous). By default only https://localhost is allowed.
    #[arg(long = "allow-any-origin", global = true)]
    allow_any_origin: bool,
    /// Path to a TOML configuration file
    #[arg(long = "config", global = true)]
    config_path: Option<String>,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Update dsterm server
    Update,
    /// Start a WebSocket LSP bridge for a stdio language server
    Lsp {
        /// Session ID for port discovery (allows multiple instances of same server)
        #[arg(short = 's', long)]
        session: Option<String>,
        /// The language server binary to run (e.g. "rust-analyzer")
        server: String,
        /// Additional arguments to forward to the language server
        #[arg(trailing_var_arg = true)]
        server_args: Vec<String>,
    },
    /// Print a Shellular-compatible pairing payload and QR code
    Pair {
        /// Host id from the relay. If omitted, relay.host_id_file is read.
        #[arg(long = "host-id")]
        host_id: Option<String>,
        /// Print only the hostId:key payload, without rendering a QR code.
        #[arg(long = "no-qr")]
        no_qr: bool,
    },
}

fn print_update_available(current_version: &str, new_version: &str) {
    println!("\n{}", "═".repeat(40).yellow());
    println!("{}", "  🎉  Update Available!".bright_yellow().bold());
    println!("  Current version: {}", current_version.bright_red());
    println!("  Latest version:  {}", new_version.bright_green());
    println!("  To update, run: {} {}", "dsterm".cyan(), "update".cyan());
    println!("{}\n", "═".repeat(40).yellow());
}

async fn check_updates_in_background() {
    let checker = UpdateChecker::new(env!("CARGO_PKG_VERSION"));
    match checker.check_update().await {
        Ok(Some(version)) => {
            print_update_available(env!("CARGO_PKG_VERSION"), &version);
        }
        Err(e) => eprintln!(
            "{} {}",
            "⚠️".yellow(),
            format!("Failed to check for updates: {e}").red()
        ),
        _ => {}
    }
}

fn load_config_or_default(path: Option<&str>, announce: bool) -> DstermConfig {
    if let Some(path) = path {
        match DstermConfig::load(path) {
            Ok(config) => {
                if announce {
                    println!("{} Config loaded from {}", "✓".bright_green(), path);
                }
                config
            }
            Err(e) => {
                eprintln!(
                    "{} Failed to load config from {path}: {e}. Using defaults.",
                    "⚠".yellow()
                );
                DstermConfig::default()
            }
        }
    } else {
        DstermConfig::default()
    }
}

#[tokio::main]
async fn main() {
    let cli: Cli = Cli::parse();

    let Cli {
        port,
        ip,
        command_override,
        allow_any_origin,
        config_path,
        command,
    } = cli;

    match command {
        Some(Commands::Update) => {
            println!("{} {}", "⟳".blue().bold(), "Checking for updates...".blue());

            let checker = UpdateChecker::new(env!("CARGO_PKG_VERSION"));

            match checker.check_update().await {
                Ok(Some(version)) => {
                    println!(
                        "{} Found new version: {}",
                        "↓".bright_green(),
                        version.green()
                    );
                    println!(
                        "{} {}",
                        "⟳".blue(),
                        "Downloading and installing update...".blue()
                    );

                    match checker.update().await {
                        Ok(()) => {
                            println!(
                                "\n{} {}",
                                "✓".bright_green().bold(),
                                "Update successful! Please restart dsterm.".green().bold()
                            );
                        }
                        Err(e) => {
                            eprintln!(
                                "\n{} {} {}",
                                "✗".red().bold(),
                                "Update failed:".red().bold(),
                                e
                            );
                            std::process::exit(1);
                        }
                    }
                }
                Ok(None) => {
                    println!(
                        "{} {}",
                        "✓".bright_green().bold(),
                        "You're already on the latest version!".green().bold()
                    );
                }
                Err(e) => {
                    eprintln!(
                        "{} {} {}",
                        "✗".red().bold(),
                        "Failed to check for updates:".red().bold(),
                        e
                    );
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::Lsp {
            session,
            server,
            server_args,
        }) => {
            let host = if ip {
                get_ip_address().unwrap_or_else(|| {
                    println!(
                        "{} localhost.",
                        "Error: IP address not found. Starting server on"
                            .red()
                            .bold()
                    );
                    LOCAL_IP
                })
            } else {
                LOCAL_IP
            };

            let config = LspBridgeConfig {
                program: server,
                args: server_args,
            };

            let lsp_port = if port != DEFAULT_PORT {
                Some(port)
            } else {
                None
            };

            start_lsp_server(host, lsp_port, session, allow_any_origin, config).await;
        }
        Some(Commands::Pair { host_id, no_qr }) => {
            let cfg = load_config_or_default(config_path.as_deref(), false);
            let secretbox = match Secretbox::load_or_create(cfg.security.key_file.as_deref()) {
                Ok(secretbox) => secretbox,
                Err(e) => {
                    eprintln!("{} Failed to load/create E2E key: {e}", "✗".red().bold());
                    std::process::exit(1);
                }
            };
            let host_id = match pairing::resolve_host_id(
                host_id.as_deref(),
                cfg.relay.host_id_file.as_deref(),
            ) {
                Ok(host_id) => host_id,
                Err(e) => {
                    eprintln!("{} Failed to resolve host id: {e}", "✗".red().bold());
                    std::process::exit(1);
                }
            };
            let payload = match pairing::PairingPayload::new(host_id, secretbox.key_base64()) {
                Ok(payload) => payload,
                Err(e) => {
                    eprintln!("{} Failed to build pairing payload: {e}", "✗".red().bold());
                    std::process::exit(1);
                }
            };

            if !no_qr {
                match pairing::render_qr(&payload) {
                    Ok(qr) => println!("{qr}"),
                    Err(e) => {
                        eprintln!("{} Failed to render QR: {e}", "✗".red().bold());
                        std::process::exit(1);
                    }
                }
            }
            println!("{}", payload.qr_text());
        }
        None => {
            // Load runtime config (defaults if no --config supplied).
            let cfg = if let Some(ref path) = config_path {
                match DstermConfig::load(path) {
                    Ok(c) => {
                        println!("{} Config loaded from {}", "✓".bright_green(), path);
                        c
                    }
                    Err(e) => {
                        eprintln!(
                            "{} Failed to load config from {path}: {e}. Using defaults.",
                            "⚠".yellow()
                        );
                        DstermConfig::default()
                    }
                }
            } else {
                DstermConfig::default()
            };
            init_config(cfg);

            tokio::task::spawn(check_updates_in_background());

            if let Some(cmd) = command_override {
                set_default_command(cmd);
            }

            let ip = if ip {
                get_ip_address().unwrap_or_else(|| {
                    println!(
                        "{} localhost.",
                        "Error: IP address not found. Starting server on"
                            .red()
                            .bold()
                    );
                    LOCAL_IP
                })
            } else {
                LOCAL_IP
            };

            start_server(ip, port, allow_any_origin).await;
        }
    }
}
