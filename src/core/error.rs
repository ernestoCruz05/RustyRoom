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

