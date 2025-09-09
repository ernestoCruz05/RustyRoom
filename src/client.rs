/*
 Rust TCP Client

 This is a simple TCP client that connects to a chat server, allowing users to join rooms, send messages, 
 and receive messages from other clients.

 Features:
    # Connect to a chat server.
    # Join different chat rooms.
    # Send messages to the room or privately to other clients.
    # Receive messages from other clients in the room or private messages.
    # Handle user input and display messages in a user-friendly format.

*/

#![allow(dead_code)]

use crate::resc::{Message, MessageType};
use std::io::{self, BufRead, BufReader, Write};
use std::net::TcpStream;
use std::thread;
use std::sync::{Arc, Mutex};

#[allow(dead_code)]
#[derive(Clone)]
pub struct ChatClient {
    server_address: String,
    user_id: u16,
    username: String,
    authenticated: bool,
}

impl ChatClient {
    pub fn new(server_address: String, username: String) -> Self {
        ChatClient {
            server_address,
            user_id: 0, 
            username,
            authenticated: false,
        }
    }

    pub fn connect(&mut self) -> io::Result<()> {
        let stream = TcpStream::connect(&self.server_address)?;
        println!("Connected to server at {}", self.server_address);
        
        let read_stream = stream.try_clone()?;
        let mut write_stream = stream;

        let client_clone = Arc::new(Mutex::new(self.clone()));
        let client_for_receiver = Arc::clone(&client_clone);

        thread::spawn(move || {
            Self::message_receiver(read_stream, client_for_receiver);
        });

        Self::show_auth_commands();

        self.handle_user_input(&mut write_stream, client_clone)?;

        Ok(())
    }

    fn message_receiver(stream: TcpStream, client: Arc<Mutex<ChatClient>>) {
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            match line {
                Ok(json_message) => {
                    if let Ok(message) = Message::from_json_file(&json_message) {
                        if let MessageType::AuthSuccess { user_id, .. } = &message.message_type {
                            {
                                let mut client_lock = client.lock().unwrap();
                                client_lock.authenticated = true;
                                client_lock.user_id = *user_id;
                            }
                            Self::display_message(message);
                            Self::show_chat_commands();
                        } else {
                            Self::display_message(message);
                        }
                    }
                }
                Err(_) => {
                    println!("Disconnected from server");
                    break;
                }
            }
        }
    }

    fn display_message(message: Message) {
        match message.message_type {
            MessageType::AuthSuccess { user_id, message: msg } => {
                println!("[SUCCESS] {}", msg);
                println!("Your user ID is: {}", user_id);
            }
            MessageType::AuthFailure { reason } => {
                println!("[ERROR] {}", reason);
            }
            MessageType::RoomMessage { room_id, sender_username , content } => {
                println!("[Room {}] {}: {}", room_id, sender_username, content);
            }
            MessageType::PrivateMessage { to_user_id: _, sender_username,  content } => {
                println!("[Private from {}]: {}", sender_username, content);
            }
            MessageType::ServerResponse { success, content } => {
                if success {
                    println!("[Server]: {}", content);
                } else {
                    println!("[Server Error]: {}", content);
                }
            }
            MessageType::RoomCreated { room_id, name, description } => {
                println!("[SUCCESS] Room '{}' (ID: {}) created successfully!", name, room_id);
                println!("         Description: {}", description);
            }
            MessageType::RoomJoined { room_id, name, description } => {
                println!("╔══════════════════════════════════════════════════════════════╗");
                println!("║ Welcome to '{}' (Room ID: {})!", name, room_id);
                println!("║");
                println!("║ About this room:");
                println!("║    {}", description);
                println!("║");
                println!("║ You can now send messages with: /msg {} <your message>", room_id);
                println!("╚══════════════════════════════════════════════════════════════╝");
            }
            MessageType::RoomNotFound { room_id } => {
                println!("[INFO] Room {} doesn't exist yet.", room_id);
                println!("       Would you like to create it? Use:");
                println!("       /create {} <name> <description> [password]", room_id);
                println!("       Example: /create {} \"General Chat\" \"A place for general discussion\"", room_id);
            }
            MessageType::Register { .. } | MessageType::Login { .. } => {
                println!("[Debug] Received unexpected auth message");
            }
            MessageType::Join { .. } | MessageType::Leave { .. } | MessageType::CreateRoom { .. } => {
                println!("[Debug] Received unexpected room action message");
            }
            MessageType::ListRooms => {
                println!("[Debug] Received ListRooms request");
            }
            MessageType::RoomList { rooms } => {
                println!("Available rooms:");
                for room in rooms {
                    println!("  [{}] {} - {}", room.id, room.name, room.description);
                }
            }
            MessageType::UserStatusUpdate { user_id, username, is_online } => {
                let status = if is_online { "online" } else { "offline" };
                println!("User {} ({}) is now {}", username, user_id, status);
            }
            MessageType::UserListUpdate { users } => {
                println!("Online users:");
                for user in users {
                    let status = if user.is_online { "●" } else { "○" };
                    println!("  {} {}", status, user.username);
                }
            }
            MessageType::RequestUserList => {
                println!("[Debug] Received RequestUserList");
            }
        }
    }

    fn handle_user_input(&mut self, stream: &mut TcpStream, client_clone: Arc<Mutex<ChatClient>>) -> io::Result<()> {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            let input = line?;
            
            if input == "/quit" {
                break;
            }
            
            let is_authenticated = {
                let client_lock = client_clone.lock().unwrap();
                client_lock.authenticated
            };
            
            if let Some(message) = self.parse_command(&input, is_authenticated) {
                let json = message.to_json_file().unwrap();
                writeln!(stream, "{}", json)?;
                stream.flush()?;
            }
        }

        Ok(())
    }

    fn parse_command(&self, input: &str, is_authenticated: bool) -> Option<Message> {
        let parts: Vec<&str> = input.split_whitespace().collect();
        
        match parts.get(0)? {
            &"/register" => {
                if is_authenticated {
                    println!("You are already logged in!");
                    return None;
                }
                if parts.len() < 3 {
                    println!("Usage: /register <username> <password>");
                    return None;
                }
                let username = parts[1].to_string();
                let password = parts[2].to_string();
                Some(Message::new(0, MessageType::Register { username, password }))
            }
            &"/login" => {
                if is_authenticated {
                    println!("You are already logged in!");
                    return None;
                }
                if parts.len() < 3 {
                    println!("Usage: /login <username> <password>");
                    return None;
                }
                let username = parts[1].to_string();
                let password = parts[2].to_string();
                Some(Message::new(0, MessageType::Login { username, password }))
            }
            &"/join" => {
                if !is_authenticated {
                    println!("Please login or register first!");
                    return None;
                }
                if parts.len() < 2 {
                    println!("Usage: /join <room_id> [password]");
                    return None;
                }
                let room_id: u16 = parts.get(1)?.parse().ok()?;
                let password = if parts.len() > 2 {
                    Some(parts[2].to_string())
                } else {
                    None
                };
                Some(Message::new(self.user_id, MessageType::Join { room_id, password }))
            }
            &"/leave" => {
                if !is_authenticated {
                    println!("Please login or register first!");
                    return None;
                }
                if parts.len() < 2 {
                    println!("Usage: /leave <room_id>");
                    return None;
                }
                let room_id: u16 = parts.get(1)?.parse().ok()?;
                Some(Message::new(self.user_id, MessageType::Leave { room_id }))
            }
            &"/create" => {
                if !is_authenticated {
                    println!("Please login or register first!");
                    return None;
                }
                
                let args = Self::parse_quoted_args(input)?;
                if args.len() < 4 {
                    println!("Usage: /create <room_id> <name> <description> [password]");
                    println!("Example: /create 1 \"General Chat\" \"A place for general discussion\" [password]");
                    return None;
                }
                
                let room_id: u16 = args.get(1)?.parse().ok()?;
                let name = args.get(2)?.clone();
                let description = args.get(3)?.clone();
                let password = if args.len() > 4 {
                    Some(args.get(4)?.clone())
                } else {
                    None
                };
                Some(Message::new(self.user_id, MessageType::CreateRoom { room_id, name, description, password }))
            }
            &"/msg" => {
                if !is_authenticated {
                    println!("Please login or register first!");
                    return None;
                }
                if parts.len() < 3 {
                    println!("Usage: /msg <room_id> <message>");
                    return None;
                }
                let room_id: u16 = parts.get(1)?.parse().ok()?;
                let content = parts[2..].join(" ");
                Some(Message::new(self.user_id, MessageType::RoomMessage { room_id, sender_username: String::new(), content }))
            }
            &"/private" => {
                if !is_authenticated {
                    println!("Please login or register first!");
                    return None;
                }
                if parts.len() < 3 {
                    println!("Usage: /private <user_id> <message>");
                    return None;
                }
                let to_user_id: u16 = parts.get(1)?.parse().ok()?;
                let content = parts[2..].join(" ");
                Some(Message::new(self.user_id, MessageType::PrivateMessage { to_user_id, sender_username: String::new(),content }))
            }
            _ => {
                println!("Unknown command: {}", input);
                if !is_authenticated {
                    println!("Available commands: /register, /login, /quit");
                } else {
                    println!("Available commands: /join, /leave, /create, /msg, /private, /quit");
                }
                None
            }
        }
    }

    fn parse_quoted_args(input: &str) -> Option<Vec<String>> {
        let mut args = Vec::new();
        let mut current_arg = String::new();
        let mut in_quotes = false;
        let mut chars = input.chars().peekable();
        
        while let Some(ch) = chars.next() {
            match ch {
                '"' => {
                    in_quotes = !in_quotes;
                }
                ' ' if !in_quotes => {
                    if !current_arg.is_empty() {
                        args.push(current_arg.trim().to_string());
                        current_arg.clear();
                    }
                }
                _ => {
                    current_arg.push(ch);
                }
            }
        }
        
        if !current_arg.is_empty() {
            args.push(current_arg.trim().to_string());
        }
        
        if args.is_empty() {
            None
        } else {
            Some(args)
        }
    }

    fn show_auth_commands() {
        println!("Welcome to FCA Chat!");
        println!("Authentication Commands:");
        println!("  /register <username> <password> - Create new account");
        println!("  /login <username> <password> - Login to existing account");
        println!("  /quit - Disconnect");
        println!();
    }

    fn show_chat_commands() {
        println!("\n=== Successfully Authenticated! ===");
        println!("Chat Commands:");
        println!("  /join <room_id> [password] - Join a room (use password if room is private)");
        println!("  /leave <room_id> - Leave a room"); 
        println!("  /create <room_id> <name> <description> [password] - Create a new room");
        println!("  /msg <room_id> <message> - Send message to room");
        println!("  /private <user_id> <message> - Send private message");
        println!("  /quit - Disconnect");
        println!();
        println!("   Tips:");
        println!("   • When you try to join a non-existent room, you'll be prompted to create it");
        println!("   • Use quotes for multi-word names: /create 1 \"My Room\" \"A cool place to chat\"");
        println!("   • Add a password to make your room private: /create 1 \"Secret\" \"Private room\" mypassword");
        println!();
    }

    pub fn start_client(server_address: &str, username: &str) -> io::Result<()> {
        let mut client = ChatClient::new(server_address.to_string(), username.to_string());
        client.connect()
    }
}
