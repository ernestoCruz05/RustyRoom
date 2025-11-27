#![allow(dead_code)]

use crate::database::Database;
use crate::resc::{Message, MessageType, Room, RoomState, User, UserStatus};
use rand::{Rng, distributions::Alphanumeric};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{RwLock, mpsc};

pub struct AsyncChatServer {
    users: Arc<RwLock<HashMap<u16, User>>>,
    rooms: Arc<RwLock<HashMap<u16, Room>>>,
    user_senders: Arc<RwLock<HashMap<u16, mpsc::UnboundedSender<String>>>>,
    database: Database,
    next_user_id: Arc<RwLock<u16>>,
    next_room_id: Arc<RwLock<u16>>,
    voice_tokens: Arc<RwLock<HashMap<String, u16>>>,
}

impl AsyncChatServer {
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

    pub async fn start_async_server(
        database_url: &str,
        address: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let server = match AsyncChatServer::new(database_url).await {
            Ok(server) => {
                println!(" Database connection successful: {}", database_url);
                server
            }
            Err(e) => {
                eprintln!(" Failed to connect to database: {}", e);
                eprintln!(" Trying in-memory database as fallback...");

                match AsyncChatServer::new("sqlite::memory:").await {
                    Ok(server) => {
                        println!(" Using in-memory database (data will not persist)");
                        server
                    }
                    Err(e2) => {
                        eprintln!(" Even in-memory database failed: {}", e2);
                        return Err(e);
                    }
                }
            }
        };

        let server = Arc::new(server);

        if let Err(e) = server.load_rooms_from_db().await {
            eprintln!(
                "  Warning: Could not load existing rooms from database: {}",
                e
            );
        }

        let server_cleanup = Arc::clone(&server);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
            loop {
                interval.tick().await;

                if let Err(e) = server_cleanup.cleanup_disconnected_users().await {
                    eprintln!("Failed to cleanup disconnected users: {}", e);
                }

                if let Err(e) = server_cleanup.database.cleanup_old_sessions().await {
                    eprintln!("Failed to cleanup old sessions: {}", e);
                }

                if let Err(e) = server_cleanup.broadcast_user_list_update().await {
                    eprintln!("Failed to broadcast user list update: {}", e);
                }
            }
        });

        let udp_address = format!("{}:8081", "0.0.0.0");
        let voice_tokens_clone = server.voice_tokens.clone();
        tokio::spawn(async move {
            if let Err(e) =
                AsyncChatServer::start_udp_listener(voice_tokens_clone, udp_address).await
            {
                eprintln!("Critical Voice Server Error: {}", e);
            }
        });

        let tcp_listener = TcpListener::bind(address).await?;
        println!("Async server listening to [{}] with database", address);

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

    pub async fn start_udp_listener(
        voice_tokens: Arc<RwLock<HashMap<String, u16>>>,
        bind_address: String,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let udp_socket = UdpSocket::bind(&bind_address).await?;
        println!("Voice server (UDP) listening on {}", bind_address);

        let mut active_voice_sessions: HashMap<std::net::SocketAddr, u16> = HashMap::new();
        let mut buf = [0u8; 8192];

        loop {
            match udp_socket.recv_from(&mut buf).await {
                Ok((size, peer_addr)) => {
                    let received_data = &buf[..size];

                    if let Ok(token_str) = std::str::from_utf8(received_data) {
                        let token = token_str.trim().to_string();
                        let mut tokens = voice_tokens.write().await;

                        if let Some(user_id) = tokens.remove(&token) {
                            active_voice_sessions.insert(peer_addr, user_id);
                            let _ = udp_socket.send_to(b"VOICE_CONNECTED", peer_addr).await;
                            continue;
                        }
                    }

                    if let Some(_user_id) = active_voice_sessions.get(&peer_addr) {
                        let _ = udp_socket.send_to(received_data, peer_addr).await;
                    }
                }
                Err(e) => eprintln!("UDP Recv Error: {}", e),
            }
        }
    }

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

    async fn handle_client_async(
        &self,
        stream: TcpStream,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let client_address = stream.peer_addr()?;
        println!("Connection: [{}]", client_address);

        let (read_half, mut write_half) = stream.into_split();
        let reader = BufReader::new(read_half);
        let mut lines = reader.lines();

        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        let mut connected_user_id: Option<u16> = None;

        let write_task = tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                if let Err(e) = write_half.write_all(message.as_bytes()).await {
                    eprintln!(
                        "Failed to write to client: {} - connection likely broken",
                        e
                    );
                    break;
                }
                if let Err(e) = write_half.write_all(b"\n").await {
                    eprintln!(
                        "Failed to write newline to client: {} - connection likely broken",
                        e
                    );
                    break;
                }
                if let Err(e) = write_half.flush().await {
                    eprintln!(
                        "Failed to flush client stream: {} - connection likely broken",
                        e
                    );
                    break;
                }
            }
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
                println!("User {} ({}) marked as offline", account.username, user_id);

                let _ = self.broadcast_user_list_update().await;
            }
            let mut user_senders = self.user_senders.write().await;
            user_senders.remove(&user_id);
        }

        write_task.abort();
        println!("Disconnected: [{}]", client_address);
        Ok(())
    }

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

    async fn handle_register(
        &self,
        username: &str,
        password: &str,
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let password_hash = Self::hash_password(password);

        match self.database.create_user(username, &password_hash).await {
            Ok(user_id) => {
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
        sender: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(account) = self.database.get_user_by_username(username).await? {
            let password_hash = Self::hash_password(password);
            if account.hash == password_hash {
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
                let response = Message::new(
                    0,
                    MessageType::AuthFailure {
                        reason: "Invalid password".to_string(),
                    },
                );
                self.send_to_sender(&sender, response)?;
            }
        } else {
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

        for user_id in disconnected_users {
            user_senders.remove(&user_id);

            if let Some(account) = self.database.get_user_by_id(user_id).await.unwrap_or(None) {
                let _ = self
                    .database
                    .set_user_online_status(user_id, &account.username, false)
                    .await;
                println!(
                    "User {} ({}) automatically marked as offline due to disconnection",
                    account.username, user_id
                );
            }
        }

        Ok(())
    }
}
