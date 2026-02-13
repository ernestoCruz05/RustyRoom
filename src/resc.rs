//! Resource Module for RustyRoom Chat Application
//!
//! Core data structures and types for the Discord-like chat system:
//! - Servers (guilds) that users can join
//! - Channels (text & voice) within servers
//! - User and Account management
//! - Message types for client-server communication
//! - Voice chat related structures

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

// ============================================================================
// USER MANAGEMENT
// ============================================================================

/// Represents a connected user in the chat system
pub struct User {
    pub id: u16,
    pub username: String,
    pub hash: String,
    pub address: SocketAddr,
    pub is_auth: bool,
}

impl User {
    pub fn new(
        id: u16,
        username: String,
        hash: String,
        address: SocketAddr,
        is_auth: bool,
    ) -> Self {
        User { id, username, hash, address, is_auth }
    }

    pub fn authed(&mut self) {
        self.is_auth = true;
    }
}

/// Persistent user account stored in the database
#[derive(Debug, Clone)]
pub struct Account {
    pub username: String,
    pub hash: String,
    pub user_id: u16,
}

impl Account {
    pub fn new(username: String, hash: String, user_id: u16) -> Self {
        Account { username, hash, user_id }
    }
}

// ============================================================================
// SERVER (GUILD) MANAGEMENT
// ============================================================================

/// A server (guild) that contains channels and members
#[derive(Debug, Clone)]
pub struct Server {
    pub id: u16,
    pub name: String,
    pub description: String,
    pub owner_id: u16,
}

impl Server {
    pub fn new(id: u16, name: String, description: String, owner_id: u16) -> Self {
        Server { id, name, description, owner_id }
    }
}

// ============================================================================
// CHANNEL MANAGEMENT
// ============================================================================

/// The type of a channel within a server
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ChannelType {
    Text,
    Voice,
}

impl ChannelType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChannelType::Text => "text",
            ChannelType::Voice => "voice",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "voice" => ChannelType::Voice,
            _ => ChannelType::Text,
        }
    }
}

/// A channel within a server
#[derive(Debug, Clone)]
pub struct Channel {
    pub id: u16,
    pub server_id: u16,
    pub name: String,
    pub description: String,
    pub channel_type: ChannelType,
    pub password_hash: Option<String>,
}

impl Channel {
    pub fn new(
        id: u16,
        server_id: u16,
        name: String,
        description: String,
        channel_type: ChannelType,
    ) -> Self {
        Channel { id, server_id, name, description, channel_type, password_hash: None }
    }

    pub fn new_with_password(
        id: u16,
        server_id: u16,
        name: String,
        description: String,
        channel_type: ChannelType,
        password_hash: String,
    ) -> Self {
        Channel {
            id, server_id, name, description, channel_type,
            password_hash: Some(password_hash),
        }
    }

    pub fn is_password_protected(&self) -> bool {
        self.password_hash.is_some()
    }

    pub fn verify_password(&self, password_hash: &str) -> bool {
        match &self.password_hash {
            Some(hash) => hash == password_hash,
            None => true,
        }
    }
}

// ============================================================================
// MESSAGE TYPES (PROTOCOL)
// ============================================================================

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum MessageType {
    // --- Authentication ---
    Register { username: String, password: String },
    Login { username: String, password: String },
    AuthSuccess { user_id: u16, message: String },
    AuthFailure { reason: String },

    // --- Server (Guild) Management ---
    CreateServer { name: String, description: String },
    ServerCreated { server_id: u16, name: String, description: String },
    JoinServer { server_id: u16 },
    LeaveServer { server_id: u16 },
    ServerJoined {
        server_id: u16,
        name: String,
        description: String,
        channels: Vec<ChannelInfo>,
    },
    ServerLeft { server_id: u16 },
    ListServers,
    ServerList { servers: Vec<ServerInfo> },
    /// Full sync of user's servers + channels (sent after login)
    UserServersSync { servers: Vec<ServerWithChannels> },

    // --- Channel Management ---
    CreateChannel {
        server_id: u16,
        name: String,
        description: String,
        channel_type: ChannelType,
        password: Option<String>,
    },
    ChannelCreated {
        server_id: u16,
        channel_id: u16,
        name: String,
        description: String,
        channel_type: ChannelType,
    },
    JoinChannel { channel_id: u16, password: Option<String> },
    LeaveChannel { channel_id: u16 },
    ChannelJoined {
        server_id: u16,
        channel_id: u16,
        name: String,
        description: String,
        channel_type: ChannelType,
    },
    ListChannels { server_id: u16 },
    ChannelList { server_id: u16, channels: Vec<ChannelInfo> },

    // --- Messaging ---
    ChannelMessage { channel_id: u16, sender_username: String, content: String },
    PrivateMessage { to_user_id: u16, sender_username: String, content: String },
    ServerResponse { success: bool, content: String },

    // --- User Queries ---
    RequestUserList,
    UserListUpdate { users: Vec<UserStatus> },
    RequestServerMembers { server_id: u16 },
    ServerMemberList { server_id: u16, members: Vec<UserStatus> },

    // --- Voice Chat ---
    RequestVoice { channel_id: u16 },
    VoiceCredentials { token: String, udp_port: u16, channel_id: u16 },
    VoiceError { message: String },
    VoiceConnect { channel_id: u16 },
    VoiceDisconnect { channel_id: u16 },
    VoiceConnected { channel_id: u16, udp_port: u16, token: String },
    VoiceStateUpdate { is_muted: bool, is_deafened: bool },
    VoiceSpeaking { user_id: u16, is_speaking: bool },
    VoiceRoomState { channel_id: u16, users: Vec<VoiceUserInfo> },
    VoiceQualityReport { latency_ms: u32, packet_loss: f32, jitter_ms: u32 },
}

// ============================================================================
// DATA TRANSFER OBJECTS
// ============================================================================

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VoiceUserInfo {
    pub user_id: u16,
    pub username: String,
    pub is_muted: bool,
    pub is_deafened: bool,
    pub is_speaking: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ServerInfo {
    pub id: u16,
    pub name: String,
    pub description: String,
    pub owner_id: u16,
    pub member_count: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ChannelInfo {
    pub id: u16,
    pub server_id: u16,
    pub name: String,
    pub description: String,
    pub channel_type: ChannelType,
    pub is_password_protected: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ServerWithChannels {
    pub server: ServerInfo,
    pub channels: Vec<ChannelInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UserStatus {
    pub user_id: u16,
    pub username: String,
    pub is_online: bool,
    pub last_seen: String,
}

// ============================================================================
// MESSAGE WRAPPER
// ============================================================================

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Message {
    pub sender_id: u16,
    pub message_type: MessageType,
}

impl Message {
    pub fn new(sender_id: u16, message_type: MessageType) -> Self {
        Message { sender_id, message_type }
    }

    pub fn to_json_file(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_json_file(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

// ============================================================================
// ERROR HANDLING
// ============================================================================

#[derive(Debug)]
pub enum ChatError {
    Io(std::io::Error),
    Database(sqlx::Error),
    Serialization(serde_json::Error),
}

impl From<std::io::Error> for ChatError {
    fn from(err: std::io::Error) -> Self { ChatError::Io(err) }
}

impl From<sqlx::Error> for ChatError {
    fn from(err: sqlx::Error) -> Self { ChatError::Database(err) }
}

impl From<serde_json::Error> for ChatError {
    fn from(err: serde_json::Error) -> Self { ChatError::Serialization(err) }
}
