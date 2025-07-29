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

use crate::resc::{Message, MessageType};
use std::io::{self, BufRead, BufReader, Write};
use std::net::TcpStream;
use std::thread;

#[allow(dead_code)]
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
            user_id: 0, // Will be assigned by server
            username,
            authenticated: false,
        }
    }

    pub fn connect(&mut self) -> io::Result<()> {
        let stream = TcpStream::connect(&self.server_address)?;
        println!("Connected to server at {}", self.server_address);
        
        let read_stream = stream.try_clone()?;
        let mut write_stream = stream;

        thread::spawn(move || {
            Self::message_receiver(read_stream);
        });

        self.handle_user_input(&mut write_stream)?;

        Ok(())
    }

    fn message_receiver(stream: TcpStream) {
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            match line {
                Ok(json_message) => {
                    if let Ok(message) = Message::from_json_file(&json_message) {
                        Self::display_message(message);
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
            MessageType::RoomMessage { room_id, content } => {
                println!("[Room {}] User {}: {}", room_id, message.sender_id, content);
            }
            MessageType::PrivateMessage { to_user_id: _, content } => {
                println!("[Private from User {}]: {}", message.sender_id, content);
            }
            MessageType::ServerResponse { success, content } => {
                if success {
                    println!("[Server]: {}", content);
                } else {
                    println!("[Server Error]: {}", content);
                }
            }
            MessageType::Register { .. } | MessageType::Login { .. } => {
                // These shouldn't be received by client, but handle gracefully
                println!("[Debug] Received unexpected auth message");
            }
            MessageType::Join { .. } | MessageType::Leave { .. } => {
                // These shouldn't be received by client, but handle gracefully
                println!("[Debug] Received unexpected room action message");
            }
        }
    }

    fn handle_user_input(&mut self, stream: &mut TcpStream) -> io::Result<()> {
        println!("Welcome to FCA Chat!");
        println!("Authentication Commands:");
        println!("  /register <username> <password> - Create new account");
        println!("  /login <username> <password> - Login to existing account");
        println!();
        println!("Chat Commands (after authentication):");
        println!("  /join <room_id> - Join a room");
        println!("  /leave <room_id> - Leave a room"); 
        println!("  /msg <room_id> <message> - Send message to room");
        println!("  /private <user_id> <message> - Send private message");
        println!("  /quit - Disconnect");

        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            let input = line?;
            
            if let Some(message) = self.parse_command(&input) {
                let json = message.to_json_file().unwrap();
                writeln!(stream, "{}", json)?;
                stream.flush()?;
            }

            if input == "/quit" {
                break;
            }
        }

        Ok(())
    }

    fn parse_command(&self, input: &str) -> Option<Message> {
        let parts: Vec<&str> = input.split_whitespace().collect();
        
        match parts.get(0)? {
            &"/register" => {
                if parts.len() < 3 {
                    println!("Usage: /register <username> <password>");
                    return None;
                }
                let username = parts[1].to_string();
                let password = parts[2].to_string();
                Some(Message::new(0, MessageType::Register { username, password }))
            }
            &"/login" => {
                if parts.len() < 3 {
                    println!("Usage: /login <username> <password>");
                    return None;
                }
                let username = parts[1].to_string();
                let password = parts[2].to_string();
                Some(Message::new(0, MessageType::Login { username, password }))
            }
            &"/join" => {
                if parts.len() < 2 {
                    println!("Usage: /join <room_id>");
                    return None;
                }
                let room_id: u16 = parts.get(1)?.parse().ok()?;
                Some(Message::new(self.user_id, MessageType::Join { room_id }))
            }
            &"/leave" => {
                if parts.len() < 2 {
                    println!("Usage: /leave <room_id>");
                    return None;
                }
                let room_id: u16 = parts.get(1)?.parse().ok()?;
                Some(Message::new(self.user_id, MessageType::Leave { room_id }))
            }
            &"/msg" => {
                if parts.len() < 3 {
                    println!("Usage: /msg <room_id> <message>");
                    return None;
                }
                let room_id: u16 = parts.get(1)?.parse().ok()?;
                let content = parts[2..].join(" ");
                Some(Message::new(self.user_id, MessageType::RoomMessage { room_id, content }))
            }
            &"/private" => {
                if parts.len() < 3 {
                    println!("Usage: /private <user_id> <message>");
                    return None;
                }
                let to_user_id: u16 = parts.get(1)?.parse().ok()?;
                let content = parts[2..].join(" ");
                Some(Message::new(self.user_id, MessageType::PrivateMessage { to_user_id, content }))
            }
            _ => {
                println!("Unknown command: {}", input);
                None
            }
        }
    }


    pub fn start_client(server_address: &str, username: &str) -> io::Result<()> {
        let mut client = ChatClient::new(server_address.to_string(), username.to_string());
        client.connect()
    }
}