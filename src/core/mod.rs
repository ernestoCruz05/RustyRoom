pub mod database;
pub mod error;

#[derive(Clone, Debug)]
pub enum RoomState {
	Open,
	Private,
	Closed,
}

#[derive(Clone, Debug)]
pub struct Room {
	pub id: u16,
	pub name: String,
	pub description: String,
	pub state: RoomState,
	pub password_hash: Option<String>,
}

impl Room {
	pub fn new(id: u16, name: String, description: String, state: RoomState) -> Self {
		Room { id, name, description, state, password_hash: None }
	}

	pub fn new_with_password(id: u16, name: String, description: String, password_hash: String) -> Self {
		Room { id, name, description, state: RoomState::Private, password_hash: Some(password_hash) }
	}

	pub fn is_password_protected(&self) -> bool { self.password_hash.is_some() }
	pub fn verify_password(&self, password_hash: &str) -> bool { self.password_hash.as_deref().map(|h| h == password_hash).unwrap_or(true) }
}

#[derive(Debug, Clone)]
pub struct Account {
	pub username: String,
	pub hash: String,
	pub user_id: u16,
}

impl Account {
	pub fn new(username: String, hash: String, user_id: u16) -> Self { Self { username, hash, user_id } }
}

