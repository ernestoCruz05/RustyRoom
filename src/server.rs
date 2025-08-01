#![allow(dead_code)]

use crate::resc::{User, Room, Account, RoomState, Message, MessageType, UserStatus, RoomInfo};
use crate::database::Database;
use std::collections::HashMap;
use std::net::{TcpListener, TcpStream, SocketAddr};
use std::sync::{Arc, Mutex};
use std::thread;
use std::io::{BufRead, BufReader, Write};
use sha2::{Sha256, Digest};
use tokio::sync::RwLock;

pub struct ChatServer { 
    users: HashMap<u16, User>,
    rooms: HashMap<u16, Room>,
    user_streams: HashMap<u16, TcpStream>,
    accounts: HashMap<String, Account>,
    unauthenticated_connections: HashMap<SocketAddr, TcpStream>, 
    next_user_id: u16,
    next_room_id: u16,
}

pub struct AsyncChatServer {
    users: Arc<RwLock<HashMap<u16, User>>>,
    rooms: Arc<RwLock<HashMap<u16, Room>>>,
    user_streams: Arc<RwLock<HashMap<u16, TcpStream>>>,
    database: Database,
    next_user_id: Arc<RwLock<u16>>,
    next_room_id: Arc<RwLock<u16>>,
}

impl ChatServer{
    pub fn new() -> Self {
        ChatServer {
            users: HashMap::new(),
            rooms: HashMap::new(),
            user_streams: HashMap::new(),
            accounts: HashMap::new(),
            unauthenticated_connections: HashMap::new(), 
            next_user_id: 1,
            next_room_id: 1,
        }
    }

    fn hash_password(password: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(password);
        format!("{:x}", hasher.finalize())
    }

    pub fn register_account(&mut self, username: String, password: String) -> Result<u16, String> {
        if self.accounts.contains_key(&username) {
            return Err("Username already exists".to_string());
        }

        let user_id = self.next_user_id;
        let password_hash = Self::hash_password(&password);
        let account = Account {
            username: username.clone(),
            hash: password_hash,
            user_id,
        };

        self.accounts.insert(username, account);
        self.next_user_id += 1;

        Ok(user_id)
    }        

 pub fn login_account(&self, username: &str, password: &str) -> Result<u16, String> {
    
    if let Some(account) = self.accounts.get(username){
        let password_hash = Self::hash_password(password);
        if account.hash == password_hash {
            Ok(account.user_id)
        } else {
            Err("Invalid password".to_string())
        }
    } else {
        Err("Account does not exist".to_string())
    }
    
 }

    pub fn create_authenticated_user(&mut self, user_id: u16, username: String, address: SocketAddr, stream: TcpStream) {
        let account = self.accounts.get(&username).unwrap();
        let mut user = User::new(user_id, username, account.hash.clone(), address, false);
        user.authed();
        
        self.users.insert(user_id, user);
        self.user_streams.insert(user_id, stream);
    }


    pub fn add_user(&mut self, _username: String, _address: SocketAddr) -> u16 {
        let user_id = self.next_user_id;
        user_id
    }

    pub fn start_server(address: &str) -> std::io::Result<()> {
        let tcp_listener = TcpListener::bind(address)?;
        let server = Arc::new(Mutex::new(ChatServer::new()));

        println!("Server listening to [{}]", address);

        for stream in tcp_listener.incoming(){
            match stream{
                Ok(stream) => {
                    let server_clone = Arc::clone(&server);
                    thread::spawn(move || {
                        Self::client_handler(server_clone, stream);
                    });
                }
                Err(e) => {
                    eprintln!("Failed to accept connection: {}", e);
                }
            }
        }

        Ok(())

    }

    fn client_handler(server: Arc<Mutex<ChatServer>>, stream: TcpStream) {
        let client_address = stream.peer_addr().unwrap();
        println!("Connection: [{}]", client_address);

        {
            let mut server_lock = server.lock().unwrap();
            server_lock.unauthenticated_connections.insert(client_address, stream.try_clone().unwrap());
        }

        let reader = BufReader::new(stream.try_clone().unwrap());

        for line in reader.lines(){
            match line{
                Ok(json_message) => {
                    if let Ok(message) = Message::from_json_file(&json_message){
                        Self::digest_message(Arc::clone(&server), client_address, message);                    }
                }
                Err(_) => {
                    break;
                }
            }
        }
        
        {
            let mut server_lock = server.lock().unwrap();
            server_lock.unauthenticated_connections.remove(&client_address);
            
            let user_to_remove: Option<u16> = server_lock.users.values()
                .find(|u| u.address == client_address)
                .map(|u| u.id);
                
            if let Some(user_id) = user_to_remove {
                server_lock.remove_user(user_id);
                server_lock.user_streams.remove(&user_id);
            }
        }
        println!("Disconnected: [{}]", client_address);
    }

    fn digest_message(server: Arc<Mutex<ChatServer>>, client_addr: SocketAddr, message: Message) {
        let mut server_lock = server.lock().unwrap();

        match message.message_type {
            MessageType::Register { username, password } => {
                match server_lock.register_account(username.clone(), password) {
                    Ok(user_id) => {
                        if let Some(stream) = server_lock.unauthenticated_connections.remove(&client_addr) {
                            server_lock.create_authenticated_user(user_id, username.clone(), client_addr, stream);
                        }
                        
                        let response = Message::new(0, MessageType::AuthSuccess { 
                            user_id, 
                            message: format!("Registration successful! Welcome, {}", username) 
                        });
                        Self::send_to_user(&server_lock, user_id, response);
                    }
                    Err(error) => {
                        let response = Message::new(0, MessageType::AuthFailure { reason: error });
                        Self::send_to_address(&server_lock, client_addr, response);
                    }
                }
            }
            MessageType::Login { username, password } => {
                match server_lock.login_account(&username, &password) {
                    Ok(user_id) => {
                        if let Some(stream) = server_lock.unauthenticated_connections.remove(&client_addr) {
                            server_lock.create_authenticated_user(user_id, username.clone(), client_addr, stream);
                        }
                        
                        let response = Message::new(0, MessageType::AuthSuccess { 
                            user_id, 
                            message: format!("Login successful! Welcome back, {}", username) 
                        });
                        Self::send_to_user(&server_lock, user_id, response);
                    }
                    Err(error) => {
                        let response = Message::new(0, MessageType::AuthFailure { reason: error });
                        Self::send_to_address(&server_lock, client_addr, response);
                    }
                }
            }
            _ => {
                if let Some(user) = server_lock.users.values().find(|u| u.address == client_addr && u.is_auth) {
                    let user_id = user.id;
                    match message.message_type {
                        MessageType::Join { room_id, password } => {
                            if !server_lock.rooms.contains_key(&room_id) {
                                let response = Message::new(0, MessageType::RoomNotFound { room_id });
                                Self::send_to_user(&server_lock, user_id, response);
                            } else {
                                let room = server_lock.rooms.get(&room_id).cloned().unwrap();
                                
                                let can_join = if room.is_password_protected() {
                                    match password {
                                        Some(pass) => {
                                            let password_hash = Self::hash_password(&pass);
                                            room.verify_password(&password_hash)
                                        }
                                        None => false,
                                    }
                                } else {
                                    true
                                };
                                
                                if can_join {
                                    if let Some(user) = server_lock.users.get_mut(&user_id) {
                                        user.subscribe_room(room.clone());
                                        let response = Message::new(0, MessageType::RoomJoined { 
                                            room_id: room.id,
                                            name: room.name.clone(),
                                            description: room.description.clone(),
                                        });
                                        Self::send_to_user(&server_lock, user_id, response);
                                    }
                                } else {
                                    let response = Message::new(0, MessageType::ServerResponse { 
                                        success: false, 
                                        content: "Invalid room password".to_string() 
                                    });
                                    Self::send_to_user(&server_lock, user_id, response);
                                }
                            }
                        }
                        MessageType::CreateRoom { room_id, name, description, password } => {
                            if server_lock.rooms.contains_key(&room_id) {
                                let response = Message::new(0, MessageType::ServerResponse { 
                                    success: false, 
                                    content: format!("Room {} already exists", room_id) 
                                });
                                Self::send_to_user(&server_lock, user_id, response);
                            } else {
                                let room = if let Some(pass) = password {
                                    let password_hash = Self::hash_password(&pass);
                                    Room::new_with_password(room_id, name.clone(), description.clone(), password_hash)
                                } else {
                                    Room::new(room_id, name.clone(), description.clone(), RoomState::Open)
                                };
                                
                                server_lock.rooms.insert(room_id, room.clone());
                                server_lock.next_room_id = server_lock.next_room_id.max(room_id + 1);
                                
                                if let Some(user) = server_lock.users.get_mut(&user_id) {
                                    user.subscribe_room(room.clone());
                                }
                                
                                let response = Message::new(0, MessageType::RoomCreated { 
                                    room_id,
                                    name: name.clone(),
                                    description: description.clone(),
                                });
                                Self::send_to_user(&server_lock, user_id, response);
                                
                                let join_response = Message::new(0, MessageType::RoomJoined { 
                                    room_id,
                                    name,
                                    description,
                                });
                                Self::send_to_user(&server_lock, user_id, join_response);
                            }
                        }
                        MessageType::Leave { room_id } => {
                            if let Some(user) = server_lock.users.get_mut(&user_id) {
                                user.unsubscribe_room(room_id);
                                let response = Message::new(0, MessageType::ServerResponse { 
                                    success: true, 
                                    content: format!("Left room {}", room_id) 
                                });
                                Self::send_to_user(&server_lock, user_id, response);
                            }
                        }
                        MessageType::RoomMessage { room_id, sender_username: _, content } => {
                            let users_in_room: Vec<u16> = server_lock.users.values()
                                .filter(|u| u.subscribed_rooms.iter().any(|r| r.id == room_id))
                                .map(|u| u.id)
                                .collect();
                            
                            let sender_username = server_lock.users.get(&user_id)
                                .map(|u| u.username.clone())
                                .unwrap_or_else(|| format!("User {}", user_id));
                            let broadcast_message = Message::new(user_id, MessageType::RoomMessage { room_id, sender_username, content });
                            
                            for target_user_id in users_in_room {
                                Self::send_to_user(&server_lock, target_user_id, broadcast_message.clone());
                            }
                        }
                        MessageType::PrivateMessage { to_user_id, sender_username: _, content } => {
                            if server_lock.users.contains_key(&to_user_id) {

                                let sender_username = server_lock.users.get(&user_id)
                                    .map(|u| u.username.clone())
                                    .unwrap_or_else(|| format!("User {}", user_id));

                                let private_message = Message::new(user_id, MessageType::PrivateMessage { to_user_id, sender_username, content });
                                Self::send_to_user(&server_lock, to_user_id, private_message);
                            } else {
                                let response = Message::new(0, MessageType::ServerResponse { 
                                    success: false, 
                                    content: format!("User {} not found", to_user_id) 
                                });
                                Self::send_to_user(&server_lock, user_id, response);
                            }
                        }
                        MessageType::ListRooms => {
                            let rooms: Vec<RoomInfo> = server_lock.rooms.values()
                                .map(|room| RoomInfo {
                                    id: room.id,
                                    name: room.name.clone(),
                                    description: room.description.clone(),
                                    is_password_protected: room.is_password_protected(),
                                })
                                .collect();
                            
                            let response = Message::new(0, MessageType::RoomList { rooms });
                            Self::send_to_user(&server_lock, user_id, response);
                        }
                        MessageType::RequestUserList => {
                            let users: Vec<UserStatus> = server_lock.users.values()
                                .map(|user| UserStatus {
                                    user_id: user.id,
                                    username: user.username.clone(),
                                    is_online: true,
                                    last_seen: "now".to_string(),
                                })
                                .collect();
                            
                            let response = Message::new(0, MessageType::UserListUpdate { users });
                            Self::send_to_user(&server_lock, user_id, response);
                        }
                        _ => {}
                    }
                } else {
                    let response = Message::new(0, MessageType::AuthFailure { 
                        reason: "Please login or register first".to_string() 
                    });
                    Self::send_to_address(&server_lock, client_addr, response);
                }
            }
        }
    }

    fn send_to_address(server: &ChatServer, addr: SocketAddr, message: Message) {
        if let Some(stream) = server.unauthenticated_connections.get(&addr) {
            if let Ok(json) = message.to_json_file() {
                let mut stream_clone = stream.try_clone().unwrap();
                let _ = writeln!(stream_clone, "{}", json);
                let _ = stream_clone.flush();
            }
        }
    }

    fn send_to_user(server: &ChatServer, user_id: u16, message: Message) {
        if let Some(stream) = server.user_streams.get(&user_id) {
            if let Ok(json) = message.to_json_file() {
                let mut stream_clone = stream.try_clone().unwrap();
                let _ = writeln!(stream_clone, "{}", json);
                let _ = stream_clone.flush();
            }
        }
    }

    pub fn remove_user(&mut self, user_id: u16) -> Option<User> {
                self.users.remove(&user_id)
    }

}

impl AsyncChatServer {
    pub async fn new(database_url: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let database = Database::new(database_url).await?;
        
        Ok(AsyncChatServer {
            users: Arc::new(RwLock::new(HashMap::new())),
            rooms: Arc::new(RwLock::new(HashMap::new())),
            user_streams: Arc::new(RwLock::new(HashMap::new())),
            database,
            next_user_id: Arc::new(RwLock::new(1)),
            next_room_id: Arc::new(RwLock::new(1)),
        })
    }

    pub async fn start_async_server(database_url: &str, address: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Try to create the server with the provided database URL
        let server = match AsyncChatServer::new(database_url).await {
            Ok(server) => {
                println!("✅ Database connection successful: {}", database_url);
                server
            }
            Err(e) => {
                eprintln!("❌ Failed to connect to database: {}", e);
                eprintln!("🔄 Trying in-memory database as fallback...");
                
                // Fallback to in-memory database
                match AsyncChatServer::new("sqlite::memory:").await {
                    Ok(server) => {
                        println!("✅ Using in-memory database (data will not persist)");
                        server
                    }
                    Err(e2) => {
                        eprintln!("❌ Even in-memory database failed: {}", e2);
                        return Err(e);
                    }
                }
            }
        };
        
        let server = Arc::new(server);
        
        // Load existing rooms from database
        if let Err(e) = server.load_rooms_from_db().await {
            eprintln!("⚠️  Warning: Could not load existing rooms from database: {}", e);
        }
        
        let tcp_listener = TcpListener::bind(address)?;
        println!("Async server listening to [{}] with database", address);

        for stream in tcp_listener.incoming() {
            match stream {
                Ok(stream) => {
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

        Ok(())
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
                }
            );
            rooms.insert(room_info.id, room);
            *next_room_id = (*next_room_id).max(room_info.id + 1);
        }
        
        Ok(())
    }

    async fn handle_client_async(&self, stream: TcpStream) -> Result<(), Box<dyn std::error::Error>> {
        let client_address = stream.peer_addr()?;
        println!("Connection: [{}]", client_address);

        let reader = BufReader::new(stream.try_clone()?);
        
        for line in reader.lines() {
            match line {
                Ok(json_message) => {
                    if let Ok(message) = Message::from_json_file(&json_message) {
                        self.process_message_async(message, &stream).await?;
                    }
                }
                Err(_) => break,
            }
        }

        println!("Disconnected: [{}]", client_address);
        Ok(())
    }

    async fn process_message_async(&self, message: Message, stream: &TcpStream) -> Result<(), Box<dyn std::error::Error>> {
        // Get sender username from database
        let sender_username = if let Some(account) = self.database.get_user_by_id(message.sender_id).await? {
            account.username
        } else {
            "Unknown".to_string()
        };

        match &message.message_type {
            MessageType::Register { username, password } => {
                self.handle_register(username, password, stream).await?;
            }
            MessageType::Login { username, password } => {
                self.handle_login(username, password, stream).await?;
            }
            MessageType::ListRooms => {
                self.handle_list_rooms(stream).await?;
            }
            MessageType::RequestUserList => {
                self.handle_user_list_request(stream).await?;
            }
            MessageType::CreateRoom { room_id, name, description, password } => {
                self.handle_create_room(*room_id, name, description, password, message.sender_id, stream).await?;
            }
            MessageType::RoomMessage { room_id, sender_username: _, content } => {
                self.handle_room_message(*room_id, content, &sender_username, message.sender_id, stream).await?;
            }
            MessageType::PrivateMessage { to_user_id, sender_username: _, content } => {
                self.handle_private_message(*to_user_id, content, &sender_username, message.sender_id, stream).await?;
            }
            MessageType::Join { room_id, password } => {
                self.handle_join_room(*room_id, password, message.sender_id, stream).await?;
            }
            _ => {
                // Handle other message types...
            }
        }
        Ok(())
    }

    async fn handle_register(&self, username: &str, password: &str, stream: &TcpStream) -> Result<(), Box<dyn std::error::Error>> {
        let password_hash = Self::hash_password(password);
        
        match self.database.create_user(username, &password_hash).await {
            Ok(user_id) => {
                // Store user stream for broadcasting
                let mut user_streams = self.user_streams.write().await;
                user_streams.insert(user_id, stream.try_clone()?);
                
                let response = Message::new(0, MessageType::AuthSuccess {
                    user_id,
                    message: format!("Registration successful! Welcome, {}", username),
                });
                self.send_to_stream(stream, response)?;
            }
            Err(_) => {
                let response = Message::new(0, MessageType::AuthFailure {
                    reason: "Username already exists".to_string(),
                });
                self.send_to_stream(stream, response)?;
            }
        }
        
        Ok(())
    }

    async fn handle_login(&self, username: &str, password: &str, stream: &TcpStream) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(account) = self.database.get_user_by_username(username).await? {
            let password_hash = Self::hash_password(password);
            if account.hash == password_hash {
                // Update user online status
                self.database.set_user_online_status(account.user_id, &account.username, true).await?;
                
                // Store user stream for broadcasting
                let mut user_streams = self.user_streams.write().await;
                user_streams.insert(account.user_id, stream.try_clone()?);
                
                let response = Message::new(0, MessageType::AuthSuccess {
                    user_id: account.user_id,
                    message: format!("Login successful! Welcome back, {}", username),
                });
                self.send_to_stream(stream, response)?;
            } else {
                let response = Message::new(0, MessageType::AuthFailure {
                    reason: "Invalid password".to_string(),
                });
                self.send_to_stream(stream, response)?;
            }
        } else {
            let response = Message::new(0, MessageType::AuthFailure {
                reason: "Account does not exist".to_string(),
            });
            self.send_to_stream(stream, response)?;
        }
        
        Ok(())
    }

    async fn handle_list_rooms(&self, stream: &TcpStream) -> Result<(), Box<dyn std::error::Error>> {
        let room_infos = self.database.get_all_rooms().await?;
        let response = Message::new(0, MessageType::RoomList { rooms: room_infos });
        self.send_to_stream(stream, response)?;
        Ok(())
    }

    async fn handle_user_list_request(&self, stream: &TcpStream) -> Result<(), Box<dyn std::error::Error>> {
        let user_sessions = self.database.get_online_users().await?;
        let user_statuses: Vec<UserStatus> = user_sessions.into_iter().map(|session| {
            UserStatus {
                user_id: session.user_id,
                username: session.username,
                is_online: session.is_online,
                last_seen: session.last_seen.format("%Y-%m-%d %H:%M:%S").to_string(),
            }
        }).collect();
        
        let response = Message::new(0, MessageType::UserListUpdate { users: user_statuses });
        self.send_to_stream(stream, response)?;
        Ok(())
    }

    async fn handle_create_room(&self, room_id: u16, name: &str, description: &str, password: &Option<String>, creator_id: u16, stream: &TcpStream) -> Result<(), Box<dyn std::error::Error>> {
        let password_hash = password.as_ref().map(|p| Self::hash_password(p));
        let room_state = if password.is_some() {
            RoomState::Private
        } else {
            RoomState::Open
        };
        
        match self.database.create_room(room_id, name, description, &room_state, password_hash.as_deref(), creator_id).await {
            Ok(_) => {
                // Add to in-memory rooms
                let mut rooms = self.rooms.write().await;
                let room = if let Some(pass_hash) = password_hash {
                    Room::new_with_password(room_id, name.to_string(), description.to_string(), pass_hash)
                } else {
                    Room::new(room_id, name.to_string(), description.to_string(), RoomState::Open)
                };
                rooms.insert(room_id, room);
                
                let response = Message::new(0, MessageType::RoomCreated {
                    room_id,
                    name: name.to_string(),
                    description: description.to_string(),
                });
                self.send_to_stream(stream, response)?;
            }
            Err(_) => {
                let response = Message::new(0, MessageType::ServerResponse {
                    success: false,
                    content: format!("Failed to create room {}", room_id),
                });
                self.send_to_stream(stream, response)?;
            }
        }
        
        Ok(())
    }

    async fn handle_room_message(&self, room_id: u16, content: &str, sender_username: &str, sender_id: u16, stream: &TcpStream) -> Result<(), Box<dyn std::error::Error>> {
        // Save message to database
        self.database.save_message(room_id, sender_id, sender_username, content, "user").await?;
        
        // Create the message to broadcast
        let message = Message::new(sender_id, MessageType::RoomMessage {
            room_id,
            sender_username: sender_username.to_string(),
            content: content.to_string(),
        });
        
        // Broadcast to all users in the room
        self.broadcast_to_room(room_id, message).await?;
        
        Ok(())
    }

    async fn handle_private_message(&self, to_user_id: u16, content: &str, sender_username: &str, sender_id: u16, stream: &TcpStream) -> Result<(), Box<dyn std::error::Error>> {
        // Save private message to database (room_id = 0 for private messages)
        self.database.save_message(0, sender_id, sender_username, content, "private").await?;
        
        // Create private message
        let message = Message::new(sender_id, MessageType::PrivateMessage {
            to_user_id,
            sender_username: sender_username.to_string(),
            content: content.to_string(),
        });
        
        // Send to recipient (simplified for now)
        self.send_to_stream(stream, message)?;
        
        Ok(())
    }

    async fn handle_join_room(&self, room_id: u16, password: &Option<String>, user_id: u16, stream: &TcpStream) -> Result<(), Box<dyn std::error::Error>> {
        // Check if room exists
        if let Some(room) = self.database.get_room(room_id).await? {
            // Verify password if required
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
                // Add user to room membership in database
                self.database.add_user_to_room(user_id, room_id).await?;
                
                let response = Message::new(0, MessageType::RoomJoined {
                    room_id,
                    name: room.name.clone(),
                    description: room.description.clone(),
                });
                self.send_to_stream(stream, response)?;
                
                // Notify other users in room that someone joined
                let username = if let Some(account) = self.database.get_user_by_id(user_id).await? {
                    account.username
                } else {
                    "Unknown".to_string()
                };
                
                let join_notification = Message::new(0, MessageType::RoomMessage {
                    room_id,
                    sender_username: "System".to_string(),
                    content: format!("{} joined the room", username),
                });
                
                self.broadcast_to_room(room_id, join_notification).await?;
            } else {
                let response = Message::new(0, MessageType::ServerResponse {
                    success: false,
                    content: "Invalid room password".to_string(),
                });
                self.send_to_stream(stream, response)?;
            }
        } else {
            let response = Message::new(0, MessageType::RoomNotFound { room_id });
            self.send_to_stream(stream, response)?;
        }
        
        Ok(())
    }

    fn send_to_stream(&self, stream: &TcpStream, message: Message) -> Result<(), std::io::Error> {
        if let Ok(json) = message.to_json_file() {
            let mut stream_clone = stream.try_clone()?;
            writeln!(stream_clone, "{}", json)?;
            stream_clone.flush()?;
        }
        Ok(())
    }

    fn hash_password(password: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(password);
        format!("{:x}", hasher.finalize())
    }

    async fn broadcast_to_room(&self, room_id: u16, message: Message) -> Result<(), Box<dyn std::error::Error>> {
        // Get all users in the room from database
        let room_members = self.database.get_room_members(room_id).await?;
        let user_streams = self.user_streams.read().await;
        
        let message_json = message.to_json_file()?;
        
        // Send message to all room members who are currently connected
        for member_id in room_members {
            if let Some(stream) = user_streams.get(&member_id) {
                if let Ok(mut stream_clone) = stream.try_clone() {
                    use std::io::Write;
                    let _ = writeln!(stream_clone, "{}", message_json);
                    let _ = stream_clone.flush();
                }
            }
        }
        
        Ok(())
    }

    async fn broadcast_to_all_users(&self, message: Message) -> Result<(), Box<dyn std::error::Error>> {
        let user_streams = self.user_streams.read().await;
        let message_json = message.to_json_file()?;
        
        // Send message to all connected users
        for stream in user_streams.values() {
            if let Ok(mut stream_clone) = stream.try_clone() {
                use std::io::Write;
                let _ = writeln!(stream_clone, "{}", message_json);
                let _ = stream_clone.flush();
            }
        }
        
        Ok(())
    }
}
