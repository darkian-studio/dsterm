mod agent_bridge;
mod ai;
mod ai_bridge;
mod assets;
mod ast_bridge;
mod config;
mod dap_bridge;
mod extension_host_bridge;
mod fs;
mod lsp;
mod lsp_bridge;
mod mcp_bridge;
mod ports;
mod process_bridge;
mod proto_frame;
mod protocol;
mod providers;
mod proxy;
mod relay;
mod startup;
mod sysmon;
mod terminal;
mod transfer;
mod updates;
mod utils;
mod web_routes;
#[cfg(feature = "zim")]
mod zim;

use clap::{Parser, Subcommand};
use colored::Colorize;
use config::DstermConfig;
use lsp::{start_lsp_server, LspBridgeConfig};
use relay::{clients::ClientStore, crypto::Secretbox, pairing};
use std::net::Ipv4Addr;
use terminal::{init_config, notify_update_ready, set_default_command, start_server};
use updates::UpdateChecker;
use utils::get_ip_address;

const DEFAULT_PORT: u16 = 8767;
const LOCAL_IP: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 1);

#[derive(Parser)]
#[command(name = "dsterm", version, author = "Darkian Studio <darkian.studio@gmail.com>", about = "CLI/Server backend to serve pty over socket", long_about = None)]
struct Cli {
    #[arg(short, long, global = true, value_parser = clap::value_parser!(u16).range(1..))]
    port: Option<u16>,
    #[arg(short, long, global = true)]
    ip: bool,
    #[arg(short = 'c', long = "command")]
    command_override: Option<String>,
    #[arg(long = "allow-any-origin", global = true)]
    allow_any_origin: bool,
    #[arg(long = "config", global = true)]
    config_path: Option<String>,
    #[arg(long = "remote", global = true)]
    remote: bool,
    #[arg(long = "self-update")]
    self_update: bool,
    #[arg(long = "listen-transfer")]
    listen_transfer: bool,
    #[arg(long = "auto-receive")]
    auto_receive: bool,
    #[arg(long = "dest")]
    dest: Option<String>,
    #[arg(long = "overwrite")]
    overwrite: bool,
    #[arg(long = "rename")]
    rename: bool,
    #[arg(long = "allow-remote")]
    allow_remote: bool,
    #[arg(long = "confirm-timeout")]
    confirm_timeout: Option<u64>,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Update {
        #[command(subcommand)]
        action: Option<UpdateAction>,
    },
    Downgrade,
    Lsp {
        #[arg(short = 's', long)]
        session: Option<String>,
        server: String,
        #[arg(trailing_var_arg = true)]
        server_args: Vec<String>,
    },
    Pair {
        #[arg(long = "host-id")]
        host_id: Option<String>,
        #[arg(long = "no-qr")]
        no_qr: bool,
    },
    Clients {
        #[command(subcommand)]
        action: ClientsAction,
    },
    Register,
    Host,
    Startup,
    Transfer {
        path: String,
        endpoint: String,
        #[arg(long = "as")]
        as_name: Option<String>,
    },
}

#[derive(Subcommand)]
enum UpdateAction {
    /// Report the staged update candidate, if any
    /// (`<binary>.new` + sidecar, re-verified). Reads disk
    /// only — works whether or not the `update_ready` push
    /// was ever received.
    Status,
}

#[derive(Subcommand)]
enum ClientsAction {
    List,
    Approve { client_id: String },
    Reject { client_id: String },
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

/// `--self-update` worker: same launch-time check as
/// [check_updates_in_background], but a newer version is
/// downloaded, verified, and staged as `<binary>.new` (no
/// activation, no restart), then announced to open terminal
/// sockets. Errors are printed; the server keeps running
/// either way.
///
/// Repeats hourly: a launch-time-only check would never
/// notice releases published mid-life (long-running daemons),
/// which is the entire point of supervised updating. Each
/// round is cheap when nothing changed (one cached API read,
/// one disk probe).
async fn stage_updates_in_background() {
    loop {
        stage_updates_once().await;
        tokio::time::sleep(SELF_UPDATE_RECHECK).await;
    }
}

const SELF_UPDATE_RECHECK: std::time::Duration = std::time::Duration::from_secs(60 * 60);

async fn stage_updates_once() {
    let checker = UpdateChecker::new(env!("CARGO_PKG_VERSION"));
    // Deliberate double resolution: the cheap cached check
    // gates the expensive fetch, which re-resolves
    // authoritatively (a lot can change between cache write
    // and stage, and staging must never trust cache).
    let tag = match checker.check_update(true).await {
        Ok(Some(tag)) => tag,
        Ok(None) => return,
        Err(e) => {
            eprintln!(
                "{} {}",
                "⚠️".yellow(),
                format!(
                    "Failed to check for updates: {e}"
                ).red()
            );
            return;
        }
    };
    let wanted = tag.trim_start_matches('v');
    if let Some(staged) = UpdateChecker::staged_update().await {
        if staged.version == wanted {
            return;
        }
    }
    // The temporary from `fetch_update().await` holds a non
    // Send `Box<dyn Error>`; it must not live across the
    // `stage_update` await below (this future is
    // `tokio::spawn`ed, hence `Send`). Unwrap into an owned
    // `FetchedUpdate` first — errors return before any await.
    let fetched = match checker.fetch_update().await {
        Ok(fetched) => fetched,
        Err(e) => {
            eprintln!(
                "{} {}",
                "✗".red().bold(),
                format!("Failed to fetch update: {e}").red()
            );
            return;
        }
    };
    match UpdateChecker::stage_update(&fetched).await {
        Ok(staged) => {
            println!(
                "{} {} {}",
                "↓".bright_green().bold(),
                "Update staged:".green().bold(),
                staged.version.green()
            );
            notify_update_ready(&staged.version);
        }
        Err(e) => eprintln!(
            "{} {}",
            "✗".red().bold(),
            format!("Failed to stage update: {e}").red()
        ),
    }
}

// Self-update must finish before the transfer socket opens:
// replacing the binary mid-transfer leaves the outcome
// undefined on every platform.
async fn run_self_update_before_listen() {
    let checker = UpdateChecker::new(
        env!("CARGO_PKG_VERSION")
    );
    match checker.check_update(false).await {
        Ok(Some(_)) => {}
        Ok(None) => return,
        Err(e) => {
            eprintln!(
                "{} Failed to check for updates: {e}",
                "⚠️".yellow()
            );
            return;
        }
    }
    let fetched = match checker.fetch_update().await {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "{} Failed to fetch update: {e}",
                "✗".red().bold()
            );
            return;
        }
    };
    match UpdateChecker::stage_update(&fetched).await {
        Ok(staged) => println!(
            "{} {} {}",
            "↓".bright_green().bold(),
            "Update staged:".green().bold(),
            staged.version.green()
        ),
        Err(e) => eprintln!(
            "{} Failed to stage update: {e}",
            "✗".red().bold()
        ),
    }
}

fn load_config_or_default(path: Option<&str>, announce: bool) -> DstermConfig {
    if let Some(path) = path {
        match DstermConfig::load(path) {
            Ok(config) => {
                if announce {
                    println!(
                        "{} Config loaded from {}",
                        "✓".bright_green(), path
                    );
                }
                config
            }
            Err(e) => {
                eprintln!(
                    "{} Failed to load config from {path}: {e}",
                    "✗".red().bold()
                );
                std::process::exit(1);
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
        port: port_opt,
        ip,
        command_override,
        allow_any_origin,
        config_path,
        remote,
        self_update,
        listen_transfer,
        auto_receive,
        dest,
        overwrite,
        rename,
        allow_remote,
        confirm_timeout,
        command,
    } = cli;

    if auto_receive && !listen_transfer {
        eprintln!(
            "{} {}",
            "✗".red().bold(),
            "error: --auto-receive requires --listen-transfer".red()
        );
        std::process::exit(2);
    }
    if (overwrite || rename || dest.is_some() || allow_remote || confirm_timeout.is_some())
        && !listen_transfer
        && !matches!(command, Some(Commands::Transfer { .. }))
    {
        eprintln!(
            "{} {}",
            "✗".red().bold(),
            "transfer receiver flags require --listen-transfer".red()
        );
        std::process::exit(2);
    }
    if overwrite && rename {
        eprintln!(
            "{} {}",
            "✗".red().bold(),
            "--overwrite and --rename are mutually exclusive".red()
        );
        std::process::exit(2);
    }

    if self_update && command.is_some() {
        // clap can't express "flag conflicts with any
        // subcommand" here (subcommands are an open set), so
        // the launch-only scope of --self-update is enforced
        // at runtime instead of in the schema.
        eprintln!(
            "{} --self-update only applies to server mode (no subcommand).",
            "✗".red().bold()
        );
        std::process::exit(2);
    }

    match command {
        Some(Commands::Update { action }) => match action {
            Some(UpdateAction::Status) => {
                println!(
                    "{} {}",
                    "⟳".blue().bold(),
                    "Checking staged update...".blue()
                );
                match UpdateChecker::staged_update().await {
                    Some(staged) => {
                        println!(
                            "{} {} {} {}",
                            "↓".bright_green(),
                            "Staged update:".green(),
                            staged.version.green().bold(),
                            format!("({})",
                                staged.path.display()
                            ).bright_black(),
                        );
                        println!(
                            "  {}",
                            "Activate it with a supervisor restart (see --self-update docs)."
                                .bright_black(),
                        );
                    }
                    None => {
                        println!(
                            "{} {} {}",
                            "✓".bright_green().bold(),
                            "No staged update.".green(),
                            format!(
                                "(running {})",
                                env!("CARGO_PKG_VERSION")
                            ).bright_black(),
                        );
                    }
                }
            }
            None => {
                println!(
                    "{} {}",
                    "⟳".blue().bold(),
                    "Checking for updates...".blue()
                );

                let checker = UpdateChecker::new(
                    env!("CARGO_PKG_VERSION")
                );

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
        },
        Some(Commands::Downgrade) => {
            let current_exe = match std::env::current_exe() {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{} {e}", "✗".red().bold());
                    std::process::exit(1);
                }
            };
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

            // A running binary cannot be overwritten in place
            // on Windows, so swap via a sibling: stash the
            // running exe, drop the old one in, then remove
            // the stash. On Unix an atomic rename over the
            // running binary is fine.
            #[cfg(windows)]
            {
                let stash_path = current_exe
                    .with_extension("disabled");
                let _ = tokio::fs::remove_file(
                    &stash_path
                ).await;
                if let Err(e) = tokio::fs::rename(
                    &current_exe, &stash_path
                ).await {
                    eprintln!("{} {e}", "✗".red().bold());
                    std::process::exit(1);
                }
                if let Err(e) = tokio::fs::rename(
                    &old_path, &current_exe
                ).await {
                    eprintln!("{} {e}", "✗".red().bold());
                    std::process::exit(1);
                }
                let _ = tokio::fs::remove_file(
                    &stash_path
                ).await;
            }
            #[cfg(not(windows))]
            {
                if let Err(e) = tokio::fs::rename(
                    &old_path, &current_exe
                ).await {
                    eprintln!("{} {e}", "✗".red().bold());
                    std::process::exit(1);
                }
            }

            println!(
                "\n{} {}",
                "✓".bright_green().bold(),
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

            let lsp_port = port_opt;

            start_lsp_server(
                host,
                lsp_port,
                session,
                allow_any_origin,
                config
            ).await;
        }
        Some(Commands::Pair { host_id, no_qr }) => {
            let cfg = load_config_or_default(
                config_path.as_deref(), false
            );
            let secretbox = match Secretbox::load_or_create(
                cfg.security.key_file.as_deref()
            ) {
                Ok(secretbox) => secretbox,
                Err(e) => {
                    eprintln!(
                        "{} Failed to load/create E2E key: {e}",
                        "✗".red().bold()
                    );
                    std::process::exit(1);
                }
            };
            let host_id = match pairing::resolve_host_id(
                host_id.as_deref(),
                cfg.relay.host_id_file.as_deref(),
            ) {
                Ok(host_id) => host_id,
                Err(e) => {
                    eprintln!(
                        "{} Failed to resolve host id: {e}",
                        "✗".red().bold()
                    );
                    std::process::exit(1);
                }
            };
            let payload = match pairing::PairingPayload::new(
                host_id, secretbox.key_base64()
            ) {
                Ok(payload) => payload,
                Err(e) => {
                    eprintln!(
                        "{} Failed to build pairing payload: {e}",
                        "✗".red().bold()
                    );
                    std::process::exit(1);
                }
            };

            if !no_qr {
                match pairing::render_qr(&payload) {
                    Ok(qr) => println!("{qr}"),
                    Err(e) => {
                        eprintln!(
                            "{} Failed to render QR: {e}",
                            "✗".red().bold()
                        );
                        std::process::exit(1);
                    }
                }
            }
            println!("{}", payload.qr_text());
        }
        Some(Commands::Clients { action }) => {
            let cfg = load_config_or_default(
                config_path.as_deref(), false
            );
            let mut store = match ClientStore::load_or_default(
                cfg.security.clients_file.as_deref()
            ){
                Ok(store) => store,
                Err(e) => {
                    eprintln!(
                        "{} Failed to load clients store: {e}",
                        "✗".red().bold()
                    );
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
                                record.platform.as_deref()
                                    .unwrap_or("-"),
                                record.app_version.as_deref()
                                    .unwrap_or("-"),
                            );
                        }
                    }
                }
                ClientsAction::Approve { client_id } => match store.approve(&client_id) {
                    Ok(()) => println!(
                        "{} Approved {client_id}",
                        "✓".bright_green().bold()
                    ),
                    Err(e) => {
                        eprintln!("{} {e}", "✗".red().bold());
                        std::process::exit(1);
                    }
                },
                ClientsAction::Reject { client_id } => match store.reject(&client_id) {
                    Ok(()) => println!(
                        "{} Rejected {client_id}",
                        "✓".bright_green().bold()
                    ),
                    Err(e) => {
                        eprintln!("{} {e}", "✗".red().bold());
                        std::process::exit(1);
                    }
                },
            }
        }
        Some(Commands::Register) => {
            let cfg = load_config_or_default(
                config_path.as_deref(), false
            );
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
                    println!(
                        "{} Registered host: {host_id}",
                        "✓".bright_green().bold()
                    );
                }
                Err(e) => {
                    eprintln!(
                        "{} Registration failed: {e}",
                        "✗".red().bold()
                    );
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::Transfer {
            path,
            endpoint,
            as_name,
        }) => {
            let port_default = crate::transfer::DEFAULT_TRANSFER_PORT;
            let _ = port_default;
            let ep = match crate::transfer::parse_endpoint(&endpoint) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("{} {e}", "✗".red().bold());
                    std::process::exit(2);
                }
            };
            let opts = crate::transfer::SenderOptions {
                source: std::path::PathBuf::from(path),
                endpoint: ep,
                dest_hint: as_name,
            };
            if let Err(e) = crate::transfer::run_sender(opts).await {
                eprintln!("{} {e}", "✗".red().bold());
                std::process::exit(1);
            }
        }
        Some(Commands::Host) => {
            let port = port_opt.unwrap_or(DEFAULT_PORT);
            let mut cfg = load_config_or_default(
                config_path.as_deref(), true
            );
            cfg.apply_remote_flag(remote);
            init_config(cfg.clone());

            if let Some(cmd) = command_override {
                set_default_command(cmd);
            }

            let secretbox = match Secretbox::load_or_create(
                cfg.security.key_file.as_deref()
            ) {
                Ok(sb) => sb,
                Err(e) => {
                    eprintln!(
                        "{} Failed to load/create E2E key: {e}",
                        "✗".red().bold()
                    );
                    std::process::exit(1);
                }
            };

            let host_id = match relay::register::read_cached(
                cfg.relay.host_id_file.as_deref()
            ) {
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
                start_server(
                    LOCAL_IP,
                    port,
                    allow_any_origin
                ).await;
            });
            tokio::time::sleep(
                std::time::Duration::from_millis(300)
            ).await;
            let relay_task = tokio::spawn(async move {
                relay::transport::run(
                    cfg,
                    secretbox,
                    host_id,
                    port
                ).await;
            });

            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    println!(
                        "\n{} Shutting down host",
                        "✓".bright_green().bold()
                    );
                }
                _ = async { let _ = server.await; } => {}
                _ = async { let _ = relay_task.await; } => {}
            }
        }
        Some(Commands::Startup) => match startup::install() {
            Ok(message) => println!(
                "{} {message}",
                "✓".bright_green().bold()
            ),
            Err(e) => {
                eprintln!(
                    "{} Startup install failed: {e}",
                    "✗".red().bold()
                );
                std::process::exit(1);
            }
        },
        None => {
            if listen_transfer {
                let transfer_port = port_opt.unwrap_or(
                    crate::transfer::DEFAULT_TRANSFER_PORT
                );
                if self_update {
                    run_self_update_before_listen().await;
                }
                let opts = crate::transfer::ReceiverOptions {
                    port: transfer_port,
                    auto_receive,
                    dest,
                    overwrite,
                    rename,
                    allow_remote,
                    confirm_timeout_secs: confirm_timeout
                        .unwrap_or(crate::transfer::DEFAULT_CONFIRM_TIMEOUT_SECS),
                    expose: ip,
                };
                if let Err(e) = crate::transfer::run_listener(opts).await {
                    eprintln!("{} {e}", "✗".red().bold());
                    std::process::exit(1);
                }
                return;
            }
            if auto_receive
                || dest.is_some()
                || overwrite
                || rename
                || allow_remote
                || confirm_timeout.is_some()
            {
                eprintln!(
                    "{} transfer receiver flags require --listen-transfer",
                    "✗".red().bold()
                );
                std::process::exit(2);
            }
            let port = port_opt.unwrap_or(DEFAULT_PORT);
            let mut cfg = if let Some(ref path) = config_path {
                match DstermConfig::load(path) {
                    Ok(c) => {
                        println!(
                            "{} Config loaded from {}",
                            "✓".bright_green(), path
                        );
                        c
                    }
                    Err(e) => {
                        eprintln!(
                            "{} Failed to load config from {path}: {e}",
                            "✗".red().bold()
                        );
                        std::process::exit(1);
                    }
                }
            } else {
                DstermConfig::default()
            };
            cfg.apply_remote_flag(remote);
            init_config(cfg);

            if self_update {
                tokio::task::spawn(
                    stage_updates_in_background()
                );
            } else {
                tokio::task::spawn(
                    check_updates_in_background()
                );
            }

            if let Some(cmd) = command_override {
                set_default_command(cmd);
            }

            let lan_requested = ip;
            let ip = if lan_requested {
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
                println!(
                    "{} Remote file system enabled",
                    "✓".bright_green().bold()
                );
                println!("IP: {ip}");
                println!("Port: {port}");
                println!("Folder: {folder}");
                if !lan_requested {
                    eprintln!(
                        "{} Remote file system is enabled but server is bound to 127.0.0.1 (localhost-only). Add -i to expose on LAN or use `dsterm host --remote` for Internet via relay.",
                        "⚠".yellow()
                    );
                }
            }

            start_server(ip, port, allow_any_origin).await;
        }
    }
}
