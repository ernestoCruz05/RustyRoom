/*
 Rust TCP Server Chat Room

 This is a simple TCP server that allows multiple clients to connect and chat with each other, conversations are divided into rooms.

 Features:
    # Multiple clients can connect simultaneously.
    # Clients can join different chat rooms.
    # Messages sent by clients are broadcasted to all clients in the same room.
    # Clients can leave the chat room.
    # There is an "admin" client that can manage rooms and clients.
    # The server can handle client disconnections gracefully.
    # Clients can send private messages to each other.


*/

#![allow(dead_code)]

use crate::resc::{User, Room, Account, RoomState, Message, MessageType};
use std::collections::HashMap;
use std::net::{TcpListener, TcpStream, SocketAddr};
use std::sync::{Arc, Mutex};
use std::thread;
use std::io::{BufRead, BufReader, Write};
use sha2::{Sha256, Digest};

pub struct ChatServer { 
    users: HashMap<u16, User>,
    rooms: HashMap<u16, Room>,
    user_streams: HashMap<u16, TcpStream>,
    accounts: HashMap<String, Account>,
    unauthenticated_connections: HashMap<SocketAddr, TcpStream>, 
    next_user_id: u16,
    next_room_id: u16,
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
        // let user = User::new(user_id, username, address);
        //self.users.insert(user_id, user);
        // self.next_user_id += 1;
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
