# FCA - TCP Chat Server

A multi-threaded TCP chat server written in Rust that allows multiple clients to connect and chat in different rooms.

## Features

✅ **Completed Features:**
-   **Modern TUI Interface** - Beautiful terminal user interface with ratatui
-   **Multi-client Support** - Multiple clients can connect simultaneously  
-   **Room-based Chat** - Clients can join different chat rooms with descriptions
-   **User Authentication** - Secure registration and login system
-   **Private Rooms** - Password-protected rooms for secure discussions
-   **Private Messaging** - Direct messages between users
-   **Rich UI** - Room tabs, message history, real-time updates
-   **Real-time** - Instant message delivery and updates
-   **Graceful Handling** - Proper client disconnection management

🚧 **In Progress:**
-   **Admin Commands** - Room and user management
-   **Message History** - Persistent chat history
-   **WebSocket Support** - Modern web connectivity

## Architecture

- **Server** ([`src/server.rs`](src/server.rs)) - Multi-threaded TCP server with room management
- **Client** ([`src/client.rs`](src/client.rs)) - TCP client for connecting to chat rooms
- **Resources** ([`src/resc.rs`](src/resc.rs)) - Shared data structures (User, Room, Message)
- **Main** ([`src/main.rs`](src/main.rs)) - Entry point and CLI argument handling

## Quick Start

### Prerequisites

- Rust 1.70+ (made with 1.88.0)
- Cargo

### Installation

```bash
git clone https://github.com/ernestoCruz05/fca.git
cd fca
cargo build --release
```

!! This will be changed later for an account based system, and asking for a login on start

### Running the Server

```bash
cargo run -- server
```

### Running a Client

#### TUI Client (Recommended)
```bash
cargo run -- tui
```

#### CLI Client
```bash
cargo run -- client YourUsername
```

## Usage

### TUI Client Commands

**Authentication Screen:**
- `Tab` - Switch between username/password fields
- `F2` - Toggle between Login/Register mode
- `Enter` - Submit authentication
- `Esc` - Quit application

**Main Chat Interface:**
- `Enter` - Send message or command
- `Tab` - Switch between joined rooms
- `F1` - Show help dialog
- `F2` - Room browser (create/join rooms)
- `F3` - Private message interface
- `F4` - Settings
- `Esc` - Close dialogs or quit

**Chat Commands:**
- `/join <room_id> [password]` - Join a room
- `/create <room_id> <name> <description> [password]` - Create new room
- `/leave <room_id>` - Leave a room

### CLI Client Commands

### CLI Client Commands

- `/register <username> <password>` - Create new account
- `/login <username> <password>` - Login to existing account
- `/join <room_id> [password]` - Join a room
- `/leave <room_id>` - Leave a room
- `/create <room_id> <name> <description> [password]` - Create new room
- `/msg <room_id> <message>` - Send message to room
- `/private <user_id> <message>` - Send private message
- `/quit` - Disconnect from server

### Example Session

```bash
# Terminal 1 - Start server
$ cargo run -- async-server
Starting TCP Chat Server...
Server listening to [127.0.0.1:8080]

# Terminal 2 - TUI Client
$ cargo run -- tui
# 1. Enter username/password in auth screen
# 2. Use Tab to switch fields, F2 to toggle Login/Register
# 3. Press Enter to authenticate
# 4. Use /create 1 "General" "General discussion"
# 5. Type messages normally, use Tab to switch rooms

# Terminal 3 - CLI Client (alternative)
$ cargo run -- client Alice
/register alice password123
[SUCCESS] Registration successful! Welcome, alice
/create 1 "General" "General discussion"
/msg 1 Hello from CLI!
```

## Development

### Project Structure

```
fca/
├── src/
│   ├── main.rs        # Entry point
│   ├── server.rs      # TCP server implementation
│   ├── client.rs      # TCP client implementation
│   └── resc.rs        # Shared resources and data structures
|   └── tui_client.rs  # Text User Interface 
├── Cargo.toml         # Dependencies
└── README.md          # This file
```

### Building

```bash
# Debug build
cargo build

# Release build
cargo build --release

# Run tests
cargo test
```

## Message Protocol

The client-server communication uses JSON-serialized messages:

```rust
pub enum MessageType {
    Join { room_id: u16 },
    Leave { room_id: u16 },
    RoomMessage { room_id: u16, content: String },
    PrivateMessage { to_user_id: u16, content: String },
    ServerResponse { success: bool, content: String },
    // more fields...
}
```

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## Roadmap

- [X] Implement room creation/deletion
- [X] Add user authentication
- [x] Add password-protected private rooms
- [x] Implement descriptive room system
- [X] Implement admin commands
- [ ] Add message history
- [ ] WebSocket support
