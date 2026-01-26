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
use std::sync::Arc;
use tokio::sync::RwLock;

use clap::{Parser, Subcommand};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use config::Config;
use input::{InputCapture, InputEvent, InputInjector};
use network::{Client, ClientEvent, NetworkConfig as NetConfig, Server, ServerEvent};
use protocol::{Message, ScreenEdge, ScreenInfo};
use screen::{get_screen_dimensions, EdgeDetectResult, EdgeDetector, EdgeDetectorConfig, ScreenLayout};

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

/// Create platform-specific input capture
#[cfg(target_os = "linux")]
fn create_input_capture() -> Box<dyn InputCapture> {
    Box::new(input::LinuxInputCapture::new())
}

#[cfg(target_os = "macos")]
fn create_input_capture() -> Box<dyn InputCapture> {
    Box::new(input::MacOSInputCapture::new())
}

#[cfg(target_os = "windows")]
fn create_input_capture() -> Box<dyn InputCapture> {
    Box::new(input::WindowsInputCapture::new())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn create_input_capture() -> Box<dyn InputCapture> {
    panic!("Unsupported platform for input capture");
}

/// Create platform-specific input injector
#[cfg(target_os = "linux")]
fn create_input_injector() -> Box<dyn InputInjector> {
    Box::new(input::LinuxInputInjector::new())
}

#[cfg(target_os = "macos")]
fn create_input_injector() -> Box<dyn InputInjector> {
    Box::new(input::MacOSInputInjector::new())
}

#[cfg(target_os = "windows")]
fn create_input_injector() -> Box<dyn InputInjector> {
    Box::new(input::WindowsInputInjector::new())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn create_input_injector() -> Box<dyn InputInjector> {
    panic!("Unsupported platform for input injection");
}

/// Convert InputEvent to protocol Message
fn input_event_to_message(event: &InputEvent) -> Option<Message> {
    match event {
        InputEvent::MouseMove(e) => Some(Message::MouseMoveRelative { dx: e.dx, dy: e.dy }),
        InputEvent::MouseButton(e) => Some(Message::MouseButton {
            button: e.button,
            pressed: e.pressed,
        }),
        InputEvent::MouseScroll(e) => Some(Message::MouseScroll { dx: e.dx, dy: e.dy }),
        InputEvent::Keyboard(e) => {
            if e.pressed {
                Some(Message::KeyDown {
                    keycode: e.keycode,
                    character: e.character,
                    modifiers: e.modifiers,
                })
            } else {
                Some(Message::KeyUp {
                    keycode: e.keycode,
                    modifiers: e.modifiers,
                })
            }
        }
    }
}

/// State for tracking which host has control
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlState {
    /// Input is going to local machine
    Local,
    /// Input is being sent to a remote host
    Remote(usize), // Index of the client
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

    // Set up input capture
    let mut input_capture = create_input_capture();
    let mut input_rx = match input_capture.start().await {
        Ok(rx) => rx,
        Err(e) => {
            tracing::error!("Failed to start input capture: {}", e);
            println!("Error: {}. Some features may not work.", e);
            // Continue without input capture for testing
            let (_, rx) = tokio::sync::mpsc::channel(1);
            rx
        }
    };

    // Set up edge detection
    let edge_config = EdgeDetectorConfig {
        edge_margin: config.screen.edge_margin,
        dwell_time_ms: config.screen.dwell_time_ms,
        require_double_tap: config.screen.require_double_tap,
        ..Default::default()
    };
    let mut edge_detector = EdgeDetector::new(edge_config, screen_info.width, screen_info.height);

    // Set up screen layout
    let layout = Arc::new(RwLock::new(ScreenLayout::new()));
    {
        let mut layout_guard = layout.write().await;
        layout_guard.set_local_host(&screen_info);
    }

    // Track connected clients and control state
    let mut clients: Vec<(SocketAddr, ScreenInfo)> = Vec::new();
    let mut control_state = ControlState::Local;

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
            // Handle network events
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

                        // Add to layout (to the right by default)
                        {
                            let mut layout_guard = layout.write().await;
                            layout_guard.add_host(&client_screen);
                        }

                        clients.push((addr, client_screen.clone()));
                        println!("+ Client connected: {} ({})", client_screen.host_name, addr);
                        println!("  Clients: {}", clients.len());
                    }
                    ServerEvent::ClientDisconnected { addr, reason } => {
                        tracing::info!("Client disconnected: {} - {}", addr, reason);
                        
                        // Remove from client list
                        if let Some(idx) = clients.iter().position(|(a, _)| *a == addr) {
                            let (_, screen) = clients.remove(idx);
                            
                            // If we were sending to this client, return to local
                            if control_state == ControlState::Remote(idx) {
                                control_state = ControlState::Local;
                                input_capture.set_suppress(false);
                            }
                            
                            // Remove from layout
                            {
                                let mut layout_guard = layout.write().await;
                                layout_guard.remove_host(&screen.host_id);
                            }
                        }
                        
                        println!("- Client disconnected: {} ({})", addr, reason);
                    }
                    ServerEvent::MessageReceived { addr, message } => {
                        tracing::debug!("Message from {}: {:?}", addr, message);
                        
                        // Handle messages from clients (e.g., when cursor returns)
                        match message {
                            Message::LeaveScreen { edge, position } => {
                                // Client is returning control to us
                                tracing::info!("Cursor returning from client via {:?} edge", edge);
                                control_state = ControlState::Local;
                                input_capture.set_suppress(false);
                                
                                // Move cursor to the appropriate position
                                let entry_edge = screen::opposite_edge(edge);
                                let (x, y) = screen::denormalize_edge_position(
                                    entry_edge,
                                    position,
                                    screen_info.width,
                                    screen_info.height,
                                );
                                tracing::debug!("Placing cursor at ({}, {})", x, y);
                            }
                            _ => {}
                        }
                    }
                    ServerEvent::Error { message } => {
                        tracing::error!("Server error: {}", message);
                    }
                    _ => {}
                }
            }
            
            // Handle input events
            Some(input_event) = input_rx.recv() => {
                match control_state {
                    ControlState::Local => {
                        // Check for edge detection
                        if let InputEvent::MouseMove(ref move_event) = input_event {
                            if let (Some(x), Some(y)) = (move_event.x, move_event.y) {
                                let result = edge_detector.check(x, y);
                                
                                match result {
                                    EdgeDetectResult::Transition { edge, position } => {
                                        // Check if there's a client on this edge
                                        if !clients.is_empty() {
                                            // For now, just use the first client for right edge
                                            if edge == ScreenEdge::Right && !clients.is_empty() {
                                                let client_idx = 0;
                                                let (client_addr, ref client_screen) = clients[client_idx];
                                                
                                                tracing::info!(
                                                    "Transitioning to {} via {:?} edge at position {}",
                                                    client_screen.host_name,
                                                    edge,
                                                    position
                                                );
                                                
                                                // Send enter screen message
                                                let _ = server.send_to(&client_addr, Message::EnterScreen {
                                                    edge: screen::opposite_edge(edge),
                                                    position,
                                                }).await;
                                                
                                                control_state = ControlState::Remote(client_idx);
                                                input_capture.set_suppress(true);
                                                edge_detector.reset();
                                                
                                                println!("-> Cursor moved to: {}", client_screen.host_name);
                                            }
                                        }
                                    }
                                    EdgeDetectResult::Dwelling { edge, remaining_ms } => {
                                        tracing::trace!("Dwelling at {:?} edge, {}ms remaining", edge, remaining_ms);
                                    }
                                    EdgeDetectResult::NotAtEdge => {}
                                }
                            }
                        }
                    }
                    ControlState::Remote(client_idx) => {
                        // Send input to the remote client
                        if client_idx < clients.len() {
                            let (client_addr, _) = &clients[client_idx];
                            
                            if let Some(message) = input_event_to_message(&input_event) {
                                let _ = server.send_to(client_addr, message).await;
                            }
                        } else {
                            // Client no longer exists, return to local
                            control_state = ControlState::Local;
                            input_capture.set_suppress(false);
                        }
                    }
                }
            }
            
            // Handle Ctrl+C
            _ = tokio::signal::ctrl_c() => {
                println!("\nShutting down...");
                break;
            }
        }
    }

    input_capture.stop().await?;
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
        println!("Discovering CoreNet servers...");
        anyhow::bail!("Auto-discovery not yet fully implemented. Please specify --server address.");
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

    // Set up input injector
    let mut input_injector = create_input_injector();
    if let Err(e) = input_injector.init().await {
        tracing::error!("Failed to initialize input injector: {}", e);
        println!("Warning: Input injection may not work: {}", e);
    }

    // Set up edge detection (for returning to server)
    let edge_config = EdgeDetectorConfig {
        edge_margin: config.screen.edge_margin,
        dwell_time_ms: config.screen.dwell_time_ms,
        ..Default::default()
    };
    let mut edge_detector = EdgeDetector::new(edge_config, screen_info.width, screen_info.height);

    // Track if we have control
    let mut has_control = false;
    let mut entry_edge = ScreenEdge::Left;
    let mut mouse_x: i32 = 0;
    let mut mouse_y: i32 = 0;

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
                        match message {
                            Message::EnterScreen { edge, position } => {
                                tracing::info!("Cursor entered via {:?} edge at position {}", edge, position);
                                has_control = true;
                                entry_edge = edge;
                                
                                // Position cursor at entry point
                                let (x, y) = screen::denormalize_edge_position(
                                    edge,
                                    position,
                                    screen_info.width,
                                    screen_info.height,
                                );
                                mouse_x = x;
                                mouse_y = y;
                                
                                if let Err(e) = input_injector.mouse_move_absolute(x, y).await {
                                    tracing::warn!("Failed to position cursor: {}", e);
                                }
                                
                                edge_detector.reset();
                                println!("<- Cursor entered from server");
                            }
                            
                            Message::MouseMoveRelative { dx, dy } if has_control => {
                                mouse_x += dx;
                                mouse_y += dy;
                                
                                // Clamp to screen bounds
                                mouse_x = mouse_x.clamp(0, screen_info.width as i32 - 1);
                                mouse_y = mouse_y.clamp(0, screen_info.height as i32 - 1);
                                
                                if let Err(e) = input_injector.mouse_move_relative(dx, dy).await {
                                    tracing::warn!("Failed to move mouse: {}", e);
                                }
                                
                                // Check if we should return to server
                                let result = edge_detector.check(mouse_x, mouse_y);
                                if let EdgeDetectResult::Transition { edge, position } = result {
                                    // Check if this is the return edge
                                    if edge == entry_edge {
                                        tracing::info!("Returning to server via {:?} edge", edge);
                                        has_control = false;
                                        
                                        // Send leave message
                                        let _ = client.send(Message::LeaveScreen {
                                            edge,
                                            position,
                                        }).await;
                                        
                                        edge_detector.reset();
                                        println!("-> Cursor returned to server");
                                    }
                                }
                            }
                            
                            Message::MouseMoveAbsolute { x, y } if has_control => {
                                mouse_x = x;
                                mouse_y = y;
                                if let Err(e) = input_injector.mouse_move_absolute(x, y).await {
                                    tracing::warn!("Failed to move mouse: {}", e);
                                }
                            }
                            
                            Message::MouseButton { button, pressed } if has_control => {
                                if let Err(e) = input_injector.mouse_button(button, pressed).await {
                                    tracing::warn!("Failed to inject mouse button: {}", e);
                                }
                            }
                            
                            Message::MouseScroll { dx, dy } if has_control => {
                                if let Err(e) = input_injector.mouse_scroll(dx, dy).await {
                                    tracing::warn!("Failed to inject scroll: {}", e);
                                }
                            }
                            
                            Message::KeyDown { keycode, modifiers, .. } if has_control => {
                                if let Err(e) = input_injector.key_down(keycode, modifiers).await {
                                    tracing::warn!("Failed to inject key down: {}", e);
                                }
                            }
                            
                            Message::KeyUp { keycode, modifiers } if has_control => {
                                if let Err(e) = input_injector.key_up(keycode, modifiers).await {
                                    tracing::warn!("Failed to inject key up: {}", e);
                                }
                            }
                            
                            _ => {
                                tracing::debug!("Received message (has_control={}): {:?}", has_control, message);
                            }
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

    input_injector.shutdown().await?;
    client.disconnect().await?;
    tracing::info!("Client disconnected");

    Ok(())
}

/// Run host discovery
async fn run_discovery(timeout_secs: u64) -> anyhow::Result<()> {
    println!("Scanning for CoreNet hosts ({} seconds)...\n", timeout_secs);

    // Use mDNS discovery
    use mdns_sd::{ServiceDaemon, ServiceEvent};
    
    let mdns = ServiceDaemon::new()?;
    let receiver = mdns.browse("_corenet._tcp.local.")?;
    
    let mut found_hosts = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    
    println!("Searching for CoreNet services...\n");
    
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                break;
            }
            event = tokio::task::spawn_blocking({
                let receiver = receiver.clone();
                move || receiver.recv_timeout(std::time::Duration::from_millis(100))
            }) => {
                if let Ok(Ok(event)) = event {
                    match event {
                        ServiceEvent::ServiceResolved(info) => {
                            println!("Found: {}", info.get_fullname());
                            println!("  Addresses: {:?}", info.get_addresses());
                            println!("  Port: {}", info.get_port());
                            println!();
                            
                            found_hosts.push((
                                info.get_fullname().to_string(),
                                info.get_addresses().iter().next().copied(),
                                info.get_port(),
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    
    mdns.shutdown()?;
    
    println!("Discovery complete. Found {} host(s).", found_hosts.len());
    
    if found_hosts.is_empty() {
        println!("\nNo CoreNet servers found on the network.");
        println!("Make sure a server is running: corenet server");
    }

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

        if input::MacOSInputCapture::has_accessibility_permission() {
            println!("  - Status: GRANTED");
        } else {
            println!("  - Status: NOT GRANTED");
        }
    }

    #[cfg(target_os = "linux")]
    {
        println!("\nLinux Requirements:");
        println!("  - User must be in 'input' group: sudo usermod -aG input $USER");
        println!("  - uinput module must be loaded: sudo modprobe uinput");

        if input::LinuxInputCapture::has_permission() {
            println!("  - Input device access: OK");
        } else {
            println!("  - Input device access: DENIED");
        }

        if input::LinuxInputInjector::is_uinput_available() {
            println!("  - uinput available: YES");
        } else {
            println!("  - uinput available: NO");
        }
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
        let cli = Cli::try_parse_from(["corenet", "info"]);
        assert!(cli.is_ok());
    }
}
