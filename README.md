# FCA - TCP Chat Server

A multi-threaded TCP chat server written in Rust that allows multiple clients to connect and chat in different rooms.

## Features

!! Some of these features are yet to be implemented/polished
!! Only works on the local network, more support later maybe 

-  Multiple clients can connect simultaneously
-  Clients can join different chat rooms
-  Messages are broadcasted to all clients in the same room
-  Private messaging between users
-  Graceful client disconnection handling
-  Admin client for room and user management
-  Room state management (Open/Closed)

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
cargo run
```

### Running a Client

```bash
# In another terminal
cargo run client YourUsername
```

## Usage

### Client Commands

- `/join <room_id>` - Join a room
- `/leave <room_id>` - Leave a room
- `/msg <room_id> <message>` - Send message to room
- `/private <user_id> <message>` - Send private message
- `/quit` - Disconnect from server

### Example Session

```bash
# Terminal 1 - Start server
$ cargo run
Starting TCP Chat Server...
Server listening to [127.0.0.1:8080]

# Terminal 2 - Client 1
$ cargo run client Alice
Starting TCP Chat Client as 'Alice'...
Connected to server at 127.0.0.1:8080
Commands:
  /join <room_id> - Join a room
  /leave <room_id> - Leave a room
  /msg <room_id> <message> - Send message to room
  /private <user_id> <message> - Send private message
  /quit - Disconnect
/join 1
/msg 1 Hello everyone!

# Terminal 3 - Client 2
$ cargo run client Bob
/join 1
[Room 1] User 1: Hello everyone!
```

## Development

### Project Structure

```
fca/
├── src/
│   ├── main.rs      # Entry point
│   ├── server.rs    # TCP server implementation
│   ├── client.rs    # TCP client implementation
│   └── resc.rs      # Shared resources and data structures
├── Cargo.toml       # Dependencies
└── README.md        # This file
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
}
```

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## Roadmap

- [ ] Implement room creation/deletion
- [ ] Add user authentication
- [ ] Implement admin commands
- [ ] Add message history
- [ ] WebSocket support
- [ ] GUI client