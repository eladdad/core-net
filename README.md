# CoreNet - Cross-Host I/O Device Sharing

A software KVM (Keyboard, Video, Mouse) solution that allows seamless sharing of input devices across multiple computers on a network.

## Overview

CoreNet enables you to use a single mouse and keyboard to control multiple computers. Move your cursor to the edge of one screen and it seamlessly transitions to the next computer - as if you had multiple monitors connected to one machine.

## Features

- **Seamless Mouse Transition**: Move cursor across screen boundaries to switch between hosts
- **Keyboard Sharing**: Type on any connected host using your primary keyboard
- **Clipboard Sync**: Copy on one machine, paste on another (optional)
- **Cross-Platform**: Supports macOS, Linux, and Windows
- **Encrypted Communication**: All traffic is encrypted using TLS
- **Zero Configuration**: Auto-discovery of hosts on the same network using mDNS

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                         CoreNet Architecture                         │
└─────────────────────────────────────────────────────────────────────┘

┌──────────────┐         Network (TCP/TLS)        ┌──────────────┐
│   Host A     │◄────────────────────────────────►│   Host B     │
│   (Server)   │                                  │   (Client)   │
│              │                                  │              │
│ ┌──────────┐ │                                  │ ┌──────────┐ │
│ │  Input   │ │    ┌────────────────────┐        │ │  Input   │ │
│ │ Capture  │─┼───►│  Protocol Layer    │───────►├─│ Injection│ │
│ └──────────┘ │    │  - Mouse events    │        │ └──────────┘ │
│              │    │  - Keyboard events │        │              │
│ ┌──────────┐ │    │  - Screen info     │        │ ┌──────────┐ │
│ │  Screen  │ │    │  - Clipboard data  │        │ │  Screen  │ │
│ │  Edge    │ │    └────────────────────┘        │ │  Edge    │ │
│ │ Detection│ │                                  │ │ Detection│ │
│ └──────────┘ │                                  │ └──────────┘ │
└──────────────┘                                  └──────────────┘
```

## How It Works

### 1. Input Capture
Each platform requires different APIs to capture input events:

| Platform | Mouse/Keyboard Capture | Input Injection |
|----------|----------------------|-----------------|
| macOS    | CGEventTap (Quartz Event Services) | CGEventPost |
| Linux    | libevdev / uinput | uinput virtual device |
| Windows  | Raw Input API / Low-level hooks | SendInput API |

### 2. Screen Edge Detection
The system monitors cursor position. When the cursor hits a configured screen edge:
1. The cursor is "captured" and hidden on the current machine
2. Input events are redirected to the target machine
3. The cursor appears on the target machine at the corresponding position

### 3. Network Protocol
CoreNet uses a custom binary protocol over TCP/TLS:

```
┌─────────────────────────────────────────┐
│            Message Format               │
├─────────┬─────────┬─────────┬──────────┤
│ Type    │ Length  │ Sequence│ Payload  │
│ (1 byte)│ (4 bytes)│(4 bytes)│ (varies) │
└─────────┴─────────┴─────────┴──────────┘

Message Types:
- 0x01: Mouse Move (relative)
- 0x02: Mouse Move (absolute)
- 0x03: Mouse Button (down/up)
- 0x04: Mouse Scroll
- 0x05: Key Down
- 0x06: Key Up
- 0x07: Screen Info
- 0x08: Enter Screen
- 0x09: Leave Screen
- 0x0A: Clipboard Data
- 0x0B: Heartbeat
```

### 4. Host Discovery
Hosts discover each other using mDNS (Bonjour/Avahi):
- Service type: `_corenet._tcp`
- TXT records contain host capabilities and screen configuration

## Installation

### Prerequisites

**macOS:**
```bash
# Requires Accessibility permissions (System Preferences > Privacy > Accessibility)
# No additional dependencies needed - uses native Quartz APIs
```

**Linux:**
```bash
# Install dependencies
sudo apt install libevdev-dev libudev-dev  # Debian/Ubuntu
sudo dnf install libevdev-devel systemd-devel  # Fedora

# User must be in 'input' group
sudo usermod -aG input $USER
```

**Windows:**
```powershell
# No additional dependencies - uses native Windows APIs
# May require running as Administrator for global hooks
```

### Building

```bash
# Clone the repository
git clone https://github.com/your-org/corenet.git
cd corenet

# Build with Cargo
cargo build --release

# The binary will be at target/release/corenet
```

## Usage

### Server Mode (Primary Host)
The server is the machine with the physical mouse/keyboard you want to share.

```bash
# Start as server
corenet server --config config.toml

# Or with command-line options
corenet server --port 24800 --name "MacBook Pro"
```

### Client Mode (Secondary Host)
Clients receive input events from the server.

```bash
# Connect to a server
corenet client --server 192.168.1.100

# Or use auto-discovery
corenet client --discover
```

### Configuration

Create a `config.toml` file:

```toml
[general]
name = "MacBook Pro"
port = 24800

[screen]
width = 2560
height = 1600
position = "center"  # left, center, right

[neighbors]
# Define which hosts are adjacent to which screen edges
left = "Desktop-PC"
right = "Linux-Workstation"

[security]
require_tls = true
certificate = "~/.corenet/cert.pem"
key = "~/.corenet/key.pem"

[clipboard]
enabled = true
max_size = "10MB"
```

## Security Considerations

1. **TLS Encryption**: All network traffic is encrypted
2. **Authentication**: Hosts authenticate using pre-shared keys or certificates
3. **Firewall**: Only the CoreNet port needs to be open (default: 24800)
4. **Local Network**: Designed for trusted local networks

## Technical Deep Dive

### Platform-Specific Implementation

#### macOS (Quartz Event Services)

```c
// Create event tap to capture all input events
CGEventMask eventMask = (1 << kCGEventMouseMoved) | 
                        (1 << kCGEventLeftMouseDown) |
                        (1 << kCGEventKeyDown) | ...;

CFMachPortRef eventTap = CGEventTapCreate(
    kCGSessionEventTap,
    kCGHeadInsertEventTap,
    kCGEventTapOptionDefault,
    eventMask,
    eventCallback,
    NULL
);
```

#### Linux (evdev/uinput)

```c
// Open input devices
int fd = open("/dev/input/event0", O_RDONLY);
libevdev_new_from_fd(fd, &dev);

// Create virtual input device for injection
int uinput_fd = open("/dev/uinput", O_WRONLY | O_NONBLOCK);
ioctl(uinput_fd, UI_SET_EVBIT, EV_KEY);
ioctl(uinput_fd, UI_SET_EVBIT, EV_REL);
```

#### Windows (Low-level Hooks)

```c
// Set up low-level hooks
HHOOK mouseHook = SetWindowsHookEx(
    WH_MOUSE_LL,
    MouseProc,
    hInstance,
    0
);

// Inject input
INPUT input = {0};
input.type = INPUT_MOUSE;
input.mi.dx = x;
input.mi.dy = y;
SendInput(1, &input, sizeof(INPUT));
```

## Comparison with Existing Solutions

| Feature | CoreNet | Synergy | Barrier | Mouse w/o Borders |
|---------|---------|---------|---------|-------------------|
| Open Source | ✅ | ❌ (Core) | ✅ | ❌ |
| Cross-Platform | ✅ | ✅ | ✅ | ❌ (Windows only) |
| Modern Codebase | ✅ (Rust) | C++ | C++ | C# |
| Auto-Discovery | ✅ | ❌ | ❌ | ✅ |
| Encrypted | ✅ | Paid | ✅ | ✅ |
| Clipboard Sync | ✅ | ✅ | ✅ | ✅ |

## Roadmap

- [x] Core protocol design
- [ ] macOS input capture/injection
- [ ] Linux input capture/injection  
- [ ] Windows input capture/injection
- [ ] Screen edge detection
- [ ] mDNS discovery
- [ ] TLS encryption
- [ ] Clipboard synchronization
- [ ] GUI configuration tool
- [ ] Drag-and-drop file transfer

## Contributing

Contributions are welcome! Please read our contributing guidelines before submitting PRs.

## License

MIT License - see LICENSE file for details.
