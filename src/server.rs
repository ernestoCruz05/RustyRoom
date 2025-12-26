//! Async Chat Server Module for FCA
//!
//! This module implements the main TCP chat server with support for:
//! - User authentication (login/register)
//! - Room-based text chat
//! - Private messaging
//! - Voice chat via UDP
//! - SQLite database persistence
//!
//! The server uses Tokio for async I/O and supports multiple concurrent clients.

#![allow(dead_code)]

use crate::database::Database;
use crate::resc::{Message, MessageType, Room, RoomState, User, UserStatus};
use chrono::Local;
use rand::{Rng, distributions::Alphanumeric};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{RwLock, mpsc};

// ============================================================================
// SERVER LOGGING
// ============================================================================

/// Log levels for categorizing server messages
#[derive(Clone, Copy)]
enum LogLevel {
    /// General information
    Info,
    /// Warnings (non-fatal issues)
    Warn,
    /// Errors (operation failures)
    Error,
    /// Debug information (verbose)
    Debug,
    /// Voice chat related messages
    Voice,
    /// Authentication related messages
    Auth,
    /// Room management messages
    Room,
}

impl LogLevel {
    /// Returns the display prefix for this log level
    fn prefix(&self) -> &'static str {
        match self {
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
            LogLevel::Debug => "DEBUG",
            LogLevel::Voice => "VOICE",
            LogLevel::Auth => "AUTH",
            LogLevel::Room => "ROOM",
        }
    }
}

/// Logs a message with timestamp and level to stdout
fn log(level: LogLevel, message: &str) {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    println!("[{}] [{}] {}", timestamp, level.prefix(), message);
}

/// Logs an error with context to stderr
fn log_error(context: &str, error: &dyn std::fmt::Display) {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    eprintln!("[{}] [ERROR] {}: {}", timestamp, context, error);
}

// ============================================================================
// ASYNC CHAT SERVER
// ============================================================================

/// The main async chat server
///
/// Manages all connected users, rooms, and handles message routing.
/// Uses async I/O for efficient handling of many concurrent connections.
pub struct AsyncChatServer {
    /// Connected users indexed by user ID
    users: Arc<RwLock<HashMap<u16, User>>>,
    /// Active rooms indexed by room ID
    rooms: Arc<RwLock<HashMap<u16, Room>>>,
    /// Message senders for each connected user (for pushing messages)
    user_senders: Arc<RwLock<HashMap<u16, mpsc::UnboundedSender<String>>>>,
    /// Database connection for persistence
    database: Database,
    /// Counter for generating unique user IDs
    next_user_id: Arc<RwLock<u16>>,
    /// Counter for generating unique room IDs
    next_room_id: Arc<RwLock<u16>>,
    /// Voice chat authentication tokens mapped to user IDs
    voice_tokens: Arc<RwLock<HashMap<String, u16>>>,
}

impl AsyncChatServer {
    /// Creates a new AsyncChatServer with database connection
    ///
    /// # Arguments
    /// * `database_url` - SQLite connection URL (e.g., "sqlite:data.db")
    ///
    /// # Returns
    /// A Result containing the server instance or an error
    pub async fn new(database_url: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let database = Database::new(database_url).await?;

        Ok(AsyncChatServer {
            users: Arc::new(RwLock::new(HashMap::new())),
            rooms: Arc::new(RwLock::new(HashMap::new())),
            user_senders: Arc::new(RwLock::new(HashMap::new())),
            database,
            next_user_id: Arc::new(RwLock::new(1)),
            next_room_id: Arc::new(RwLock::new(1)),
            voice_tokens: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Starts the async chat server and begins accepting connections
    ///
    /// This is the main entry point for running the server. It:
    /// 1. Connects to the database (falls back to in-memory if needed)
    /// 2. Loads existing rooms from the database
    /// 3. Starts background cleanup tasks
    /// 4. Starts the UDP voice server
    /// 5. Accepts TCP connections in a loop
    ///
    /// # Arguments
    /// * `database_url` - SQLite connection URL
    /// * `address` - TCP bind address (e.g., "0.0.0.0:8080")
    pub async fn start_async_server(
        database_url: &str,
        address: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        println!();
        println!("========================================");
        println!("       FCA Chat Server Starting");
        println!("========================================");
        println!();
        
        let server = match AsyncChatServer::new(database_url).await {
            Ok(server) => {
                log(LogLevel::Info, &format!("Database connected: {}", database_url));
                server
            }
            Err(e) => {
                log(LogLevel::Warn, &format!("Database connection failed: {}", e));
                log(LogLevel::Info, "Trying in-memory database as fallback...");

                match AsyncChatServer::new("sqlite::memory:").await {
                    Ok(server) => {
                        log(LogLevel::Warn, "Using in-memory database (data will not persist)");
                        server
                    }
                    Err(e2) => {
                        log(LogLevel::Error, &format!("In-memory database also failed: {}", e2));
                        return Err(e);
                    }
                }
            }
        };

        let server = Arc::new(server);

        if let Err(e) = server.load_rooms_from_db().await {
            log(LogLevel::Warn, &format!("Could not load rooms from database: {}", e));
        }

        // Spawn background cleanup task - runs every 30 seconds
        let server_cleanup = Arc::clone(&server);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                interval.tick().await;

                if let Err(e) = server_cleanup.cleanup_disconnected_users().await {
                    log_error("Cleanup disconnected users", &e);
                }

                if let Err(e) = server_cleanup.database.cleanup_old_sessions().await {
                    log_error("Cleanup old sessions", &e);
                }

                if let Err(e) = server_cleanup.broadcast_user_list_update().await {
                    log_error("Broadcast user list", &e);
                }
            }
        });

        // Start UDP voice server on port 8081
        let udp_address = format!("{}:8081", "0.0.0.0");
        let voice_tokens_clone = server.voice_tokens.clone();
        tokio::spawn(async move {
            if let Err(e) =
                AsyncChatServer::start_udp_listener(voice_tokens_clone, udp_address).await
            {
                log(LogLevel::Error, &format!("Voice server failed: {}", e));
            }
        });

        let tcp_listener = TcpListener::bind(address).await?;
        
        println!();
        log(LogLevel::Info, &format!("TCP server listening on {}", address));
        log(LogLevel::Voice, "UDP voice server listening on port 8081");
        println!();
        println!("----------------------------------------");
        println!("  Server ready and accepting connections");
        println!("----------------------------------------");
        println!();

        loop {
            match tcp_listener.accept().await {
                Ok((stream, _addr)) => {
                    let server_clone = Arc::clone(&server);
                    tokio::spawn(async move {
                        if let Err(e) = server_clone.handle_client_async(stream).await {
                            eprintln!("Error handling client: {}", e);
                        }
                    });
                }
                Err(e) => {
                    eprintln!("Failed to accept connection: {}", e);
                }
            }
        }
    }

    /// Enhanced UDP voice server with room-based audio routing
    ///
    /// Features:
    /// - Token-based authentication
    /// - Room-based audio routing (users in same room hear each other)
    /// - Support for both new voice protocol and legacy raw audio
    /// - Automatic session cleanup
    pub async fn start_udp_listener(
        voice_tokens: Arc<RwLock<HashMap<String, u16>>>,
        bind_address: String,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::voice::{VoicePacket, VoicePacketHeader, PacketType};
        
        let udp_socket = UdpSocket::bind(&bind_address).await?;
        // Voice server started - main log is in start_async_server

        // Active voice sessions: addr -> (user_id, room_id)
        let active_sessions: Arc<RwLock<HashMap<std::net::SocketAddr, (u16, u16)>>> = 
            Arc::new(RwLock::new(HashMap::new()));
        
        // Room membership: room_id -> set of socket addresses
        let room_members: Arc<RwLock<HashMap<u16, Vec<std::net::SocketAddr>>>> = 
            Arc::new(RwLock::new(HashMap::new()));

        let mut buf = [0u8; 4096];

        loop {
            match udp_socket.recv_from(&mut buf).await {
                Ok((size, peer_addr)) => {
                    let received_data = &buf[..size];

                    // === AUTHENTICATION PHASE ===
                    // Check if this is a token authentication attempt
                    if let Ok(token_str) = std::str::from_utf8(received_data) {
                        let token = token_str.trim().to_string();
                        let mut tokens = voice_tokens.write().await;

                        if let Some(user_id) = tokens.remove(&token) {
                            // For now, use room_id = 1 (General room)
                            // In a full implementation, the token would include room_id
                            let room_id: u16 = 1;
                            
                            // Register session
                            {
                                let mut sessions = active_sessions.write().await;
                                sessions.insert(peer_addr, (user_id, room_id));
                            }
                            
                            // Add to room members
                            {
                                let mut rooms = room_members.write().await;
                                rooms.entry(room_id)
                                    .or_insert_with(Vec::new)
                                    .push(peer_addr);
                            }
                            
                            let _ = udp_socket.send_to(b"VOICE_CONNECTED", peer_addr).await;
                            log(LogLevel::Voice, &format!("User {} joined voice in room {}", user_id, room_id));
                            continue;
                        }
                    }

                    // === AUDIO ROUTING PHASE ===
                    let sessions = active_sessions.read().await;
                    
                    if let Some((sender_user_id, room_id)) = sessions.get(&peer_addr) {
                        let rooms = room_members.read().await;
                        
                        if let Some(members) = rooms.get(room_id) {
                            // Forward audio to all room members except sender
                            for member_addr in members {
                                if *member_addr != peer_addr {
                                    // Check if recipient is still in active sessions
                                    if sessions.contains_key(member_addr) {
                                        let _ = udp_socket.send_to(received_data, *member_addr).await;
                                    }
                                }
                            }
                        }
                        
                        // Handle heartbeat packets for session keepalive
                        if let Some(packet) = VoicePacket::decode(received_data) {
                            if packet.header.packet_type == PacketType::Heartbeat {
                                // Acknowledge heartbeat
                                let ack = VoicePacketHeader::new_heartbeat(*sender_user_id, *room_id);
                                let _ = udp_socket.send_to(&ack.encode(), peer_addr).await;
                            }
                        }
                    }
                }
                Err(e) => {
                    // Only log unexpected errors, not timeouts or connection resets
                    let msg = e.to_string();
                    if !msg.contains("timed out") && !msg.contains("Connection refused") {
                        log(LogLevel::Error, &format!("UDP recv error: {}", e));
                    }
                }
            }
        }
    }

    /// Loads existing rooms from the database into memory
    ///
    /// Retrieves all rooms from SQLite and populates the in-memory rooms HashMap.
    /// Creates a default "General" room if no rooms exist in the database.
    /// Also updates the `next_room_id` counter to avoid ID collisions.
    ///
    /// # Returns
    /// `Ok(())` on success, or a SQLx error if database operations fail
    async fn load_rooms_from_db(&self) -> Result<(), sqlx::Error> {
        let rooms_data = self.database.get_all_rooms().await?;
        let mut rooms = self.rooms.write().await;
        let mut next_room_id = self.next_room_id.write().await;

        for room_info in rooms_data {
            let room = Room::new(
                room_info.id,
                room_info.name,
                room_info.description,
                if room_info.is_password_protected {
                    RoomState::Private
                } else {
                    RoomState::Open
                },
            );
            rooms.insert(room_info.id, room);
            *next_room_id = (*next_room_id).max(room_info.id + 1);
        }

        if rooms.is_empty() {
            let general_room = Room::new(
                1,
                "General".to_string(),
                "General chat room for everyone".to_string(),
                RoomState::Open,
            );

            if let Ok(_) = self
                .database
                .create_system_room(1, "General", "General chat room for everyone")
                .await
            {
                rooms.insert(1, general_room);
                *next_room_id = 2;
                println!("Created default 'General' room");
            } else {
                eprintln!("Failed to create default General room");
            }
        }

        Ok(())
    }

    /// Handles an individual client TCP connection
    ///
    /// Sets up bidirectional communication with a connected client:
    /// - Splits the TCP stream into read/write halves
    /// - Creates an async channel for outgoing messages
    /// - Spawns a writer task to send messages to the client
    /// - Reads incoming messages line-by-line and processes them
    /// - Cleans up user state when the connection closes
    ///
    /// # Arguments
    /// * `stream` - The TCP stream for the connected client
    ///
    /// # Returns
    /// `Ok(())` when the client disconnects normally, or an error
    async fn handle_client_async(
        &self,
        stream: TcpStream,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let client_address = stream.peer_addr()?;
        log(LogLevel::Info, &format!("New connection from {}", client_address));

        let (read_half, mut write_half) = stream.into_split();
        let reader = BufReader::new(read_half);
        let mut lines = reader.lines();

        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        let mut connected_user_id: Option<u16> = None;

        let client_addr_clone = client_address;
        let write_task = tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                if let Err(_e) = write_half.write_all(message.as_bytes()).await {
                    // Client disconnected - this is expected, don't spam logs
                    break;
                }
                if let Err(_e) = write_half.write_all(b"\n").await {
                    break;
                }
                if let Err(_e) = write_half.flush().await {
                    break;
                }
            }
            // Log disconnect once when write task ends
            let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
            println!("[{}] [INFO] Client {} write channel closed", timestamp, client_addr_clone);
        });

        while let Some(line) = lines.next_line().await? {
            if let Ok(message) = Message::from_json_file(&line) {
                if connected_user_id.is_none() && message.sender_id != 0 {
                    connected_user_id = Some(message.sender_id);
                }
                self.process_message_async(message, tx.clone()).await?;
            }
        }

        if let Some(user_id) = connected_user_id {
            if let Some(account) = self.database.get_user_by_id(user_id).await.unwrap_or(None) {
                let _ = self
                    .database
                    .set_user_online_status(user_id, &account.username, false)
                    .await;
                log(LogLevel::Auth, &format!("User '{}' (ID: {}) disconnected and marked offline", account.username, user_id));

                let _ = self.broadcast_user_list_update().await;
            }
            let mut user_senders = self.user_senders.write().await;
            user_senders.remove(&user_id);
        }

        write_task.abort();
        log(LogLevel::Info, &format!("Connection closed: {}", client_address));
        Ok(())
    }

    /// Processes an incoming message from a client
    ///
    /// Routes the message to the appropriate handler based on its type.
    /// Also updates the user's online status when messages are received.
    ///
    /// # Arguments
    /// * `message` - The parsed message from the client
    /// * `sender` - Channel to send responses back to this client
    ///
    /// # Returns
    /// `Ok(())` on successful processing, or an error
    async fn process_message_async(
        &self,
        message: Message,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let sender_username =
            if let Some(account) = self.database.get_user_by_id(message.sender_id).await? {
                if message.sender_id != 0 {
                    let _ = self
                        .database
                        .set_user_online_status(message.sender_id, &account.username, true)
                        .await;
                }
                account.username
            } else {
                "Unknown".to_string()
            };

        match &message.message_type {
            MessageType::Register { username, password } => {
                self.handle_register(username, password, sender).await?;
            }
            MessageType::Login { username, password } => {
                self.handle_login(username, password, sender).await?;
            }
            MessageType::ListRooms => {
                self.handle_list_rooms(sender).await?;
            }
            MessageType::RequestUserList => {
                self.handle_user_list_request(sender).await?;
            }
            MessageType::CreateRoom {
                name,
                description,
                password,
            } => {
                self.handle_create_room(name, description, password, message.sender_id, sender)
                    .await?;
            }
            MessageType::RoomMessage {
                room_id,
                sender_username: _,
                content,
            } => {
                self.handle_room_message(*room_id, content, &sender_username, message.sender_id)
                    .await?;
            }
            MessageType::PrivateMessage {
                to_user_id,
                sender_username: _,
                content,
            } => {
                self.handle_private_message(
                    *to_user_id,
                    content,
                    &sender_username,
                    message.sender_id,
                )
                .await?;
            }
            MessageType::Join { room_id, password } => {
                self.handle_join_room(*room_id, password, message.sender_id, sender)
                    .await?;
            }
            MessageType::Leave { room_id } => {
                self.handle_leave_room(*room_id, message.sender_id, sender)
                    .await?;
            }
            MessageType::RequestVoice => {
                let token: String = rand::thread_rng()
                    .sample_iter(&Alphanumeric)
                    .take(16)
                    .map(char::from)
                    .collect();

                let mut tokens = self.voice_tokens.write().await;
                tokens.insert(token.clone(), message.sender_id);

                let response = Message::new(
                    0,
                    MessageType::VoiceCredentials {
                        token,
                        udp_port: 8081,
                    },
                );
                self.send_to_sender(&sender, response)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Handles user registration requests
    ///
    /// Creates a new user account with the provided credentials.
    /// The password is hashed before storage. On success, the user is
    /// automatically logged in and added to the active senders list.
    ///
    /// # Arguments
    /// * `username` - The desired username
    /// * `password` - The plaintext password (will be hashed)
    /// * `sender` - Channel to send the response back to the client
    ///
    /// # Returns
    /// `Ok(())` after sending AuthSuccess or AuthFailure response
    async fn handle_register(
        &self,
        username: &str,
        password: &str,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let password_hash = Self::hash_password(password);

        match self.database.create_user(username, &password_hash).await {
            Ok(user_id) => {
                log(LogLevel::Auth, &format!("New user registered: '{}' (ID: {})", username, user_id));
                
                let mut user_senders = self.user_senders.write().await;
                user_senders.insert(user_id, sender.clone());
                drop(user_senders);

                let response = Message::new(
                    0,
                    MessageType::AuthSuccess {
                        user_id,
                        message: format!("Registration successful! Welcome, {}", username),
                    },
                );
                self.send_to_sender(&sender, response)?;

                self.broadcast_user_list_update().await?;
            }
            Err(_) => {
                log(LogLevel::Auth, &format!("Registration failed: username '{}' already exists", username));
                
                let response = Message::new(
                    0,
                    MessageType::AuthFailure {
                        reason: "Username already exists".to_string(),
                    },
                );
                self.send_to_sender(&sender, response)?;
            }
        }

        Ok(())
    }

    /// Handles user login requests
    ///
    /// Authenticates a user with username and password. Verifies the
    /// password hash matches the stored hash. On success, marks the user
    /// as online and registers their message channel.
    ///
    /// # Arguments
    /// * `username` - The username to authenticate
    /// * `password` - The plaintext password to verify
    /// * `sender` - Channel to send the response back to the client
    ///
    /// # Returns
    /// `Ok(())` after sending AuthSuccess or AuthFailure response
    async fn handle_login(
        &self,
        username: &str,
        password: &str,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(account) = self.database.get_user_by_username(username).await? {
            let password_hash = Self::hash_password(password);
            if account.hash == password_hash {
                log(LogLevel::Auth, &format!("User '{}' (ID: {}) logged in", username, account.user_id));
                
                self.database
                    .set_user_online_status(account.user_id, &account.username, true)
                    .await?;

                let mut user_senders = self.user_senders.write().await;
                user_senders.insert(account.user_id, sender.clone());
                drop(user_senders);

                let response = Message::new(
                    0,
                    MessageType::AuthSuccess {
                        user_id: account.user_id,
                        message: format!("Login successful! Welcome back, {}", username),
                    },
                );
                self.send_to_sender(&sender, response)?;

                self.broadcast_user_list_update().await?;
            } else {
                log(LogLevel::Auth, &format!("Login failed for '{}': invalid password", username));
                
                let response = Message::new(
                    0,
                    MessageType::AuthFailure {
                        reason: "Invalid password".to_string(),
                    },
                );
                self.send_to_sender(&sender, response)?;
            }
        } else {
            log(LogLevel::Auth, &format!("Login failed: account '{}' does not exist", username));
            
            let response = Message::new(
                0,
                MessageType::AuthFailure {
                    reason: "Account does not exist".to_string(),
                },
            );
            self.send_to_sender(&sender, response)?;
        }

        Ok(())
    }

    async fn handle_list_rooms(
        &self,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let room_infos = self.database.get_all_rooms().await?;
        let response = Message::new(0, MessageType::RoomList { rooms: room_infos });
        self.send_to_sender(&sender, response)?;
        Ok(())
    }

    async fn handle_user_list_request(
        &self,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let user_sessions = self.database.get_online_users().await?;
        let user_statuses: Vec<UserStatus> = user_sessions
            .into_iter()
            .map(|session| UserStatus {
                user_id: session.user_id,
                username: session.username,
                is_online: session.is_online,
                last_seen: session.last_seen.format("%Y-%m-%d %H:%M:%S").to_string(),
            })
            .collect();

        let response = Message::new(
            0,
            MessageType::UserListUpdate {
                users: user_statuses,
            },
        );
        self.send_to_sender(&sender, response)?;
        Ok(())
    }

    /// Handles room creation requests
    ///
    /// Creates a new chat room with the specified properties. Rooms can be
    /// public (no password) or private (password protected). The room is
    /// persisted to the database and added to the in-memory room list.
    ///
    /// # Arguments
    /// * `name` - The room name
    /// * `description` - A description of the room's purpose
    /// * `password` - Optional password for private rooms
    /// * `creator_id` - User ID of the room creator
    /// * `sender` - Channel to send the response back to the client
    ///
    /// # Returns
    /// `Ok(())` after sending RoomCreated or error response
    async fn handle_create_room(
        &self,
        name: &str,
        description: &str,
        password: &Option<String>,
        creator_id: u16,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let password_hash = password.as_ref().map(|p| Self::hash_password(p));
        let room_state = if password.is_some() {
            RoomState::Private
        } else {
            RoomState::Open
        };

        let mut next_room_id_lock = self.next_room_id.write().await;
        let room_id = *next_room_id_lock;

        match self
            .database
            .create_room(
                room_id,
                name,
                description,
                &room_state,
                password_hash.as_deref(),
                creator_id,
            )
            .await
        {
            Ok(_) => {
                *next_room_id_lock += 1;

                let mut rooms = self.rooms.write().await;
                let room = if let Some(pass_hash) = password_hash {
                    Room::new_with_password(
                        room_id,
                        name.to_string(),
                        description.to_string(),
                        pass_hash,
                    )
                } else {
                    Room::new(
                        room_id,
                        name.to_string(),
                        description.to_string(),
                        RoomState::Open,
                    )
                };
                rooms.insert(room_id, room);

                let response = Message::new(
                    0,
                    MessageType::RoomCreated {
                        room_id,
                        name: name.to_string(),
                        description: description.to_string(),
                    },
                );
                self.send_to_sender(&sender, response)?;
            }
            Err(_) => {
                let response = Message::new(
                    0,
                    MessageType::ServerResponse {
                        success: false,
                        content: format!("Failed to create room {}", room_id),
                    },
                );
                self.send_to_sender(&sender, response)?;
            }
        }

        Ok(())
    }

    /// Handles messages sent to a room
    ///
    /// Saves the message to the database for history and broadcasts it
    /// to all users currently in the room.
    ///
    /// # Arguments
    /// * `room_id` - The target room ID
    /// * `content` - The message content
    /// * `sender_username` - Username of the message sender
    /// * `sender_id` - User ID of the message sender
    ///
    /// # Returns
    /// `Ok(())` after broadcasting the message
    async fn handle_room_message(
        &self,
        room_id: u16,
        content: &str,
        sender_username: &str,
        sender_id: u16,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.database
            .save_message(room_id, sender_id, sender_username, content, "user")
            .await?;

        let message = Message::new(
            sender_id,
            MessageType::RoomMessage {
                room_id,
                sender_username: sender_username.to_string(),
                content: content.to_string(),
            },
        );

        self.broadcast_to_room(room_id, message).await?;

        Ok(())
    }

    /// Handles private (direct) messages between users
    ///
    /// Saves the message to the database and sends it directly to the
    /// target user if they are currently connected.
    ///
    /// # Arguments
    /// * `to_user_id` - The recipient's user ID
    /// * `content` - The message content
    /// * `sender_username` - Username of the message sender
    /// * `sender_id` - User ID of the message sender
    ///
    /// # Returns
    /// `Ok(())` after sending the message
    async fn handle_private_message(
        &self,
        to_user_id: u16,
        content: &str,
        sender_username: &str,
        sender_id: u16,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.database
            .save_message(0, sender_id, sender_username, content, "private")
            .await?;

        let message = Message::new(
            sender_id,
            MessageType::PrivateMessage {
                to_user_id,
                sender_username: sender_username.to_string(),
                content: content.to_string(),
            },
        );

        self.send_to_user(to_user_id, message).await?;

        Ok(())
    }

    /// Handles room join requests
    ///
    /// Validates the room exists and verifies the password for private rooms.
    /// On success, adds the user to the room, sends recent message history,
    /// and broadcasts a join notification to other room members.
    ///
    /// # Arguments
    /// * `room_id` - The room to join
    /// * `password` - Optional password for private rooms
    /// * `user_id` - The joining user's ID
    /// * `sender` - Channel to send responses back to the client
    ///
    /// # Returns
    /// `Ok(())` after processing the join request
    async fn handle_join_room(
        &self,
        room_id: u16,
        password: &Option<String>,
        user_id: u16,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(room) = self.database.get_room(room_id).await? {
            let can_join = if room.is_password_protected() {
                match password {
                    Some(pass) => {
                        let password_hash = Self::hash_password(pass);
                        room.verify_password(&password_hash)
                    }
                    None => false,
                }
            } else {
                true
            };

            if can_join {
                self.database.add_user_to_room(user_id, room_id).await?;
                let _ = self.database.increment_room_user_count(room_id).await;

                let response = Message::new(
                    0,
                    MessageType::RoomJoined {
                        room_id,
                        name: room.name.clone(),
                        description: room.description.clone(),
                    },
                );
                self.send_to_sender(&sender, response)?;

                match self.database.get_recent_messages(room_id).await {
                    Ok(messages) => {
                        for stored_message in messages {
                            let history_message = Message::new(
                                stored_message.sender_id,
                                MessageType::RoomMessage {
                                    room_id,
                                    sender_username: stored_message.sender_username,
                                    content: format!(
                                        "[{}] {}",
                                        stored_message.timestamp.format("%H:%M"),
                                        stored_message.content
                                    ),
                                },
                            );
                            self.send_to_sender(&sender, history_message)?;
                        }

                        let separator = Message::new(
                            0,
                            MessageType::RoomMessage {
                                room_id,
                                sender_username: "System".to_string(),
                                content: "--- End of message history ---".to_string(),
                            },
                        );
                        self.send_to_sender(&sender, separator)?;
                    }
                    Err(e) => {
                        eprintln!("Failed to load message history for room {}: {}", room_id, e);
                    }
                }

                let username = if let Some(account) = self.database.get_user_by_id(user_id).await? {
                    account.username
                } else {
                    "Unknown".to_string()
                };

                let join_notification = Message::new(
                    0,
                    MessageType::RoomMessage {
                        room_id,
                        sender_username: "System".to_string(),
                        content: format!("{} joined the room", username),
                    },
                );

                self.broadcast_to_room(room_id, join_notification).await?;
            } else {
                let response = Message::new(
                    0,
                    MessageType::ServerResponse {
                        success: false,
                        content: "Invalid room password".to_string(),
                    },
                );
                self.send_to_sender(&sender, response)?;
            }
        } else {
            let response = Message::new(0, MessageType::RoomNotFound { room_id });
            self.send_to_sender(&sender, response)?;
        }

        Ok(())
    }

    /// Handles room leave requests
    ///
    /// Removes the user from the room and broadcasts a leave notification
    /// to remaining room members. Also updates the room's user count.
    ///
    /// # Arguments
    /// * `room_id` - The room to leave
    /// * `user_id` - The leaving user's ID
    /// * `sender` - Channel to send the confirmation response
    ///
    /// # Returns
    /// `Ok(())` after processing the leave request
    async fn handle_leave_room(
        &self,
        room_id: u16,
        user_id: u16,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Err(e) = self.database.remove_user_from_room(user_id, room_id).await {
            eprintln!(
                "Failed to remove user {} from room {}: {}",
                user_id, room_id, e
            );
        } else {
            let _ = self.database.decrement_room_user_count(room_id).await;
        }

        let response = Message::new(
            0,
            MessageType::ServerResponse {
                success: true,
                content: format!("Left room {}", room_id),
            },
        );
        self.send_to_sender(&sender, response)?;

        let username = if let Some(account) = self.database.get_user_by_id(user_id).await? {
            account.username
        } else {
            "Unknown".to_string()
        };

        let leave_notification = Message::new(
            0,
            MessageType::RoomMessage {
                room_id,
                sender_username: "System".to_string(),
                content: format!("{} left the room", username),
            },
        );

        self.broadcast_to_room(room_id, leave_notification).await?;

        Ok(())
    }

    /// Sends a message to a specific user by their ID
    ///
    /// Looks up the user's message channel and sends the message if they
    /// are currently connected. Silently succeeds if the user is offline.
    ///
    /// # Arguments
    /// * `user_id` - The target user's ID
    /// * `message` - The message to send
    ///
    /// # Returns
    /// `Ok(())` after attempting to send
    async fn send_to_user(
        &self,
        user_id: u16,
        message: Message,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let user_senders = self.user_senders.read().await;
        if let Some(sender) = user_senders.get(&user_id) {
            self.send_to_sender(sender, message)?;
        }
        Ok(())
    }

    /// Sends a message through a specific sender channel
    ///
    /// Serializes the message to JSON and sends it through the provided
    /// unbounded channel. This is a synchronous helper used by other send methods.
    ///
    /// # Arguments
    /// * `sender` - The channel to send through
    /// * `message` - The message to serialize and send
    ///
    /// # Returns
    /// `Ok(())` on success, or an IO error if the channel is closed
    fn send_to_sender(
        &self,
        sender: &mpsc::UnboundedSender<String>,
        message: Message,
    ) -> Result<(), std::io::Error> {
        if let Ok(json) = message.to_json_file() {
            sender.send(json).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Channel closed")
            })?;
        }
        Ok(())
    }

    /// Hashes a password using SHA-256
    ///
    /// # Arguments
    /// * `password` - The plaintext password to hash
    ///
    /// # Returns
    /// The hexadecimal string representation of the SHA-256 hash
    fn hash_password(password: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(password);
        format!("{:x}", hasher.finalize())
    }

    /// Broadcasts a message to all users in a specific room
    ///
    /// Retrieves the list of room members from the database and sends
    /// the message to each connected member.
    ///
    /// # Arguments
    /// * `room_id` - The room to broadcast to
    /// * `message` - The message to broadcast
    ///
    /// # Returns
    /// `Ok(())` after sending to all available recipients
    async fn broadcast_to_room(
        &self,
        room_id: u16,
        message: Message,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let room_members = self.database.get_room_members(room_id).await?;
        let user_senders = self.user_senders.read().await;

        let message_json = message.to_json_file()?;

        for member_id in room_members {
            if let Some(sender) = user_senders.get(&member_id) {
                let _ = sender.send(message_json.clone());
            }
        }

        Ok(())
    }

    /// Broadcasts a message to all connected users
    ///
    /// Sends the message to every user with an active connection,
    /// regardless of which rooms they are in.
    ///
    /// # Arguments
    /// * `message` - The message to broadcast
    ///
    /// # Returns
    /// `Ok(())` after sending to all connected users
    async fn broadcast_to_all_users(
        &self,
        message: Message,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let user_senders = self.user_senders.read().await;
        let message_json = message.to_json_file()?;

        for sender in user_senders.values() {
            let _ = sender.send(message_json.clone());
        }

        Ok(())
    }

    /// Broadcasts an updated user list to all connected clients
    ///
    /// Fetches the current online users from the database and sends
    /// a `UserListUpdate` message to all connected clients. This is
    /// called when users log in, log out, or during periodic cleanup.
    ///
    /// # Returns
    /// `Ok(())` after broadcasting the update
    async fn broadcast_user_list_update(&self) -> Result<(), Box<dyn std::error::Error>> {
        let user_sessions = self.database.get_online_users().await?;
        let user_statuses: Vec<UserStatus> = user_sessions
            .into_iter()
            .map(|session| UserStatus {
                user_id: session.user_id,
                username: session.username,
                is_online: session.is_online,
                last_seen: session.last_seen.format("%Y-%m-%d %H:%M:%S").to_string(),
            })
            .collect();

        let message = Message::new(
            0,
            MessageType::UserListUpdate {
                users: user_statuses,
            },
        );
        self.broadcast_to_all_users(message).await?;
        Ok(())
    }

    /// Cleans up stale connections from disconnected users
    ///
    /// Iterates through all user sender channels and removes any that
    /// are closed. Also marks those users as offline in the database.
    /// This runs periodically as a background task to handle ungraceful
    /// disconnections.
    ///
    /// # Returns
    /// `Ok(())` after cleanup completes
    async fn cleanup_disconnected_users(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut user_senders = self.user_senders.write().await;
        let mut disconnected_users = Vec::new();

        for (&user_id, sender) in user_senders.iter() {
            if sender.is_closed() {
                disconnected_users.push(user_id);
            }
        }

        if !disconnected_users.is_empty() {
            log(LogLevel::Debug, &format!("Cleaning up {} stale connections", disconnected_users.len()));
        }
        
        for user_id in disconnected_users {
            user_senders.remove(&user_id);

            if let Some(account) = self.database.get_user_by_id(user_id).await.unwrap_or(None) {
                let _ = self
                    .database
                    .set_user_online_status(user_id, &account.username, false)
                    .await;
                log(LogLevel::Auth, &format!("User '{}' marked offline (stale connection)", account.username));
            }
        }

        Ok(())
    }
}
