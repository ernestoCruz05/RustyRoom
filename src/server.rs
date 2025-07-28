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

use crate::resc::{User, Room, RoomState, Message, MessageType};
use std::collections::HashMap;
use std::net::{TcpListener, TcpStream, SocketAddr};
use std::sync::{Arc, Mutex};
use std::thread;
use std::io::{BufRead, BufReader, Write};
pub struct ChatServer { 
    users: HashMap<u16, User>,
    rooms: HashMap<u16, Room>,
    user_streams: HashMap<u16, TcpStream>,
    next_user_id: u16,
    next_room_id: u16,
}

impl ChatServer{
    pub fn new() -> Self {
        ChatServer {
            users: HashMap::new(),
            rooms: HashMap::new(),
            user_streams: HashMap::new(),
            next_user_id: 1,
            next_room_id: 1,
        }
    }


        pub fn add_user(&mut self, username: String, address: SocketAddr) -> u16 {
        let user_id = self.next_user_id;
        let user = User::new(user_id, username, address);
        self.users.insert(user_id, user);
        self.next_user_id += 1;
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

        let user_id = {
            let mut server_lock = server.lock().unwrap();
            server_lock.add_user(format!("User_{}", client_address.port()), client_address)
        };

        let reader = BufReader::new(stream.try_clone().unwrap());

        for line in reader.lines(){
            match line{
                Ok(json_message) => {
                    if let Ok(message) = Message::from_json_file(&json_message){
                        Self::digest_message(Arc::clone(&server), user_id, message);
                    }
                }
                Err(_) => {
                    break;
                }
            }
        }
        let mut server_lock = server.lock().unwrap();
        server_lock.remove_user(user_id);
        println!("Disconnected: [{}]", client_address);
    }

    fn digest_message(server: Arc<Mutex<(ChatServer)>>, userd_id: u16, message: Message){
        let mut server_lock = server.lock().unwrap();

        match message.message_type {
            MessageType::Join { room_id} => {
                // Join room logic
            }
            MessageType::Leave { room_id } => {
                // Leave room logic
            }
            MessageType::RoomMessage { room_id, content } => {
                // Broadcast message to room
            }
            MessageType::PrivateMessage { to_user_id, content } => {
                // Send private message logic
            }
            _ => {
                // ? I dont know 
            }
        }
    }

    pub fn remove_user(&mut self, user_id: u16) -> Option<User> {
                self.users.remove(&user_id)
    }

}
