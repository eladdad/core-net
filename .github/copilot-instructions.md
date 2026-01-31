# CoreNet - Cross-Host I/O Device Sharing

## Project Overview

CoreNet is a software KVM (Keyboard, Video, Mouse) solution that allows seamless sharing of input devices across multiple computers on a network. It enables users to move their cursor across screen boundaries to switch between hosts as if they had multiple monitors connected to one machine.

## Architecture

The system consists of several key modules:

1. **Input Module** (`src/input/`) - Platform-specific input capture and injection
   - Captures local input events (mouse, keyboard)
   - Injects remote input events into the local system
   - Implements platform-specific APIs:
     - macOS: Quartz Event Services (CGEventTap)
     - Linux: libevdev / uinput
     - Windows: Raw Input API / Low-level hooks

2. **Network Module** (`src/network/`) - TCP/TLS communication
   - Server for accepting incoming connections
   - Client for connecting to servers
   - Connection management and message routing
   - Uses TLS encryption for security

3. **Protocol Module** (`src/protocol/`) - Binary communication protocol
   - Custom binary format for efficiency
   - Message types for mouse/keyboard events, screen info, clipboard data
   - Supports seamless cursor transitions between hosts

4. **Discovery Module** (`src/discovery/`) - mDNS-based host discovery
   - Auto-discovery of hosts on the same network
   - Uses DNS-SD service type `_corenet._tcp.local.`

5. **Screen Module** (`src/screen/`) - Screen edge detection and layout
   - Detects when cursor hits screen edges
   - Manages screen layouts and transitions
   - Handles coordinate transformations

## Key Features

- **Seamless Mouse Transition**: Move cursor across screen boundaries to switch between hosts
- **Keyboard Sharing**: Type on any connected host using your primary keyboard
- **Clipboard Sync**: Copy on one machine, paste on another (optional)
- **Cross-Platform**: Supports macOS, Linux, and Windows
- **Encrypted Communication**: All traffic is encrypted using TLS
- **Zero Configuration**: Auto-discovery of hosts on the same network using mDNS

## Build and Run

```bash
# Build with Cargo
cargo build --release

# Run as server (primary host)
corenet server --config config.toml

# Run as client (connect to a server)
corenet client --server 192.168.1.100

# Auto-discovery
corenet client --discover
```

## Configuration

Configuration is handled through a TOML file with sections for:
- General settings (host name, logging)
- Screen settings (dimensions, edge detection)
- Network settings (port, timeouts)
- Neighbor configuration (which hosts are adjacent)
- Security settings (TLS certificates)
- Clipboard settings

## Platform-Specific Implementation Details

### macOS
- Uses Quartz Event Services (CGEventTap) for input capture
- Uses CGEventPost for input injection
- Requires Accessibility permissions

### Linux
- Uses libevdev for input capture
- Uses uinput for input injection
- Requires user to be in 'input' group

### Windows
- Uses Raw Input API for input capture
- Uses SendInput API for input injection
- May require running as Administrator for global hooks

## Protocol Format

Messages use a simple binary format:
- 1 byte message type
- 4 bytes payload length (big-endian)
- 4 bytes sequence number (big-endian)
- Variable length payload

Message types include:
- Mouse movement (relative/absolute)
- Mouse button events
- Keyboard events
- Screen information
- Clipboard data
- Heartbeat for connection keep-alive

## Development Workflow

1. **Build**: Use `cargo build` or `cargo build --release`
2. **Test**: Run unit tests with `cargo test`
3. **Debug**: Use `cargo run` with verbose logging for debugging
4. **Cross-platform**: Platform-specific code is conditionally compiled using `#[cfg(target_os = "platform")]`

## Key Files and Patterns

- `src/main.rs` - Entry point with CLI argument parsing
- `src/input/mod.rs` - Platform-specific input implementations
- `src/protocol/message.rs` - All message types and their serialization
- `src/network/server.rs` - Server implementation for accepting connections
- `src/network/client.rs` - Client implementation for connecting to servers
- `src/config/mod.rs` - Configuration handling with TOML serialization

## External Dependencies

- `tokio` - Async runtime
- `serde` - Serialization
- `clap` - CLI argument parsing
- `mdns-sd` - mDNS discovery
- Platform-specific crates:
  - macOS: `core-graphics`, `core-foundation`
  - Linux: `evdev`, `uinput`, `nix`
  - Windows: `windows` crate with specific Windows API features