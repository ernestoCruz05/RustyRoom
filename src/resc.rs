//! Resource Module for FCA Chat Application
//!
//! This module contains all the core data structures and types used throughout
//! the FCA chat application, including:
//! - User and Account management
//! - Room definitions and states
//! - Message types for client-server communication
//! - Voice chat related structures
//! - Error handling types

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

// ============================================================================
// USER MANAGEMENT
// ============================================================================

/// Represents a connected user in the chat system
/// 
/// A User is created when someone connects to the server and contains
/// their connection state, authentication status, and room subscriptions.
pub struct User {
    /// Unique identifier for the user
    pub id: u16,
    /// Display name chosen by the user
    pub username: String,
    /// Hashed password for authentication
    pub hash: String,
    /// Network address of the user's connection
    pub address: SocketAddr,
    /// List of rooms the user has joined
    pub subscribed_rooms: Vec<Room>,
    /// Whether the user has been authenticated
    pub is_auth: bool,
}

impl User {
    /// Creates a new User instance
    /// 
    /// # Arguments
    /// * `id` - Unique user identifier
    /// * `username` - User's display name
    /// * `hash` - Hashed password
    /// * `address` - Client's socket address
    /// * `is_auth` - Initial authentication state
    pub fn new(
        id: u16,
        username: String,
        hash: String,
        address: SocketAddr,
        is_auth: bool,
    ) -> Self {
        User {
            id,
            username,
            hash,
            address,
            subscribed_rooms: Vec::new(),
            is_auth,
        }
    }

    /// Marks the user as authenticated
    pub fn authed(&mut self) {
        self.is_auth = true;
    }

    /// Adds a room to the user's subscription list
    pub fn subscribe_room(&mut self, room: Room) {
        self.subscribed_rooms.push(room);
    }

    /// Removes a room from the user's subscription list
    pub fn unsubscribe_room(&mut self, room_id: u16) {
        self.subscribed_rooms.retain(|room| room.id != room_id);
    }
}

// ============================================================================
// ROOM MANAGEMENT
// ============================================================================

/// Represents the access state of a chat room
#[derive(Clone)]
pub enum RoomState {
    /// Room is open to all users
    Open,
    /// Room requires a password to join
    Private,
    /// Room is closed and cannot be joined
    Closed,
}

/// Represents a chat room in the system
/// 
/// Rooms are persistent containers for conversations. They can be
/// public, password-protected, or closed.
#[derive(Clone)]
pub struct Room {
    /// Unique room identifier
    pub id: u16,
    /// Display name of the room
    pub name: String,
    /// Brief description of the room's purpose
    pub description: String,
    /// Current access state of the room
    pub state: RoomState,
    /// Optional password hash for private rooms
    pub password_hash: Option<String>,
}

impl Room {
    /// Creates a new open room without password protection
    /// 
    /// # Arguments
    /// * `id` - Unique room identifier
    /// * `name` - Display name for the room
    /// * `description` - Brief description of the room
    /// * `state` - Initial room state (Open, Private, or Closed)
    pub fn new(id: u16, name: String, description: String, state: RoomState) -> Self {
        Room {
            id,
            name,
            description,
            state,
            password_hash: None,
        }
    }

    /// Creates a new password-protected room
    /// 
    /// # Arguments
    /// * `id` - Unique room identifier
    /// * `name` - Display name for the room
    /// * `description` - Brief description of the room
    /// * `password_hash` - SHA-256 hash of the room password
    pub fn new_with_password(
        id: u16,
        name: String,
        description: String,
        password_hash: String,
    ) -> Self {
        Room {
            id,
            name,
            description,
            state: RoomState::Private,
            password_hash: Some(password_hash),
        }
    }

    /// Updates the room's access state
    pub fn set_state(&mut self, state: RoomState) {
        self.state = state;
    }

    /// Checks if the room requires a password to join
    pub fn is_password_protected(&self) -> bool {
        self.password_hash.is_some()
    }

    /// Verifies if a password hash matches the room's password
    /// 
    /// Returns true if:
    /// - The room has no password (open room)
    /// - The provided hash matches the room's password hash
    pub fn verify_password(&self, password_hash: &str) -> bool {
        match &self.password_hash {
            Some(hash) => hash == password_hash,
            None => true,
        }
    }
}

// ============================================================================
// MESSAGE TYPES
// ============================================================================

/// All possible message types exchanged between client and server
/// 
/// This enum defines the protocol for client-server communication.
/// Each variant represents a specific action or response.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum MessageType {
    // --- Authentication ---
    
    /// Client request to create a new account
    Register {
        username: String,
        password: String,
    },
    /// Client request to log into an existing account
    Login {
        username: String,
        password: String,
    },
    /// Server response for successful authentication
    AuthSuccess {
        user_id: u16,
        message: String,
    },
    /// Server response for failed authentication
    AuthFailure {
        reason: String,
    },
    
    // --- Room Management ---
    
    /// Client request to join a room
    Join {
        room_id: u16,
        password: Option<String>,
    },
    /// Client request to leave a room
    Leave {
        room_id: u16,
    },
    /// Client request to create a new room
    CreateRoom {
        name: String,
        description: String,
        password: Option<String>,
    },
    /// Server notification that a room was created
    RoomCreated {
        room_id: u16,
        name: String,
        description: String,
    },
    /// Server notification that user joined a room
    RoomJoined {
        room_id: u16,
        name: String,
        description: String,
    },
    /// Server response when a room doesn't exist
    RoomNotFound {
        room_id: u16,
    },
    
    // --- Messaging ---
    
    /// A message sent to a room (broadcast to all room members)
    RoomMessage {
        room_id: u16,
        sender_username: String,
        content: String,
    },
    /// A direct message to a specific user
    PrivateMessage {
        to_user_id: u16,
        sender_username: String,
        content: String,
    },
    /// Generic server response with success/failure status
    ServerResponse {
        success: bool,
        content: String,
    },
    
    // --- User/Room Queries ---
    
    /// Client request for the list of available rooms
    ListRooms,
    /// Server response with room list
    RoomList {
        rooms: Vec<RoomInfo>,
    },
    /// Server notification of a user's status change
    UserStatusUpdate {
        user_id: u16,
        username: String,
        is_online: bool,
    },
    /// Server broadcast of all user statuses
    UserListUpdate {
        users: Vec<UserStatus>,
    },
    /// Client request for current user list
    RequestUserList,
    
    // --- Voice Chat ---
    
    /// Client request to enable voice chat
    RequestVoice,
    /// Server response with voice connection credentials
    VoiceCredentials {
        token: String,
        udp_port: u16,
    },
    /// Server response for voice-related errors
    VoiceError {
        message: String,
    },
    /// Client request to join voice in a room
    VoiceConnect {
        room_id: u16,
    },
    /// Client request to leave voice chat
    VoiceDisconnect {
        room_id: u16,
    },
    /// Server confirmation of voice connection
    VoiceConnected {
        room_id: u16,
        udp_port: u16,
        token: String,
    },
    /// Client notification of mute/deafen state change
    VoiceStateUpdate {
        is_muted: bool,
        is_deafened: bool,
    },
    /// Server notification of user speaking state
    VoiceSpeaking {
        user_id: u16,
        is_speaking: bool,
    },
    /// Server broadcast of voice room participant states
    VoiceRoomState {
        room_id: u16,
        users: Vec<VoiceUserInfo>,
    },
    /// Voice quality metrics report
    VoiceQualityReport {
        latency_ms: u32,
        packet_loss: f32,
        jitter_ms: u32,
    },
}

// ============================================================================
// DATA TRANSFER OBJECTS
// ============================================================================

/// Voice user information for room state updates
/// 
/// Sent to all voice participants when a user's voice state changes.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VoiceUserInfo {
    /// User's unique identifier
    pub user_id: u16,
    /// User's display name
    pub username: String,
    /// Whether the user has muted their microphone
    pub is_muted: bool,
    /// Whether the user has deafened (muted speakers)
    pub is_deafened: bool,
    /// Whether the user is currently speaking
    pub is_speaking: bool,
}

/// Lightweight room information for room listings
/// 
/// Used when displaying available rooms to join.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RoomInfo {
    /// Room's unique identifier
    pub id: u16,
    /// Room's display name
    pub name: String,
    /// Brief description of the room
    pub description: String,
    /// Whether a password is required to join
    pub is_password_protected: bool,
}

/// User status information for user listings
/// 
/// Used to display online/offline status of users.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UserStatus {
    /// User's unique identifier
    pub user_id: u16,
    /// User's display name
    pub username: String,
    /// Whether the user is currently online
    pub is_online: bool,
    /// Timestamp of last activity (formatted string)
    pub last_seen: String,
}

// ============================================================================
// MESSAGE WRAPPER
// ============================================================================

/// Wrapper for all messages exchanged between client and server
/// 
/// Every message includes the sender's ID and the specific message type.
/// This is serialized to JSON for transmission over the network.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Message {
    /// ID of the user sending this message (0 for server messages)
    pub sender_id: u16,
    /// The specific type and content of the message
    pub message_type: MessageType,
}

impl Message {
    /// Creates a new message
    /// 
    /// # Arguments
    /// * `sender_id` - ID of the sending user (0 for server)
    /// * `message_type` - The type and content of the message
    pub fn new(sender_id: u16, message_type: MessageType) -> Self {
        Message {
            sender_id,
            message_type,
        }
    }

    /// Serializes the message to a JSON string
    /// 
    /// Used for sending messages over the network.
    pub fn to_json_file(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Deserializes a message from a JSON string
    /// 
    /// Used for receiving messages from the network.
    pub fn from_json_file(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

// ============================================================================
// ACCOUNT & ERROR HANDLING
// ============================================================================

/// Represents a user account stored in the database
/// 
/// This is the persistent user data, as opposed to the runtime User struct.
#[derive(Debug, Clone)]
pub struct Account {
    /// User's display name (unique)
    pub username: String,
    /// SHA-256 hash of the user's password
    pub hash: String,
    /// Unique user identifier
    pub user_id: u16,
}

impl Account {
    /// Creates a new Account instance
    pub fn new(username: String, hash: String, user_id: u16) -> Self {
        Account {
            username,
            hash,
            user_id,
        }
    }
}

/// Error types for the chat system
/// 
/// Wraps various error types that can occur during chat operations.
#[derive(Debug)]
pub enum ChatError {
    /// I/O errors (network, file)
    Io(std::io::Error),
    /// Database errors
    Database(sqlx::Error),
    /// JSON serialization/deserialization errors
    Serialization(serde_json::Error),
}

impl From<std::io::Error> for ChatError {
    fn from(err: std::io::Error) -> Self {
        ChatError::Io(err)
    }
}

impl From<sqlx::Error> for ChatError {
    fn from(err: sqlx::Error) -> Self {
        ChatError::Database(err)
    }
}

impl From<serde_json::Error> for ChatError {
    fn from(err: serde_json::Error) -> Self {
        ChatError::Serialization(err)
    }
}
