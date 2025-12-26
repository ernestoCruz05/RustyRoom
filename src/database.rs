/*
 Database module for FCA Chat Server
 
 Handles SQLite database operations for:
 - User accounts and authentication
 - Room management and persistence
 - Message history storage
 - User online/offline status tracking
*/

use crate::resc::{Account, Room, RoomState, RoomInfo};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool, sqlite::SqlitePoolOptions};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: i64,
    pub room_id: u16,
    pub sender_id: u16,
    pub sender_username: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub message_type: String, // "user", "system", "private"
}

#[derive(Debug, Clone)]
pub struct UserSession {
    pub user_id: u16,
    pub username: String,
    pub is_online: bool,
    pub last_seen: DateTime<Utc>,
    pub current_rooms: Vec<u16>,
}

pub struct Database {
    pool: SqlitePool,
}

impl Database {
    /// Creates a new database connection and initializes the schema.
    ///
    /// Establishes a connection pool to the SQLite database at the given URL
    /// and creates all required tables if they don't exist.
    ///
    /// # Arguments
    /// * `database_url` - SQLite connection string (e.g., "sqlite:chat.db" or "sqlite::memory:")
    ///
    /// # Errors
    /// Returns an error if the connection fails or table initialization fails.
    pub async fn new(database_url: &str) -> Result<Self, sqlx::Error> {
        println!(" Attempting database connection: {}", database_url);
        
        let pool = SqlitePoolOptions::new()
            .max_connections(100)
            .connect(database_url)
            .await
            .map_err(|e| {
                eprintln!("Failed to create database pool: {}", e);
                eprintln!("   Database URL: {}", database_url);
                
                if database_url.contains("sqlite:") && !database_url.contains(":memory:") {
                    let file_path = database_url.strip_prefix("sqlite:").unwrap_or(database_url);
                    eprintln!("   File path: {}", file_path);
                    eprintln!("   Hint: Check if the directory exists and is writable");
                    eprintln!("   Try: touch {} && chmod 666 {}", file_path, file_path);
                }
                e
            })?;
        
        println!("Database pool created successfully with {} connections", pool.size());
        
        let db = Database { pool };
        
        println!("Initializing database tables...");
        match db.init_tables().await {
            Ok(_) => println!("Database tables initialized successfully"),
            Err(e) => {
                eprintln!("Failed to initialize database tables: {}", e);
                return Err(e);
            }
        }
        
        Ok(db)
    }

    /// Initializes the database schema by creating all required tables.
    ///
    /// Creates tables for users, rooms, room memberships, messages, and user sessions.
    /// Uses CREATE TABLE IF NOT EXISTS, so it's safe to call on an existing database.
    async fn init_tables(&self) -> Result<(), sqlx::Error> {
        // Users table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS users (
                id INTEGER PRIMARY KEY,
                username TEXT UNIQUE NOT NULL,
                password_hash TEXT NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                last_seen DATETIME DEFAULT CURRENT_TIMESTAMP,
                is_online BOOLEAN DEFAULT FALSE
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Rooms table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS rooms (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT NOT NULL,
                state TEXT NOT NULL DEFAULT 'Open',
                password_hash TEXT,
                user_count INTEGER NOT NULL DEFAULT 0,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                created_by INTEGER,
                FOREIGN KEY (created_by) REFERENCES users (id)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Room memberships table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS room_memberships (
                user_id INTEGER,
                room_id INTEGER,
                joined_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (user_id, room_id),
                FOREIGN KEY (user_id) REFERENCES users (id),
                FOREIGN KEY (room_id) REFERENCES rooms (id)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Messages table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                room_id INTEGER,
                sender_id INTEGER,
                sender_username TEXT NOT NULL,
                content TEXT NOT NULL,
                message_type TEXT NOT NULL DEFAULT 'user',
                timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (room_id) REFERENCES rooms (id),
                FOREIGN KEY (sender_id) REFERENCES users (id)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS user_sessions (
                user_id INTEGER PRIMARY KEY,
                username TEXT NOT NULL,
                is_online BOOLEAN DEFAULT TRUE,
                last_activity DATETIME DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (user_id) REFERENCES users (id)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Creates a new user account in the database.
    ///
    /// # Arguments
    /// * `username` - Unique username for the new account
    /// * `password_hash` - Pre-hashed password (caller must hash before calling)
    ///
    /// # Returns
    /// The newly assigned user ID on success.
    pub async fn create_user(&self, username: &str, password_hash: &str) -> Result<u16, sqlx::Error> {
        let result = sqlx::query(
            "INSERT INTO users (username, password_hash) VALUES (?, ?) RETURNING id"
        )
        .bind(username)
        .bind(password_hash)
        .fetch_one(&self.pool)
        .await?;

        Ok(result.get::<i64, _>("id") as u16)
    }

    /// Retrieves a user account by username.
    ///
    /// # Arguments
    /// * `username` - The username to look up
    ///
    /// # Returns
    /// `Some(Account)` if found, `None` if no user exists with that username.
    pub async fn get_user_by_username(&self, username: &str) -> Result<Option<Account>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT id, username, password_hash FROM users WHERE username = ?"
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(Some(Account::new(
                row.get("username"),
                row.get("password_hash"),
                row.get::<i64, _>("id") as u16,
            ))),
            None => Ok(None),
        }
    }

    /// Retrieves a user account by user ID.
    ///
    /// # Arguments
    /// * `user_id` - The unique user ID to look up
    ///
    /// # Returns
    /// `Some(Account)` if found, `None` if no user exists with that ID.
    pub async fn get_user_by_id(&self, user_id: u16) -> Result<Option<Account>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT id, username, password_hash FROM users WHERE id = ?"
        )
        .bind(user_id as i64)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(Some(Account::new(
                row.get("username"),
                row.get("password_hash"),
                row.get::<i64, _>("id") as u16,
            ))),
            None => Ok(None),
        }
    }

    /// Updates a user's online/offline status.
    ///
    /// Updates both the user_sessions table and the users table.
    /// Also updates the last_activity/last_seen timestamps.
    ///
    /// # Arguments
    /// * `user_id` - The user's ID
    /// * `username` - The user's username
    /// * `is_online` - `true` if the user is coming online, `false` if going offline
    pub async fn set_user_online_status(&self, user_id: u16, username: &str, is_online: bool) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO user_sessions (user_id, username, is_online, last_activity)
            VALUES (?, ?, ?, CURRENT_TIMESTAMP)
            ON CONFLICT(user_id) DO UPDATE SET
                is_online = excluded.is_online,
                last_activity = excluded.last_activity
            "#
        )
        .bind(user_id as i64)
        .bind(username)
        .bind(is_online)
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "UPDATE users SET is_online = ?, last_seen = CURRENT_TIMESTAMP WHERE id = ?"
        )
        .bind(is_online)
        .bind(user_id as i64)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Retrieves all user sessions, ordered by online status and username.
    ///
    /// # Returns
    /// A list of `UserSession` structs containing user info and online status.
    /// Online users appear first, then sorted alphabetically by username.
    pub async fn get_online_users(&self) -> Result<Vec<UserSession>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT user_id, username, is_online, last_activity 
            FROM user_sessions 
            ORDER BY is_online DESC, username ASC
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        let mut users = Vec::new();
        for row in rows {
            users.push(UserSession {
                user_id: row.get::<i64, _>("user_id") as u16,
                username: row.get("username"),
                is_online: row.get("is_online"),
                last_seen: row.get("last_activity"),
                current_rooms: Vec::new(), // TODO: Load current rooms
            });
        }

        Ok(users)
    }

    /// Creates a new chat room in the database.
    ///
    /// # Arguments
    /// * `id` - Unique room ID
    /// * `name` - Display name for the room
    /// * `description` - Room description
    /// * `state` - Room visibility state (Open, Private, or Closed)
    /// * `password_hash` - Optional password hash for protected rooms
    /// * `created_by` - User ID of the room creator
    pub async fn create_room(&self, id: u16, name: &str, description: &str, state: &RoomState, password_hash: Option<&str>, created_by: u16) -> Result<(), sqlx::Error> {
        let state_str = match state {
            RoomState::Open => "Open",
            RoomState::Private => "Private",
            RoomState::Closed => "Closed",
        };

        sqlx::query(
            "INSERT INTO rooms (id, name, description, state, password_hash, created_by) VALUES (?, ?, ?, ?, ?, ?)"
        )
        .bind(id as i64)
        .bind(name)
        .bind(description)
        .bind(state_str)
        .bind(password_hash)
        .bind(created_by as i64)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Creates a system-managed room (e.g., lobby, announcements).
    ///
    /// System rooms have no creator (created_by is NULL) and are always open.
    ///
    /// # Arguments
    /// * `id` - Unique room ID
    /// * `name` - Display name for the room
    /// * `description` - Room description
    pub async fn create_system_room(&self, id: u16, name: &str, description: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO rooms (id, name, description, state, password_hash, created_by) VALUES (?, ?, ?, ?, ?, NULL)"
        )
        .bind(id as i64)
        .bind(name)
        .bind(description)
        .bind("Open")
        .bind(None::<String>)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Retrieves a room by its ID.
    ///
    /// # Arguments
    /// * `room_id` - The room ID to look up
    ///
    /// # Returns
    /// `Some(Room)` if found, `None` if no room exists with that ID.
    pub async fn get_room(&self, room_id: u16) -> Result<Option<Room>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT id, name, description, state, password_hash FROM rooms WHERE id = ?"
        )
        .bind(room_id as i64)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => {
                let state = match row.get::<String, _>("state").as_str() {
                    "Open" => RoomState::Open,
                    "Private" => RoomState::Private,
                    "Closed" => RoomState::Closed,
                    _ => RoomState::Open,
                };

                let room = if let Some(password_hash) = row.get::<Option<String>, _>("password_hash") {
                    Room::new_with_password(
                        row.get::<i64, _>("id") as u16,
                        row.get("name"),
                        row.get("description"),
                        password_hash,
                    )
                } else {
                    Room::new(
                        row.get::<i64, _>("id") as u16,
                        row.get("name"),
                        row.get("description"),
                        state,
                    )
                };

                Ok(Some(room))
            }
            None => Ok(None),
        }
    }

    /// Retrieves summary information for all rooms.
    ///
    /// # Returns
    /// A list of `RoomInfo` structs ordered by room name, containing
    /// basic room details and whether the room is password-protected.
    pub async fn get_all_rooms(&self) -> Result<Vec<RoomInfo>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, name, description, password_hash FROM rooms ORDER BY name"
        )
        .fetch_all(&self.pool)
        .await?;

        let mut rooms = Vec::new();
        for row in rows {
            rooms.push(RoomInfo {
                id: row.get::<i64, _>("id") as u16,
                name: row.get("name"),
                description: row.get("description"),
                is_password_protected: row.get::<Option<String>, _>("password_hash").is_some(),
            });
        }

        Ok(rooms)
    }

    /// Adds a user to a room's membership list.
    ///
    /// Uses INSERT OR IGNORE to safely handle duplicate join attempts.
    ///
    /// # Arguments
    /// * `user_id` - The user ID to add
    /// * `room_id` - The room ID to join
    pub async fn add_user_to_room(&self, user_id: u16, room_id: u16) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT OR IGNORE INTO room_memberships (user_id, room_id) VALUES (?, ?)"
        )
        .bind(user_id as i64)
        .bind(room_id as i64)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Removes a user from a room's membership list.
    ///
    /// # Arguments
    /// * `user_id` - The user ID to remove
    /// * `room_id` - The room ID to leave
    pub async fn remove_user_from_room(&self, user_id: u16, room_id: u16) -> Result<(), sqlx::Error> {
        sqlx::query(
            "DELETE FROM room_memberships WHERE user_id = ? AND room_id = ?"
        )
        .bind(user_id as i64)
        .bind(room_id as i64)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Retrieves all rooms that a user is a member of.
    ///
    /// # Arguments
    /// * `user_id` - The user ID to look up
    ///
    /// # Returns
    /// A list of `Room` objects the user has joined, ordered by name.
    pub async fn get_user_rooms(&self, user_id: u16) -> Result<Vec<Room>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT r.id, r.name, r.description, r.state, r.password_hash
            FROM rooms r
            JOIN room_memberships rm ON r.id = rm.room_id
            WHERE rm.user_id = ?
            ORDER BY r.name
            "#
        )
        .bind(user_id as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut rooms = Vec::new();
        for row in rows {
            let state = match row.get::<String, _>("state").as_str() {
                "Open" => RoomState::Open,
                "Private" => RoomState::Private,
                "Closed" => RoomState::Closed,
                _ => RoomState::Open,
            };

            let room = if let Some(password_hash) = row.get::<Option<String>, _>("password_hash") {
                Room::new_with_password(
                    row.get::<i64, _>("id") as u16,
                    row.get("name"),
                    row.get("description"),
                    password_hash,
                )
            } else {
                Room::new(
                    row.get::<i64, _>("id") as u16,
                    row.get("name"),
                    row.get("description"),
                    state,
                )
            };

            rooms.push(room);
        }

        Ok(rooms)
    }

    /// Retrieves the list of user IDs that are members of a room.
    ///
    /// # Arguments
    /// * `room_id` - The room ID to query
    ///
    /// # Returns
    /// A vector of user IDs for all members in the room.
    pub async fn get_room_members(&self, room_id: u16) -> Result<Vec<u16>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT user_id FROM room_memberships WHERE room_id = ?"
        )
        .bind(room_id as i64)
        .fetch_all(&self.pool)
        .await?;

        let members: Vec<u16> = rows.into_iter()
            .map(|row| row.get::<i64, _>("user_id") as u16)
            .collect();

        Ok(members)
    }

    /// Saves a chat message to the database.
    ///
    /// # Arguments
    /// * `room_id` - The room where the message was sent
    /// * `sender_id` - The user ID of the sender
    /// * `sender_username` - The username of the sender (denormalized for performance)
    /// * `content` - The message content
    /// * `message_type` - Type of message: "user", "system", or "private"
    ///
    /// # Returns
    /// The auto-generated message ID.
    pub async fn save_message(&self, room_id: u16, sender_id: u16, sender_username: &str, content: &str, message_type: &str) -> Result<i64, sqlx::Error> {
        let result = sqlx::query(
            "INSERT INTO messages (room_id, sender_id, sender_username, content, message_type) VALUES (?, ?, ?, ?, ?) RETURNING id"
        )
        .bind(room_id as i64)
        .bind(sender_id as i64)
        .bind(sender_username)
        .bind(content)
        .bind(message_type)
        .fetch_one(&self.pool)
        .await?;

        Ok(result.get("id"))
    }

    /// Retrieves message history for a room.
    ///
    /// # Arguments
    /// * `room_id` - The room to fetch messages from
    /// * `limit` - Maximum number of messages to retrieve
    ///
    /// # Returns
    /// Messages in chronological order (oldest first).
    pub async fn get_room_messages(&self, room_id: u16, limit: i64) -> Result<Vec<StoredMessage>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT id, room_id, sender_id, sender_username, content, message_type, timestamp
            FROM messages
            WHERE room_id = ?
            ORDER BY timestamp DESC
            LIMIT ?
            "#
        )
        .bind(room_id as i64)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let mut messages = Vec::new();
        for row in rows {
            messages.push(StoredMessage {
                id: row.get("id"),
                room_id: row.get::<i64, _>("room_id") as u16,
                sender_id: row.get::<i64, _>("sender_id") as u16,
                sender_username: row.get("sender_username"),
                content: row.get("content"),
                timestamp: row.get("timestamp"),
                message_type: row.get("message_type"),
            });
        }

        messages.reverse();
        Ok(messages)
    }

    /// Retrieves the 50 most recent messages from a room.
    ///
    /// Convenience wrapper around `get_room_messages` with a default limit.
    ///
    /// # Arguments
    /// * `room_id` - The room to fetch messages from
    pub async fn get_recent_messages(&self, room_id: u16) -> Result<Vec<StoredMessage>, sqlx::Error> {
        self.get_room_messages(room_id, 50).await
    }

    /// Cleans up stale user sessions that have been inactive.
    ///
    /// Marks users as offline if they haven't had activity in the last minute.
    /// Should be called periodically to keep online status accurate.
    pub async fn cleanup_old_sessions(&self) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            UPDATE user_sessions 
            SET is_online = FALSE 
            WHERE last_activity < datetime('now', '-1 minutes') AND is_online = TRUE
            "#
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            UPDATE users 
            SET is_online = FALSE, last_seen = CURRENT_TIMESTAMP
            WHERE id IN (
                SELECT user_id FROM user_sessions 
                WHERE last_activity < datetime('now', '-1 minutes') AND is_online = FALSE
            )
            "#
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Loads all user accounts from the database into memory.
    ///
    /// # Returns
    /// A HashMap keyed by username containing all registered accounts.
    pub async fn load_all_accounts(&self) -> Result<HashMap<String, Account>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, username, password_hash FROM users"
        )
        .fetch_all(&self.pool)
        .await?;

        let mut accounts = HashMap::new();
        for row in rows {
            let account = Account::new(
                row.get::<String, _>("username").clone(),
                row.get("password_hash"),
                row.get::<i64, _>("id") as u16,
            );
            accounts.insert(row.get("username"), account);
        }

        Ok(accounts)
    }

    /// Loads all rooms from the database into memory.
    ///
    /// # Returns
    /// A HashMap keyed by room ID containing all rooms.
    pub async fn load_all_rooms(&self) -> Result<HashMap<u16, Room>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, name, description, state, password_hash FROM rooms"
        )
        .fetch_all(&self.pool)
        .await?;

        let mut rooms = HashMap::new();
        for row in rows {
            let state = match row.get::<String, _>("state").as_str() {
                "Open" => RoomState::Open,
                "Private" => RoomState::Private,
                "Closed" => RoomState::Closed,
                _ => RoomState::Open,
            };

            let room = if let Some(password_hash) = row.get::<Option<String>, _>("password_hash") {
                Room::new_with_password(
                    row.get::<i64, _>("id") as u16,
                    row.get("name"),
                    row.get("description"),
                    password_hash,
                )
            } else {
                Room::new(
                    row.get::<i64, _>("id") as u16,
                    row.get("name"),
                    row.get("description"),
                    state,
                )
            };

            rooms.insert(row.get::<i64, _>("id") as u16, room);
        }

        Ok(rooms)
    }

    /// Increments the user count for a room by 1.
    ///
    /// # Arguments
    /// * `room_id` - The room to update
    pub async fn increment_room_user_count(&self, room_id: u16) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE rooms SET user_count = user_count + 1 WHERE id = ?")
            .bind(room_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Decrements the user count for a room by 1 (minimum 0).
    ///
    /// # Arguments
    /// * `room_id` - The room to update
    pub async fn decrement_room_user_count(&self, room_id: u16) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE rooms SET user_count = user_count - 1 WHERE id = ? AND user_count > 0")
            .bind(room_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
