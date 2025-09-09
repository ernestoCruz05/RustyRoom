# RustyRoom - Async TCP Chat Server

A high-performance, async TCP chat server written in Rust with SQLite persistence and modern terminal user interface. RustyRoom provides a robust platform for real-time multi-user chat with room-based organization, user authentication, and comprehensive message management.

## Features

### Core Functionality
- **Asynchronous Architecture** - Built with Tokio for high-performance concurrent connections
- **SQLite Database** - Persistent storage for users, rooms, messages, and sessions
- **User Authentication** - Secure registration and login with SHA256 password hashing
- **Room Management** - Create, join, and manage chat rooms with descriptions and optional passwords
- **Real-time Messaging** - Instant message delivery with automatic user status updates
- **Message History** - Persistent chat history that survives server restarts
- **Private Messaging** - Direct messages between users
- **Connection Pooling** - Optimized database connections with automatic cleanup

### User Interface
- **Modern TUI Client** - Beautiful terminal interface built with ratatui
- **CLI Client** - Command-line interface for script automation and testing
- **Real-time Updates** - Live user lists, message notifications, and status changes
- **Multi-room Support** - Tab-based room switching with visual indicators

### Security & Reliability
- **Secure Authentication** - Password hashing and session management
- **Protected Rooms** - Password-protected private rooms
- **Graceful Disconnection** - Automatic cleanup of stale connections
- **Error Handling** - Comprehensive error management with custom error types

## Architecture

The application follows a modular architecture with clear separation of concerns:

- **Server** (`src/server.rs`) - Async TCP server with room and user management
- **Database** (`src/database.rs`) - SQLite operations with connection pooling
- **TUI Client** (`src/tui_client.rs`) - Terminal user interface implementation
- **CLI Client** (`src/client.rs`) - Command-line client interface
- **Resources** (`src/resc.rs`) - Shared data structures and message types
- **Main** (`src/main.rs`) - Application entry point and configuration

## Quick Start

### Prerequisites

- Rust 1.70 or higher
- Cargo package manager
- SQLite (automatically handled)

### Installation

```bash
git clone https://github.com/ernestoCruz05/RustyRoom.git
cd RustyRoom
cargo build --release
```

### Starting the Server

```bash
cargo run server
```

The server will:
- Create a SQLite database (`chat.db`) in the current directory
- Initialize required tables automatically
- Create a default "General" room
- Listen on `127.0.0.1:8080`

### Connecting Clients

#### TUI Client (Recommended)
```bash
cargo run tui
```

#### CLI Client
```bash
cargo run client <username>
```

## Usage Guide

### TUI Client Interface

**Authentication Screen:**
- `Tab` - Switch between username and password fields
- `F2` - Toggle between Login and Register modes
- `Enter` - Submit authentication credentials
- `Esc` - Exit application

**Main Chat Interface:**
- `Enter` - Send message or execute command
- `Tab` - Switch between joined rooms
- `F1` - Display help dialog
- `F2` - Open room browser (create/join rooms)
- `F3` - Access private messaging
- `F4` - Open settings menu
- `Esc` - Close dialogs or exit application

### Chat Commands

All commands are prefixed with `/` and work in both TUI and CLI clients:

```bash
/join <room_id> [password]                    # Join existing room
/leave <room_id>                              # Leave current room
/create <room_id> <name> <description> [pwd]  # Create new room
/msg <room_id> <message>                      # Send room message (CLI only)
/private <user_id> <message>                  # Send private message
/quit                                         # Disconnect from server
```

### CLI Client Commands

```bash
/register <username> <password>  # Create new account
/login <username> <password>     # Login to existing account
/rooms                          # List available rooms
/users                          # List online users
```

## Example Session

```bash
# Terminal 1 - Start the server
$ cargo run server
Starting Async TCP Chat Server with database...
Database connection successful: sqlite:chat.db
Created default 'General' room
Async server listening to [127.0.0.1:8080] with database

# Terminal 2 - TUI Client (User: Alice)
$ cargo run tui
# 1. Enter credentials in authentication screen
# 2. Use F2 to register new account or login
# 3. Join General room or create new rooms
# 4. Start chatting with real-time updates

# Terminal 3 - CLI Client (User: Bob)
$ cargo run client Bob
/register bob secretpass
[SUCCESS] Registration successful! Welcome, bob
/join 1
[SUCCESS] Joined room 1: General
/msg 1 Hello everyone from CLI!
```

## Database Schema

RustyRoom uses SQLite with the following core tables:

```sql
-- User accounts with authentication
users (id, username, password_hash, is_online, created_at, last_seen)

-- Chat rooms with metadata
rooms (id, name, description, state, password_hash, user_count, created_at, created_by)

-- Message history
messages (id, room_id, sender_id, sender_username, content, message_type, timestamp)

-- Room memberships
room_memberships (user_id, room_id, joined_at)

-- Active user sessions
user_sessions (user_id, username, is_online, last_activity, current_rooms)
```

## Development

### Building

```bash
# Debug build
cargo build

# Release build with optimizations
cargo build --release

# Run tests
cargo test

# Check code formatting
cargo fmt --check

# Run linter
cargo clippy
```

### Project Structure

```
RustyRoom/
├── src/
│   ├── main.rs           # Application entry point
│   ├── server.rs         # Async TCP server implementation
│   ├── database.rs       # SQLite database operations
│   ├── tui_client.rs     # Terminal UI client
│   ├── client.rs         # CLI client implementation
│   └── resc.rs           # Shared data structures
├── Cargo.toml            # Project dependencies
├── LICENSE               # MIT license
└── README.md             # This documentation
```

### Message Protocol

Communication uses JSON-serialized messages over TCP:

```rust
pub enum MessageType {
    // Authentication
    Register { username: String, password: String },
    Login { username: String, password: String },
    AuthSuccess { user_id: u16, message: String },
    AuthFailure { reason: String },
    
    // Room operations
    CreateRoom { room_id: u16, name: String, description: String, password: Option<String> },
    Join { room_id: u16, password: Option<String> },
    Leave { room_id: u16 },
    
    // Messaging
    RoomMessage { room_id: u16, sender_username: String, content: String },
    PrivateMessage { to_user_id: u16, sender_username: String, content: String },
    
    // Updates
    UserListUpdate { users: Vec<UserStatus> },
    RoomList { rooms: Vec<RoomInfo> },
}
```

## Performance Characteristics

- **Concurrent Connections**: Handles hundreds of simultaneous users
- **Database Pool**: Configurable connection pooling (default: 10 connections)
- **Memory Usage**: Efficient async/await with minimal heap allocations
- **Latency**: Sub-millisecond message routing for local networks
- **Persistence**: All messages and user data survive server restarts

## Configuration

The server can be configured through environment variables:

```bash
# Database configuration
DATABASE_URL=sqlite:custom_path.db

# Server binding
BIND_ADDRESS=0.0.0.0:8080

# Connection pool size
MAX_CONNECTIONS=20
```

## Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

Please ensure all tests pass and follow the existing code style.

## License

This project is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.

## Roadmap

### Completed
- Async TCP server architecture
- SQLite database integration with connection pooling
- User authentication and session management
- Room creation and management with password protection
- Real-time messaging with message history
- Modern TUI client with comprehensive features
- Automatic disconnect detection and cleanup

### Planned
- WebSocket support for web clients
- File transfer capabilities
- Advanced admin commands and moderation tools
- Message encryption for enhanced security
- REST API for third-party integrations
- Horizontal scaling with Redis backend
