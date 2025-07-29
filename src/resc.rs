/*
 Oh yeah yeah, shared resources module! yeah yeah
*/

use std::net::SocketAddr;
use serde::{Deserialize, Serialize};


// ? User module down


pub struct User{
    pub id: u16,
    pub username: String,
    pub hash: String,
    pub address: SocketAddr,
    pub subscribed_rooms: Vec<Room>,
    pub is_auth: bool,
}

impl User {
    pub fn new(id: u16, username: String, hash: String,  address: SocketAddr,  is_auth: bool) -> Self {
        User {
            id,
            username,
            hash,
            address,
            subscribed_rooms: Vec::new(),
            is_auth,
        }
    }

    pub fn authed(&mut self) {
        self.is_auth = true;
    }

    pub fn subscribe_room(&mut self, room: Room) {
        self.subscribed_rooms.push(room);
    }

    pub fn unsubscribe_room(&mut self, room_id: u16) {
        self.subscribed_rooms.retain(|room| room.id != room_id);
    }
}

// ? Room module down

#[derive(Clone)]
pub enum RoomState{
    Open,
    Closed,
}

#[derive(Clone)]
pub struct Room{
    pub id: u16,
    pub name: String,
    pub state: RoomState, 
}

impl Room {
    pub fn new(id: u16, name: String, state: RoomState) -> Self {
        Room {
            id,
            name,
            state,
        }
    }

    pub fn set_state(&mut self, state: RoomState) {
        self.state = state;
    }
}

// ? Message module down

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum MessageType {
    Register {username: String, password: String},
    Login {username: String, password: String},
    AuthSuccess {user_id: u16, message: String},
    AuthFailure {reason: String},
    Join {room_id: u16},
    Leave {room_id: u16},
    RoomMessage {room_id: u16, content: String},
    PrivateMessage {to_user_id: u16, content: String},
    ServerResponse {success: bool, content: String},

    
}


#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Message {
    pub sender_id: u16,
    pub message_type: MessageType,
}

impl Message {
    pub fn new(sender_id: u16, message_type: MessageType) -> Self {
        Message {
            sender_id,
            message_type,
        }
    }

    pub fn to_json_file(&self) -> Result<String, serde_json::Error>{
        serde_json::to_string(self)
    }

    pub fn from_json_file(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

}

// ? Account module down

#[derive(Debug, Clone)]
pub struct Account {
    pub username: String,
    pub hash: String,
    pub user_id: u16,
}

impl Account{
    pub fn new(username: String, hash: String, user_id: u16) -> Self {
        Account{
            username,
            hash,
            user_id,
        }
    }
}
