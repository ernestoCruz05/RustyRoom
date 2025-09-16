use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum MessageType {
	Register { username: String, password: String },
	Login { username: String, password: String },
	AuthSuccess { user_id: u16, message: String },
	AuthFailure { reason: String },
	Join { room_id: u16, password: Option<String> },
	Leave { room_id: u16 },
	CreateRoom { room_id: u16, name: String, description: String, password: Option<String> },
	RoomCreated { room_id: u16, name: String, description: String },
	RoomJoined { room_id: u16, name: String, description: String },
	RoomNotFound { room_id: u16 },
	RoomMessage { room_id: u16, sender_username: String, content: String },
	PrivateMessage { to_user_id: u16, sender_username: String, content: String },
	ServerResponse { success: bool, content: String },
	ListRooms,
	RoomList { rooms: Vec<RoomInfo> },
	UserStatusUpdate { user_id: u16, username: String, is_online: bool },
	UserListUpdate { users: Vec<UserStatus> },
	RequestUserList,
	VoiceChannelJoin { room_id: u16 },
	VoiceChannelLeave { room_id: u16 },
	VoiceChannelJoined { room_id: u16, ssrc: u32, udp_port: u16 },
	VoiceChannelLeft { room_id: u16 },
	VoiceUserSpeaking { user_id: u16, room_id: u16, ssrc: u32, speaking: bool },
	VoiceUserJoined { user_id: u16, username: String, room_id: u16, ssrc: u32 },
	VoiceUserLeft { user_id: u16, room_id: u16 },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RoomInfo {
	pub id: u16,
	pub name: String,
	pub description: String,
	pub is_password_protected: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UserStatus {
	pub user_id: u16,
	pub username: String,
	pub is_online: bool,
	pub last_seen: String,
}

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

