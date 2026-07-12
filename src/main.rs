mod agent_bridge;
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
mod proxy;
mod relay;
mod startup;
mod sysmon;
mod terminal;
mod updates;
mod utils;

use clap::{Parser, Subcommand};
use colored::Colorize;
use config::DstermConfig;
use lsp::{start_lsp_server, LspBridgeConfig};
use relay::{clients::ClientStore, crypto::Secretbox, pairing};
use std::net::Ipv4Addr;
use terminal::{init_config, set_default_command, start_server};
use tokio::fs;
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
    /// Enable the remote filesystem API (/fs/*) using the current directory as
    /// the workspace root, with no config file required. Equivalent to
    /// [filesystem] enabled = true.
    #[arg(long = "remote", global = true)]
    remote: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Update dsterm server
    Update,
    /// Revert to the previous dsterm version (the binary stashed as
    /// dsterm.old by the last successful `update`)
    Downgrade,
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
    /// Manage approved relay clients
    Clients {
        #[command(subcommand)]
        action: ClientsAction,
    },
    /// Register this host with the relay and cache the returned hostId
    Register,
    /// Run as a relay host: serve locally on 127.0.0.1 and bridge to the relay
    Host,
    /// Install an OS-native autostart entry (systemd user unit or Termux:Boot)
    Startup,
}

#[derive(Subcommand)]
enum ClientsAction {
    /// List all known clients and their approval state
    List,
    /// Approve a client by id
    Approve {
        /// The client id to approve
        client_id: String,
    },
    /// Reject a client by id
    Reject {
        /// The client id to reject
        client_id: String,
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
    match checker.check_update(false).await {
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
        remote,
        command,
    } = cli;

    match command {
        Some(Commands::Update) => {
            println!("{} {}", "⟳".blue().bold(), "Checking for updates...".blue());

            let checker = UpdateChecker::new(env!("CARGO_PKG_VERSION"));

            match checker.check_update(true).await {
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
        Some(Commands::Downgrade) => {
            let current_exe = std::env::current_exe()?;
            let old_path = current_exe.with_extension("old");
            if !old_path.exists() {
                eprintln!(
                    "{} {}",
                    "✗".red().bold(),
                    "No previous version found (dsterm.old is missing).".red()
                );
                std::process::exit(1);
            }

            println!(
                "{} {}",
                "⟲".blue(),
                "Reverting to the previous version...".blue()
            );

            // A running binary cannot be overwritten in place on Windows, so swap
            // via a sibling: stash the running exe, drop the old one in, then
            // remove the stash. On Unix an atomic rename over the running
            // binary is fine.
            #[cfg(windows)]
            {
                let stash_path = current_exe.with_extension("disabled");
                let _ = fs::remove_file(&stash_path).await;
                fs::rename(&current_exe, &stash_path).await?;
                fs::rename(&old_path, &current_exe).await?;
                let _ = fs::remove_file(&stash_path).await;
            }
            #[cfg(not(windows))]
            {
                fs::rename(&old_path, &current_exe).await?;
            }

            println!(
                "\n{} {}",
                "✓".right_green().bold(),
                "Revert successful! Please restart dsterm.".green().bold()
            );
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
        Some(Commands::Clients { action }) => {
            let cfg = load_config_or_default(config_path.as_deref(), false);
            let mut store = match ClientStore::load_or_default(cfg.security.clients_file.as_deref())
            {
                Ok(store) => store,
                Err(e) => {
                    eprintln!("{} Failed to load clients store: {e}", "✗".red().bold());
                    std::process::exit(1);
                }
            };
            match action {
                ClientsAction::List => {
                    let clients = store.list();
                    if clients.is_empty() {
                        println!("No known clients.");
                    } else {
                        for record in clients {
                            println!(
                                "{}  {:?}  platform={}  app={}",
                                record.client_id,
                                record.approval,
                                record.platform.as_deref().unwrap_or("-"),
                                record.app_version.as_deref().unwrap_or("-"),
                            );
                        }
                    }
                }
                ClientsAction::Approve { client_id } => match store.approve(&client_id) {
                    Ok(()) => println!("{} Approved {client_id}", "✓".bright_green().bold()),
                    Err(e) => {
                        eprintln!("{} {e}", "✗".red().bold());
                        std::process::exit(1);
                    }
                },
                ClientsAction::Reject { client_id } => match store.reject(&client_id) {
                    Ok(()) => println!("{} Rejected {client_id}", "✓".bright_green().bold()),
                    Err(e) => {
                        eprintln!("{} {e}", "✗".red().bold());
                        std::process::exit(1);
                    }
                },
            }
        }
        Some(Commands::Register) => {
            let cfg = load_config_or_default(config_path.as_deref(), false);
            let http = reqwest::Client::new();
            let machine_id = relay::register::machine_id();
            match relay::register::register_host(
                &http,
                &cfg.relay.server_url,
                cfg.relay.host_id_file.as_deref(),
                &machine_id,
            )
            .await
            {
                Ok(host_id) => {
                    println!("{} Registered host: {host_id}", "✓".bright_green().bold());
                }
                Err(e) => {
                    eprintln!("{} Registration failed: {e}", "✗".red().bold());
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::Host) => {
            let mut cfg = load_config_or_default(config_path.as_deref(), true);
            if remote {
                cfg.filesystem.enabled = true;
            }
            init_config(cfg.clone());

            if let Some(cmd) = command_override {
                set_default_command(cmd);
            }

            let secretbox = match Secretbox::load_or_create(cfg.security.key_file.as_deref()) {
                Ok(sb) => sb,
                Err(e) => {
                    eprintln!("{} Failed to load/create E2E key: {e}", "✗".red().bold());
                    std::process::exit(1);
                }
            };

            let host_id = match relay::register::read_cached(cfg.relay.host_id_file.as_deref()) {
                Some(id) => id,
                None => {
                    let http = reqwest::Client::new();
                    let machine_id = relay::register::machine_id();
                    match relay::register::register_host(
                        &http,
                        &cfg.relay.server_url,
                        cfg.relay.host_id_file.as_deref(),
                        &machine_id,
                    )
                    .await
                    {
                        Ok(id) => id,
                        Err(e) => {
                            eprintln!(
                                "{} Host not registered and registration failed: {e}",
                                "✗".red().bold()
                            );
                            std::process::exit(1);
                        }
                    }
                }
            };

            println!(
                "{} Serving locally on 127.0.0.1:{port} and bridging to relay as host {host_id}",
                "⟳".blue().bold()
            );

            let server = tokio::spawn(async move {
                start_server(LOCAL_IP, port, allow_any_origin).await;
            });
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let relay_task = tokio::spawn(async move {
                relay::transport::run(cfg, secretbox, host_id, port).await;
            });

            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    println!("\n{} Shutting down host", "✓".bright_green().bold());
                }
                _ = async { let _ = server.await; } => {}
                _ = async { let _ = relay_task.await; } => {}
            }
        }
        Some(Commands::Startup) => match startup::install() {
            Ok(message) => println!("{} {message}", "✓".bright_green().bold()),
            Err(e) => {
                eprintln!("{} Startup install failed: {e}", "✗".red().bold());
                std::process::exit(1);
            }
        },
        None => {
            // Load runtime config (defaults if no --config supplied).
            let mut cfg = if let Some(ref path) = config_path {
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
            if remote {
                cfg.filesystem.enabled = true;
            }
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

            if remote {
                let folder = std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| ".".to_string());
                println!("{} Remote file system enabled", "✓".bright_green().bold());
                println!("IP: {ip}");
                println!("Port: {port}");
                println!("Folder: {folder}");
            }

            start_server(ip, port, allow_any_origin).await;
        }
    }
}
