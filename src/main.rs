//! CoreNet - Cross-Host I/O Device Sharing
//!
//! A software KVM solution for sharing mouse and keyboard across multiple computers.

mod config;
mod discovery;
mod input;
mod network;
mod protocol;
mod screen;

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use config::Config;
use network::{Client, NetworkConfig as NetConfig, Server, ServerEvent, ClientEvent};
use protocol::ScreenInfo;
use screen::{get_screen_dimensions, EdgeDetector, EdgeDetectorConfig, ScreenLayout};

/// CoreNet - Cross-host I/O device sharing
#[derive(Parser)]
#[command(name = "corenet")]
#[command(author = "CoreNet Contributors")]
#[command(version = "0.1.0")]
#[command(about = "Share mouse and keyboard across multiple computers", long_about = None)]
struct Cli {
    /// Path to configuration file
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run as server (primary host with physical mouse/keyboard)
    Server {
        /// Port to listen on
        #[arg(short, long, default_value_t = protocol::DEFAULT_PORT)]
        port: u16,

        /// Host name to advertise
        #[arg(short, long)]
        name: Option<String>,

        /// Disable TLS (not recommended)
        #[arg(long)]
        no_tls: bool,
    },

    /// Run as client (connect to a server)
    Client {
        /// Server address to connect to
        #[arg(short, long)]
        server: Option<String>,

        /// Server port
        #[arg(short, long, default_value_t = protocol::DEFAULT_PORT)]
        port: u16,

        /// Use auto-discovery to find servers
        #[arg(short, long)]
        discover: bool,
    },

    /// Show current configuration
    Config {
        /// Generate sample configuration
        #[arg(long)]
        generate: bool,

        /// Output path for generated config
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Discover hosts on the network
    Discover {
        /// How long to scan (seconds)
        #[arg(short, long, default_value_t = 5)]
        timeout: u64,
    },

    /// Show system information
    Info,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("info")
    };

    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(filter)
        .init();

    // Load configuration
    let config = if let Some(config_path) = &cli.config {
        Config::load(config_path)?
    } else {
        Config::load_default().unwrap_or_default()
    };

    match cli.command {
        Commands::Server { port, name, no_tls } => {
            run_server(config, port, name, !no_tls).await?;
        }
        Commands::Client {
            server,
            port,
            discover,
        } => {
            run_client(config, server, port, discover).await?;
        }
        Commands::Config { generate, output } => {
            if generate {
                let sample = config::generate_sample_config();
                if let Some(path) = output {
                    std::fs::write(&path, &sample)?;
                    println!("Configuration written to: {}", path.display());
                } else {
                    println!("{}", sample);
                }
            } else {
                println!("{}", toml::to_string_pretty(&config)?);
            }
        }
        Commands::Discover { timeout } => {
            run_discovery(timeout).await?;
        }
        Commands::Info => {
            print_system_info();
        }
    }

    Ok(())
}

/// Run the server (primary host)
async fn run_server(
    config: Config,
    port: u16,
    name: Option<String>,
    _use_tls: bool,
) -> anyhow::Result<()> {
    let (width, height) = get_screen_dimensions();
    
    let screen_info = ScreenInfo::new(
        config.host_id(),
        name.unwrap_or(config.general.name.clone()),
        config.screen.width.unwrap_or(width),
        config.screen.height.unwrap_or(height),
    );

    tracing::info!(
        "Starting CoreNet server '{}' on port {}",
        screen_info.host_name,
        port
    );
    tracing::info!("Screen: {}x{}", screen_info.width, screen_info.height);

    let net_config = NetConfig::new(port);
    let mut server = Server::new(net_config, screen_info.clone());
    
    let mut event_rx = server.take_event_receiver().unwrap();

    // Start the server
    server.start().await?;

    // Set up edge detection
    let edge_config = EdgeDetectorConfig {
        edge_margin: config.screen.edge_margin,
        dwell_time_ms: config.screen.dwell_time_ms,
        require_double_tap: config.screen.require_double_tap,
        ..Default::default()
    };
    let mut _edge_detector = EdgeDetector::new(edge_config, screen_info.width, screen_info.height);

    // Set up screen layout
    let mut layout = ScreenLayout::new();
    layout.set_local_host(&screen_info);

    println!("\n========================================");
    println!("  CoreNet Server Running");
    println!("========================================");
    println!("  Host: {}", screen_info.host_name);
    println!("  Port: {}", port);
    println!("  Screen: {}x{}", screen_info.width, screen_info.height);
    println!("========================================");
    println!("\nWaiting for clients to connect...");
    println!("Press Ctrl+C to stop.\n");

    // Main event loop
    loop {
        tokio::select! {
            Some(event) = event_rx.recv() => {
                match event {
                    ServerEvent::ClientConnected { addr, screen_info: client_screen } => {
                        tracing::info!(
                            "Client connected: {} ({}) - {}x{}",
                            client_screen.host_name,
                            addr,
                            client_screen.width,
                            client_screen.height
                        );
                        
                        // Add to layout
                        layout.add_host(&client_screen);
                        
                        println!("+ Client connected: {} ({})", client_screen.host_name, addr);
                    }
                    ServerEvent::ClientDisconnected { addr, reason } => {
                        tracing::info!("Client disconnected: {} - {}", addr, reason);
                        println!("- Client disconnected: {} ({})", addr, reason);
                    }
                    ServerEvent::MessageReceived { addr, message } => {
                        tracing::debug!("Message from {}: {:?}", addr, message);
                        // Handle input events, clipboard, etc.
                    }
                    ServerEvent::Error { message } => {
                        tracing::error!("Server error: {}", message);
                    }
                    _ => {}
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("\nShutting down...");
                break;
            }
        }
    }

    server.stop().await?;
    tracing::info!("Server stopped");

    Ok(())
}

/// Run the client (secondary host)
async fn run_client(
    config: Config,
    server_addr: Option<String>,
    port: u16,
    discover: bool,
) -> anyhow::Result<()> {
    let (width, height) = get_screen_dimensions();
    
    let screen_info = ScreenInfo::new(
        config.host_id(),
        config.general.name.clone(),
        config.screen.width.unwrap_or(width),
        config.screen.height.unwrap_or(height),
    );

    let server_socket_addr = if let Some(addr) = server_addr {
        let addr: SocketAddr = if addr.contains(':') {
            addr.parse()?
        } else {
            format!("{}:{}", addr, port).parse()?
        };
        addr
    } else if discover {
        // Run discovery
        println!("Discovering CoreNet servers...");
        // In real implementation, use mDNS discovery
        // For now, fail with helpful message
        anyhow::bail!("Auto-discovery not yet implemented. Please specify --server address.");
    } else {
        anyhow::bail!("Please specify --server address or use --discover");
    };

    tracing::info!(
        "Connecting to server at {} as '{}'",
        server_socket_addr,
        screen_info.host_name
    );

    let net_config = NetConfig::new(port);
    let mut client = Client::new(net_config, screen_info.clone());
    
    let mut event_rx = client.take_event_receiver().unwrap();

    // Connect to server
    println!("Connecting to {}...", server_socket_addr);
    client.connect(server_socket_addr).await?;

    println!("\n========================================");
    println!("  CoreNet Client Connected");
    println!("========================================");
    println!("  Local: {}", screen_info.host_name);
    println!("  Server: {}", server_socket_addr);
    println!("  Screen: {}x{}", screen_info.width, screen_info.height);
    println!("========================================");
    println!("\nReceiving input from server...");
    println!("Press Ctrl+C to disconnect.\n");

    // Main event loop
    loop {
        tokio::select! {
            Some(event) = event_rx.recv() => {
                match event {
                    ClientEvent::Connected { server_addr, server_screen } => {
                        tracing::info!(
                            "Connected to server: {} ({}) - {}x{}",
                            server_screen.host_name,
                            server_addr,
                            server_screen.width,
                            server_screen.height
                        );
                    }
                    ClientEvent::Disconnected { reason } => {
                        tracing::info!("Disconnected: {}", reason);
                        println!("Disconnected: {}", reason);
                        break;
                    }
                    ClientEvent::MessageReceived { message } => {
                        tracing::debug!("Message: {:?}", message);
                        // Handle input injection, clipboard, etc.
                        match message {
                            protocol::Message::MouseMoveRelative { dx, dy } => {
                                tracing::debug!("Mouse move: dx={}, dy={}", dx, dy);
                                // Inject mouse movement
                            }
                            protocol::Message::KeyDown { keycode, .. } => {
                                tracing::debug!("Key down: {:#x}", keycode);
                                // Inject key press
                            }
                            _ => {}
                        }
                    }
                    ClientEvent::Error { message } => {
                        tracing::error!("Client error: {}", message);
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("\nDisconnecting...");
                break;
            }
        }
    }

    client.disconnect().await?;
    tracing::info!("Client disconnected");

    Ok(())
}

/// Run host discovery
async fn run_discovery(timeout_secs: u64) -> anyhow::Result<()> {
    println!("Scanning for CoreNet hosts ({} seconds)...\n", timeout_secs);

    // In real implementation, use mDNS discovery
    // For now, show a placeholder message
    
    tokio::time::sleep(std::time::Duration::from_secs(timeout_secs)).await;
    
    println!("Discovery complete.");
    println!("\nNote: mDNS discovery requires the mdns-sd crate to be fully integrated.");
    println!("For now, please use --server to specify the host address manually.");

    Ok(())
}

/// Print system information
fn print_system_info() {
    let (width, height) = get_screen_dimensions();
    
    println!("CoreNet System Information");
    println!("==========================\n");
    
    println!("Platform: {}", input::platform_name());
    println!("Screen: {}x{}", width, height);
    
    #[cfg(target_os = "macos")]
    {
        println!("\nmacOS Requirements:");
        println!("  - Accessibility permissions required");
        println!("  - System Preferences > Security & Privacy > Privacy > Accessibility");
    }
    
    #[cfg(target_os = "linux")]
    {
        println!("\nLinux Requirements:");
        println!("  - User must be in 'input' group: sudo usermod -aG input $USER");
        println!("  - uinput module must be loaded: sudo modprobe uinput");
    }
    
    #[cfg(target_os = "windows")]
    {
        println!("\nWindows Requirements:");
        println!("  - May require running as Administrator for global hooks");
    }
    
    println!("\nProtocol Version: {}", protocol::PROTOCOL_VERSION);
    println!("Default Port: {}", protocol::DEFAULT_PORT);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parsing() {
        // Test that CLI parsing works
        let cli = Cli::try_parse_from(["corenet", "info"]);
        assert!(cli.is_ok());
    }
}
