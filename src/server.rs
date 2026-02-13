//! Async Chat Server Module for RustyRoom
//!
//! TCP server with support for:
//! - User authentication (login/register)
//! - Server (guild) management
//! - Channel-based text chat (text & voice channels)
//! - Private messaging
//! - Voice chat via UDP
//! - SQLite database persistence

#![allow(dead_code)]

use crate::database::Database;
use crate::resc::{
    ChannelType, Message, MessageType, User,
    UserStatus,
};
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

#[derive(Clone, Copy)]
enum LogLevel {
    Info,
    Warn,
    Error,
    Debug,
    Voice,
    Auth,
    Server,
    Channel,
}

impl LogLevel {
    fn prefix(&self) -> &'static str {
        match self {
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
            LogLevel::Debug => "DEBUG",
            LogLevel::Voice => "VOICE",
            LogLevel::Auth => "AUTH",
            LogLevel::Server => "SERVER",
            LogLevel::Channel => "CHANNEL",
        }
    }
}

fn log(level: LogLevel, message: &str) {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    println!("[{}] [{}] {}", timestamp, level.prefix(), message);
}

fn log_error(context: &str, error: &dyn std::fmt::Display) {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    eprintln!("[{}] [ERROR] {}: {}", timestamp, context, error);
}

// ============================================================================
// VOICE TOKEN DATA
// ============================================================================

/// Voice token stores both user_id and channel_id so the UDP listener
/// can route audio to the correct voice channel.
#[derive(Debug, Clone)]
pub(crate) struct VoiceTokenData {
    user_id: u16,
    channel_id: u16,
}

// ============================================================================
// ASYNC CHAT SERVER
// ============================================================================

pub struct AsyncChatServer {
    /// Connected users indexed by user ID
    users: Arc<RwLock<HashMap<u16, User>>>,
    /// Message senders for each connected user
    user_senders: Arc<RwLock<HashMap<u16, mpsc::UnboundedSender<String>>>>,
    /// Database connection
    database: Database,
    /// Voice chat authentication tokens
    voice_tokens: Arc<RwLock<HashMap<String, VoiceTokenData>>>,
}

impl AsyncChatServer {
    pub async fn new(database_url: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let database = Database::new(database_url).await?;

        Ok(AsyncChatServer {
            users: Arc::new(RwLock::new(HashMap::new())),
            user_senders: Arc::new(RwLock::new(HashMap::new())),
            database,
            voice_tokens: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Main entry point: start TCP + UDP servers
    pub async fn start_async_server(
        database_url: &str,
        address: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        println!();
        println!("========================================");
        println!("     RustyRoom Chat Server Starting");
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

        // Background cleanup task
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
        let voice_tokens_clone = server.voice_tokens.clone();
        tokio::spawn(async move {
            if let Err(e) =
                AsyncChatServer::start_udp_listener(voice_tokens_clone, "0.0.0.0:8081".to_string())
                    .await
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

    // ========================================================================
    // UDP VOICE SERVER
    // ========================================================================

    pub async fn start_udp_listener(
        voice_tokens: Arc<RwLock<HashMap<String, VoiceTokenData>>>,
        bind_address: String,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::voice::{PacketType, VoicePacket, VoicePacketHeader};

        let udp_socket = UdpSocket::bind(&bind_address).await?;

        // Active voice sessions: addr -> (user_id, channel_id)
        let active_sessions: Arc<RwLock<HashMap<std::net::SocketAddr, (u16, u16)>>> =
            Arc::new(RwLock::new(HashMap::new()));

        // Channel membership: channel_id -> set of socket addresses
        let channel_members: Arc<RwLock<HashMap<u16, Vec<std::net::SocketAddr>>>> =
            Arc::new(RwLock::new(HashMap::new()));

        let mut buf = [0u8; 4096];

        loop {
            match udp_socket.recv_from(&mut buf).await {
                Ok((size, peer_addr)) => {
                    let received_data = &buf[..size];

                    // === AUTHENTICATION PHASE ===
                    if let Ok(token_str) = std::str::from_utf8(received_data) {
                        let token = token_str.trim().to_string();
                        let mut tokens = voice_tokens.write().await;

                        if let Some(token_data) = tokens.remove(&token) {
                            let user_id = token_data.user_id;
                            let channel_id = token_data.channel_id;

                            {
                                let mut sessions = active_sessions.write().await;
                                sessions.insert(peer_addr, (user_id, channel_id));
                            }
                            {
                                let mut channels = channel_members.write().await;
                                channels
                                    .entry(channel_id)
                                    .or_insert_with(Vec::new)
                                    .push(peer_addr);
                            }

                            let _ = udp_socket.send_to(b"VOICE_CONNECTED", peer_addr).await;
                            log(
                                LogLevel::Voice,
                                &format!(
                                    "User {} joined voice channel {}",
                                    user_id, channel_id
                                ),
                            );
                            continue;
                        }
                    }

                    // === AUDIO ROUTING PHASE ===
                    let sessions = active_sessions.read().await;

                    if let Some((sender_user_id, channel_id)) = sessions.get(&peer_addr) {
                        let channels = channel_members.read().await;

                        if let Some(members) = channels.get(channel_id) {
                            for member_addr in members {
                                if *member_addr != peer_addr {
                                    if sessions.contains_key(member_addr) {
                                        let _ = udp_socket
                                            .send_to(received_data, *member_addr)
                                            .await;
                                    }
                                }
                            }
                        }

                        if let Some(packet) = VoicePacket::decode(received_data) {
                            if packet.header.packet_type == PacketType::Heartbeat {
                                let ack = VoicePacketHeader::new_heartbeat(
                                    *sender_user_id,
                                    *channel_id,
                                );
                                let _ = udp_socket.send_to(&ack.encode(), peer_addr).await;
                            }
                        }
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    if !msg.contains("timed out") && !msg.contains("Connection refused") {
                        log(LogLevel::Error, &format!("UDP recv error: {}", e));
                    }
                }
            }
        }
    }

    // ========================================================================
    // CLIENT CONNECTION HANDLER
    // ========================================================================

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
                if let Err(_) = write_half.write_all(message.as_bytes()).await {
                    break;
                }
                if let Err(_) = write_half.write_all(b"\n").await {
                    break;
                }
                if let Err(_) = write_half.flush().await {
                    break;
                }
            }
            let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
            println!(
                "[{}] [INFO] Client {} write channel closed",
                timestamp, client_addr_clone
            );
        });

        while let Some(line) = lines.next_line().await? {
            if let Ok(message) = Message::from_json_file(&line) {
                if connected_user_id.is_none() && message.sender_id != 0 {
                    connected_user_id = Some(message.sender_id);
                }
                self.process_message_async(message, tx.clone()).await?;
            }
        }

        // Cleanup on disconnect
        if let Some(user_id) = connected_user_id {
            if let Some(account) = self.database.get_user_by_id(user_id).await.unwrap_or(None) {
                let _ = self
                    .database
                    .set_user_online_status(user_id, &account.username, false)
                    .await;
                log(
                    LogLevel::Auth,
                    &format!(
                        "User '{}' (ID: {}) disconnected and marked offline",
                        account.username, user_id
                    ),
                );
                let _ = self.broadcast_user_list_update().await;
            }
            let mut user_senders = self.user_senders.write().await;
            user_senders.remove(&user_id);
        }

        write_task.abort();
        log(
            LogLevel::Info,
            &format!("Connection closed: {}", client_address),
        );
        Ok(())
    }

    // ========================================================================
    // MESSAGE ROUTER
    // ========================================================================

    async fn process_message_async(
        &self,
        message: Message,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Update user online status
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
            // --- Authentication ---
            MessageType::Register { username, password } => {
                self.handle_register(username, password, sender).await?;
            }
            MessageType::Login { username, password } => {
                self.handle_login(username, password, message.sender_id, sender)
                    .await?;
            }

            // --- Server Management ---
            MessageType::CreateServer { name, description } => {
                self.handle_create_server(name, description, message.sender_id, sender)
                    .await?;
            }
            MessageType::JoinServer { server_id } => {
                self.handle_join_server(*server_id, message.sender_id, sender)
                    .await?;
            }
            MessageType::LeaveServer { server_id } => {
                self.handle_leave_server(*server_id, message.sender_id, sender)
                    .await?;
            }
            MessageType::ListServers => {
                self.handle_list_servers(sender).await?;
            }

            // --- Channel Management ---
            MessageType::CreateChannel {
                server_id,
                name,
                description,
                channel_type,
                password,
            } => {
                self.handle_create_channel(
                    *server_id,
                    name,
                    description,
                    channel_type,
                    password,
                    message.sender_id,
                    sender,
                )
                .await?;
            }
            MessageType::JoinChannel {
                channel_id,
                password,
            } => {
                self.handle_join_channel(*channel_id, password, message.sender_id, sender)
                    .await?;
            }
            MessageType::LeaveChannel { channel_id } => {
                self.handle_leave_channel(*channel_id, message.sender_id, sender)
                    .await?;
            }
            MessageType::ListChannels { server_id } => {
                self.handle_list_channels(*server_id, sender).await?;
            }

            // --- Messaging ---
            MessageType::ChannelMessage {
                channel_id,
                sender_username: _,
                content,
            } => {
                self.handle_channel_message(
                    *channel_id,
                    content,
                    &sender_username,
                    message.sender_id,
                )
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

            // --- User Queries ---
            MessageType::RequestUserList => {
                self.handle_user_list_request(sender).await?;
            }
            MessageType::RequestServerMembers { server_id } => {
                self.handle_server_members_request(*server_id, sender)
                    .await?;
            }

            // --- Voice ---
            MessageType::RequestVoice { channel_id } => {
                self.handle_request_voice(*channel_id, message.sender_id, sender)
                    .await?;
            }

            _ => {}
        }
        Ok(())
    }

    // ========================================================================
    // AUTH HANDLERS
    // ========================================================================

    async fn handle_register(
        &self,
        username: &str,
        password: &str,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let password_hash = Self::hash_password(password);

        match self.database.create_user(username, &password_hash).await {
            Ok(user_id) => {
                log(
                    LogLevel::Auth,
                    &format!("New user registered: '{}' (ID: {})", username, user_id),
                );

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

                // Send servers sync (empty for new user)
                let sync = Message::new(
                    0,
                    MessageType::UserServersSync { servers: vec![] },
                );
                self.send_to_sender(&sender, sync)?;

                self.broadcast_user_list_update().await?;
            }
            Err(_) => {
                log(
                    LogLevel::Auth,
                    &format!("Registration failed: username '{}' already exists", username),
                );
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

    async fn handle_login(
        &self,
        username: &str,
        password: &str,
        _sender_id: u16,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(account) = self.database.get_user_by_username(username).await? {
            let password_hash = Self::hash_password(password);
            if account.hash == password_hash {
                log(
                    LogLevel::Auth,
                    &format!("User '{}' (ID: {}) logged in", username, account.user_id),
                );

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

                // Send user's servers + channels sync
                match self
                    .database
                    .get_user_servers_with_channels(account.user_id)
                    .await
                {
                    Ok(servers) => {
                        let sync =
                            Message::new(0, MessageType::UserServersSync { servers });
                        self.send_to_sender(&sender, sync)?;
                    }
                    Err(e) => {
                        log_error("Failed to load user servers for sync", &e);
                    }
                }

                self.broadcast_user_list_update().await?;
            } else {
                log(
                    LogLevel::Auth,
                    &format!("Login failed for '{}': invalid password", username),
                );
                let response = Message::new(
                    0,
                    MessageType::AuthFailure {
                        reason: "Invalid password".to_string(),
                    },
                );
                self.send_to_sender(&sender, response)?;
            }
        } else {
            log(
                LogLevel::Auth,
                &format!("Login failed: account '{}' does not exist", username),
            );
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

    // ========================================================================
    // SERVER (GUILD) HANDLERS
    // ========================================================================

    async fn handle_create_server(
        &self,
        name: &str,
        description: &str,
        creator_id: u16,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match self
            .database
            .create_server(name, description, creator_id)
            .await
        {
            Ok(server_id) => {
                log(
                    LogLevel::Server,
                    &format!(
                        "Server '{}' created (ID: {}) by user {}",
                        name, server_id, creator_id
                    ),
                );

                // Auto-create a #general text channel
                let _general_channel_id = self
                    .database
                    .create_channel(
                        server_id,
                        "general",
                        "General discussion",
                        &ChannelType::Text,
                        None,
                    )
                    .await?;

                // Auto-create a voice channel
                let _voice_channel_id = self
                    .database
                    .create_channel(
                        server_id,
                        "General Voice",
                        "Voice chat",
                        &ChannelType::Voice,
                        None,
                    )
                    .await?;

                let response = Message::new(
                    0,
                    MessageType::ServerCreated {
                        server_id,
                        name: name.to_string(),
                        description: description.to_string(),
                    },
                );
                self.send_to_sender(&sender, response)?;

                // Send full server joined info with auto-created channels
                let channels = self.database.get_server_channels(server_id).await?;
                let joined = Message::new(
                    0,
                    MessageType::ServerJoined {
                        server_id,
                        name: name.to_string(),
                        description: description.to_string(),
                        channels,
                    },
                );
                self.send_to_sender(&sender, joined)?;
            }
            Err(e) => {
                let response = Message::new(
                    0,
                    MessageType::ServerResponse {
                        success: false,
                        content: format!("Failed to create server: {}", e),
                    },
                );
                self.send_to_sender(&sender, response)?;
            }
        }

        Ok(())
    }

    async fn handle_join_server(
        &self,
        server_id: u16,
        user_id: u16,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(server) = self.database.get_server(server_id).await? {
            // Check if already a member
            if self.database.is_user_in_server(user_id, server_id).await? {
                let response = Message::new(
                    0,
                    MessageType::ServerResponse {
                        success: false,
                        content: "You are already a member of this server".to_string(),
                    },
                );
                self.send_to_sender(&sender, response)?;
                return Ok(());
            }

            self.database.add_user_to_server(user_id, server_id).await?;

            let channels = self.database.get_server_channels(server_id).await?;

            let response = Message::new(
                0,
                MessageType::ServerJoined {
                    server_id,
                    name: server.name.clone(),
                    description: server.description.clone(),
                    channels,
                },
            );
            self.send_to_sender(&sender, response)?;

            // Notify server members
            let username = if let Some(account) = self.database.get_user_by_id(user_id).await? {
                account.username
            } else {
                "Unknown".to_string()
            };

            // Get the first text channel for the join announcement
            let server_channels = self.database.get_server_channels(server_id).await?;
            if let Some(first_text) = server_channels
                .iter()
                .find(|c| c.channel_type == ChannelType::Text)
            {
                let join_msg = Message::new(
                    0,
                    MessageType::ChannelMessage {
                        channel_id: first_text.id,
                        sender_username: "System".to_string(),
                        content: format!("{} joined the server", username),
                    },
                );
                self.broadcast_to_server(server_id, join_msg).await?;
            }

            log(
                LogLevel::Server,
                &format!(
                    "User '{}' (ID: {}) joined server '{}' (ID: {})",
                    username, user_id, server.name, server_id
                ),
            );
        } else {
            let response = Message::new(
                0,
                MessageType::ServerResponse {
                    success: false,
                    content: format!("Server {} not found", server_id),
                },
            );
            self.send_to_sender(&sender, response)?;
        }

        Ok(())
    }

    async fn handle_leave_server(
        &self,
        server_id: u16,
        user_id: u16,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.database
            .remove_user_from_server(user_id, server_id)
            .await?;

        let response = Message::new(0, MessageType::ServerLeft { server_id });
        self.send_to_sender(&sender, response)?;

        let username = if let Some(account) = self.database.get_user_by_id(user_id).await? {
            account.username
        } else {
            "Unknown".to_string()
        };

        log(
            LogLevel::Server,
            &format!("User '{}' left server {}", username, server_id),
        );

        Ok(())
    }

    async fn handle_list_servers(
        &self,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let servers = self.database.get_all_servers().await?;
        let response = Message::new(0, MessageType::ServerList { servers });
        self.send_to_sender(&sender, response)?;
        Ok(())
    }

    // ========================================================================
    // CHANNEL HANDLERS
    // ========================================================================

    async fn handle_create_channel(
        &self,
        server_id: u16,
        name: &str,
        description: &str,
        channel_type: &ChannelType,
        password: &Option<String>,
        creator_id: u16,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Verify creator is a member of the server
        if !self
            .database
            .is_user_in_server(creator_id, server_id)
            .await?
        {
            let response = Message::new(
                0,
                MessageType::ServerResponse {
                    success: false,
                    content: "You must be a member of the server to create channels".to_string(),
                },
            );
            self.send_to_sender(&sender, response)?;
            return Ok(());
        }

        let password_hash = password.as_ref().map(|p| Self::hash_password(p));

        match self
            .database
            .create_channel(
                server_id,
                name,
                description,
                channel_type,
                password_hash.as_deref(),
            )
            .await
        {
            Ok(channel_id) => {
                log(
                    LogLevel::Channel,
                    &format!(
                        "Channel '{}' ({}) created in server {} by user {}",
                        name,
                        channel_type.as_str(),
                        server_id,
                        creator_id
                    ),
                );

                let response = Message::new(
                    0,
                    MessageType::ChannelCreated {
                        server_id,
                        channel_id,
                        name: name.to_string(),
                        description: description.to_string(),
                        channel_type: channel_type.clone(),
                    },
                );
                // Broadcast to all server members
                self.broadcast_to_server(server_id, response).await?;
            }
            Err(e) => {
                let response = Message::new(
                    0,
                    MessageType::ServerResponse {
                        success: false,
                        content: format!("Failed to create channel: {}", e),
                    },
                );
                self.send_to_sender(&sender, response)?;
            }
        }

        Ok(())
    }

    async fn handle_join_channel(
        &self,
        channel_id: u16,
        password: &Option<String>,
        user_id: u16,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(channel) = self.database.get_channel(channel_id).await? {
            // Verify user is in the server
            if !self
                .database
                .is_user_in_server(user_id, channel.server_id)
                .await?
            {
                let response = Message::new(
                    0,
                    MessageType::ServerResponse {
                        success: false,
                        content: "You must join the server first".to_string(),
                    },
                );
                self.send_to_sender(&sender, response)?;
                return Ok(());
            }

            // Check password if needed
            if channel.is_password_protected() {
                let valid = match password {
                    Some(pass) => {
                        let password_hash = Self::hash_password(pass);
                        channel.verify_password(&password_hash)
                    }
                    None => false,
                };
                if !valid {
                    let response = Message::new(
                        0,
                        MessageType::ServerResponse {
                            success: false,
                            content: "Invalid channel password".to_string(),
                        },
                    );
                    self.send_to_sender(&sender, response)?;
                    return Ok(());
                }
            }

            // Send join confirmation
            let response = Message::new(
                0,
                MessageType::ChannelJoined {
                    server_id: channel.server_id,
                    channel_id,
                    name: channel.name.clone(),
                    description: channel.description.clone(),
                    channel_type: channel.channel_type.clone(),
                },
            );
            self.send_to_sender(&sender, response)?;

            // Send message history for text channels
            if channel.channel_type == ChannelType::Text {
                match self.database.get_recent_messages(channel_id).await {
                    Ok(messages) => {
                        for stored in messages {
                            let history = Message::new(
                                stored.sender_id,
                                MessageType::ChannelMessage {
                                    channel_id,
                                    sender_username: stored.sender_username,
                                    content: format!(
                                        "[{}] {}",
                                        stored.timestamp.format("%H:%M"),
                                        stored.content
                                    ),
                                },
                            );
                            self.send_to_sender(&sender, history)?;
                        }

                        let separator = Message::new(
                            0,
                            MessageType::ChannelMessage {
                                channel_id,
                                sender_username: "System".to_string(),
                                content: "── end of message history ──".to_string(),
                            },
                        );
                        self.send_to_sender(&sender, separator)?;
                    }
                    Err(e) => {
                        eprintln!(
                            "Failed to load message history for channel {}: {}",
                            channel_id, e
                        );
                    }
                }
            }
        } else {
            let response = Message::new(
                0,
                MessageType::ServerResponse {
                    success: false,
                    content: format!("Channel {} not found", channel_id),
                },
            );
            self.send_to_sender(&sender, response)?;
        }

        Ok(())
    }

    async fn handle_leave_channel(
        &self,
        channel_id: u16,
        _user_id: u16,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let response = Message::new(
            0,
            MessageType::ServerResponse {
                success: true,
                content: format!("Left channel {}", channel_id),
            },
        );
        self.send_to_sender(&sender, response)?;
        Ok(())
    }

    async fn handle_list_channels(
        &self,
        server_id: u16,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let channels = self.database.get_server_channels(server_id).await?;
        let response = Message::new(
            0,
            MessageType::ChannelList {
                server_id,
                channels,
            },
        );
        self.send_to_sender(&sender, response)?;
        Ok(())
    }

    // ========================================================================
    // MESSAGING HANDLERS
    // ========================================================================

    async fn handle_channel_message(
        &self,
        channel_id: u16,
        content: &str,
        sender_username: &str,
        sender_id: u16,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Look up the channel to find the server_id
        if let Some(channel) = self.database.get_channel(channel_id).await? {
            self.database
                .save_message(channel_id, sender_id, sender_username, content, "user")
                .await?;

            let message = Message::new(
                sender_id,
                MessageType::ChannelMessage {
                    channel_id,
                    sender_username: sender_username.to_string(),
                    content: content.to_string(),
                },
            );

            // Broadcast to all server members (they filter by active channel on client)
            self.broadcast_to_server(channel.server_id, message).await?;
        }

        Ok(())
    }

    async fn handle_private_message(
        &self,
        to_user_id: u16,
        content: &str,
        sender_username: &str,
        sender_id: u16,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // DMs use channel_id 0
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

    // ========================================================================
    // USER QUERY HANDLERS
    // ========================================================================

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

    async fn handle_server_members_request(
        &self,
        server_id: u16,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let member_ids = self.database.get_server_member_ids(server_id).await?;
        let all_sessions = self.database.get_online_users().await?;

        let members: Vec<UserStatus> = member_ids
            .iter()
            .filter_map(|&uid| {
                all_sessions.iter().find(|s| s.user_id == uid).map(|s| UserStatus {
                    user_id: s.user_id,
                    username: s.username.clone(),
                    is_online: s.is_online,
                    last_seen: s.last_seen.format("%Y-%m-%d %H:%M:%S").to_string(),
                })
            })
            .collect();

        let response = Message::new(
            0,
            MessageType::ServerMemberList {
                server_id,
                members,
            },
        );
        self.send_to_sender(&sender, response)?;
        Ok(())
    }

    // ========================================================================
    // VOICE HANDLER
    // ========================================================================

    async fn handle_request_voice(
        &self,
        channel_id: u16,
        user_id: u16,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Verify channel exists and is a voice channel
        if let Some(channel) = self.database.get_channel(channel_id).await? {
            if channel.channel_type != ChannelType::Voice {
                let response = Message::new(
                    0,
                    MessageType::VoiceError {
                        message: "Not a voice channel".to_string(),
                    },
                );
                self.send_to_sender(&sender, response)?;
                return Ok(());
            }

            let token: String = rand::thread_rng()
                .sample_iter(&Alphanumeric)
                .take(16)
                .map(char::from)
                .collect();

            let mut tokens = self.voice_tokens.write().await;
            tokens.insert(
                token.clone(),
                VoiceTokenData {
                    user_id,
                    channel_id,
                },
            );

            let response = Message::new(
                0,
                MessageType::VoiceCredentials {
                    token,
                    udp_port: 8081,
                    channel_id,
                },
            );
            self.send_to_sender(&sender, response)?;

            log(
                LogLevel::Voice,
                &format!(
                    "Voice token issued for user {} in channel {}",
                    user_id, channel_id
                ),
            );
        } else {
            let response = Message::new(
                0,
                MessageType::VoiceError {
                    message: format!("Channel {} not found", channel_id),
                },
            );
            self.send_to_sender(&sender, response)?;
        }

        Ok(())
    }

    // ========================================================================
    // UTILITY METHODS
    // ========================================================================

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

    fn hash_password(password: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(password);
        format!("{:x}", hasher.finalize())
    }

    /// Broadcast to all members of a server
    async fn broadcast_to_server(
        &self,
        server_id: u16,
        message: Message,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let member_ids = self.database.get_server_member_ids(server_id).await?;
        let user_senders = self.user_senders.read().await;
        let message_json = message.to_json_file()?;

        for member_id in member_ids {
            if let Some(sender) = user_senders.get(&member_id) {
                let _ = sender.send(message_json.clone());
            }
        }

        Ok(())
    }

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

    async fn cleanup_disconnected_users(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut user_senders = self.user_senders.write().await;
        let mut disconnected_users = Vec::new();

        for (&user_id, sender) in user_senders.iter() {
            if sender.is_closed() {
                disconnected_users.push(user_id);
            }
        }

        if !disconnected_users.is_empty() {
            log(
                LogLevel::Debug,
                &format!("Cleaning up {} stale connections", disconnected_users.len()),
            );
        }

        for user_id in disconnected_users {
            user_senders.remove(&user_id);

            if let Some(account) = self.database.get_user_by_id(user_id).await.unwrap_or(None) {
                let _ = self
                    .database
                    .set_user_online_status(user_id, &account.username, false)
                    .await;
                log(
                    LogLevel::Auth,
                    &format!(
                        "User '{}' marked offline (stale connection)",
                        account.username
                    ),
                );
            }
        }

        Ok(())
    }
}
