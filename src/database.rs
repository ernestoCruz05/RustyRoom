//! Database module for RustyRoom Chat Server
//!
//! Handles SQLite persistence for:
//! - User accounts and authentication
//! - Servers (guilds) and memberships
//! - Channels (text & voice) within servers
//! - Message history
//! - User online/offline status tracking

use crate::resc::{Account, Channel, ChannelInfo, ChannelType, Server, ServerInfo, ServerWithChannels};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool, sqlite::SqlitePoolOptions};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: i64,
    pub channel_id: u16,
    pub sender_id: u16,
    pub sender_username: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub message_type: String,
}

#[derive(Debug, Clone)]
pub struct UserSession {
    pub user_id: u16,
    pub username: String,
    pub is_online: bool,
    pub last_seen: DateTime<Utc>,
}

pub struct Database {
    pool: SqlitePool,
}

impl Database {
    pub async fn new(database_url: &str) -> Result<Self, sqlx::Error> {
        println!(" Attempting database connection: {}", database_url);

        let pool = SqlitePoolOptions::new()
            .max_connections(100)
            .connect(database_url)
            .await
            .map_err(|e| {
                eprintln!("Failed to create database pool: {}", e);
                e
            })?;

        println!("Database pool created successfully");

        let db = Database { pool };
        db.init_tables().await?;
        println!("Database tables initialized successfully");

        Ok(db)
    }

    async fn init_tables(&self) -> Result<(), sqlx::Error> {
        // Users
        sqlx::query(
            r#"CREATE TABLE IF NOT EXISTS users (
                id INTEGER PRIMARY KEY,
                username TEXT UNIQUE NOT NULL,
                password_hash TEXT NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                last_seen DATETIME DEFAULT CURRENT_TIMESTAMP,
                is_online BOOLEAN DEFAULT FALSE
            )"#,
        )
        .execute(&self.pool)
        .await?;

        // Servers (guilds)
        sqlx::query(
            r#"CREATE TABLE IF NOT EXISTS servers (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                owner_id INTEGER NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (owner_id) REFERENCES users (id)
            )"#,
        )
        .execute(&self.pool)
        .await?;

        // Server memberships
        sqlx::query(
            r#"CREATE TABLE IF NOT EXISTS server_memberships (
                user_id INTEGER,
                server_id INTEGER,
                joined_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (user_id, server_id),
                FOREIGN KEY (user_id) REFERENCES users (id),
                FOREIGN KEY (server_id) REFERENCES servers (id)
            )"#,
        )
        .execute(&self.pool)
        .await?;

        // Channels (text & voice) within servers
        sqlx::query(
            r#"CREATE TABLE IF NOT EXISTS channels (
                id INTEGER PRIMARY KEY,
                server_id INTEGER NOT NULL,
                name TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                channel_type TEXT NOT NULL DEFAULT 'text',
                password_hash TEXT,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (server_id) REFERENCES servers (id)
            )"#,
        )
        .execute(&self.pool)
        .await?;

        // Messages (linked to channels)
        sqlx::query(
            r#"CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                channel_id INTEGER NOT NULL,
                sender_id INTEGER,
                sender_username TEXT NOT NULL,
                content TEXT NOT NULL,
                message_type TEXT NOT NULL DEFAULT 'user',
                timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (channel_id) REFERENCES channels (id),
                FOREIGN KEY (sender_id) REFERENCES users (id)
            )"#,
        )
        .execute(&self.pool)
        .await?;

        // User sessions
        sqlx::query(
            r#"CREATE TABLE IF NOT EXISTS user_sessions (
                user_id INTEGER PRIMARY KEY,
                username TEXT NOT NULL,
                is_online BOOLEAN DEFAULT TRUE,
                last_activity DATETIME DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (user_id) REFERENCES users (id)
            )"#,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    // ========================================================================
    // USER OPERATIONS
    // ========================================================================

    pub async fn create_user(&self, username: &str, password_hash: &str) -> Result<u16, sqlx::Error> {
        let result = sqlx::query(
            "INSERT INTO users (username, password_hash) VALUES (?, ?) RETURNING id",
        )
        .bind(username)
        .bind(password_hash)
        .fetch_one(&self.pool)
        .await?;
        Ok(result.get::<i64, _>("id") as u16)
    }

    pub async fn get_user_by_username(&self, username: &str) -> Result<Option<Account>, sqlx::Error> {
        let row = sqlx::query("SELECT id, username, password_hash FROM users WHERE username = ?")
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

    pub async fn get_user_by_id(&self, user_id: u16) -> Result<Option<Account>, sqlx::Error> {
        let row = sqlx::query("SELECT id, username, password_hash FROM users WHERE id = ?")
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

    pub async fn set_user_online_status(&self, user_id: u16, username: &str, is_online: bool) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"INSERT INTO user_sessions (user_id, username, is_online, last_activity)
            VALUES (?, ?, ?, CURRENT_TIMESTAMP)
            ON CONFLICT(user_id) DO UPDATE SET
                is_online = excluded.is_online,
                last_activity = excluded.last_activity"#,
        )
        .bind(user_id as i64)
        .bind(username)
        .bind(is_online)
        .execute(&self.pool)
        .await?;

        sqlx::query("UPDATE users SET is_online = ?, last_seen = CURRENT_TIMESTAMP WHERE id = ?")
            .bind(is_online)
            .bind(user_id as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_online_users(&self) -> Result<Vec<UserSession>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT user_id, username, is_online, last_activity FROM user_sessions ORDER BY is_online DESC, username ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|row| UserSession {
            user_id: row.get::<i64, _>("user_id") as u16,
            username: row.get("username"),
            is_online: row.get("is_online"),
            last_seen: row.get("last_activity"),
        }).collect())
    }

    pub async fn cleanup_old_sessions(&self) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE user_sessions SET is_online = FALSE WHERE last_activity < datetime('now', '-1 minutes') AND is_online = TRUE",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"UPDATE users SET is_online = FALSE, last_seen = CURRENT_TIMESTAMP
            WHERE id IN (
                SELECT user_id FROM user_sessions
                WHERE last_activity < datetime('now', '-1 minutes') AND is_online = FALSE
            )"#,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ========================================================================
    // SERVER OPERATIONS
    // ========================================================================

    pub async fn create_server(&self, name: &str, description: &str, owner_id: u16) -> Result<u16, sqlx::Error> {
        let result = sqlx::query(
            "INSERT INTO servers (name, description, owner_id) VALUES (?, ?, ?) RETURNING id",
        )
        .bind(name)
        .bind(description)
        .bind(owner_id as i64)
        .fetch_one(&self.pool)
        .await?;
        let server_id = result.get::<i64, _>("id") as u16;

        // Owner auto-joins the server
        self.add_user_to_server(owner_id, server_id).await?;

        Ok(server_id)
    }

    pub async fn get_server(&self, server_id: u16) -> Result<Option<Server>, sqlx::Error> {
        let row = sqlx::query("SELECT id, name, description, owner_id FROM servers WHERE id = ?")
            .bind(server_id as i64)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(row) => Ok(Some(Server::new(
                row.get::<i64, _>("id") as u16,
                row.get("name"),
                row.get("description"),
                row.get::<i64, _>("owner_id") as u16,
            ))),
            None => Ok(None),
        }
    }

    pub async fn get_all_servers(&self) -> Result<Vec<ServerInfo>, sqlx::Error> {
        let rows = sqlx::query(
            r#"SELECT s.id, s.name, s.description, s.owner_id,
                      (SELECT COUNT(*) FROM server_memberships sm WHERE sm.server_id = s.id) as member_count
               FROM servers s ORDER BY s.name"#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|row| ServerInfo {
            id: row.get::<i64, _>("id") as u16,
            name: row.get("name"),
            description: row.get("description"),
            owner_id: row.get::<i64, _>("owner_id") as u16,
            member_count: row.get::<i64, _>("member_count") as u32,
        }).collect())
    }

    pub async fn add_user_to_server(&self, user_id: u16, server_id: u16) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT OR IGNORE INTO server_memberships (user_id, server_id) VALUES (?, ?)")
            .bind(user_id as i64)
            .bind(server_id as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn remove_user_from_server(&self, user_id: u16, server_id: u16) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM server_memberships WHERE user_id = ? AND server_id = ?")
            .bind(user_id as i64)
            .bind(server_id as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_user_servers(&self, user_id: u16) -> Result<Vec<ServerInfo>, sqlx::Error> {
        let rows = sqlx::query(
            r#"SELECT s.id, s.name, s.description, s.owner_id,
                      (SELECT COUNT(*) FROM server_memberships sm2 WHERE sm2.server_id = s.id) as member_count
               FROM servers s
               JOIN server_memberships sm ON s.id = sm.server_id
               WHERE sm.user_id = ?
               ORDER BY s.name"#,
        )
        .bind(user_id as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|row| ServerInfo {
            id: row.get::<i64, _>("id") as u16,
            name: row.get("name"),
            description: row.get("description"),
            owner_id: row.get::<i64, _>("owner_id") as u16,
            member_count: row.get::<i64, _>("member_count") as u32,
        }).collect())
    }

    pub async fn get_server_member_ids(&self, server_id: u16) -> Result<Vec<u16>, sqlx::Error> {
        let rows = sqlx::query("SELECT user_id FROM server_memberships WHERE server_id = ?")
            .bind(server_id as i64)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|row| row.get::<i64, _>("user_id") as u16).collect())
    }

    pub async fn is_user_in_server(&self, user_id: u16, server_id: u16) -> Result<bool, sqlx::Error> {
        let row = sqlx::query(
            "SELECT 1 FROM server_memberships WHERE user_id = ? AND server_id = ?",
        )
        .bind(user_id as i64)
        .bind(server_id as i64)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    /// Get full sync data for a user: all their servers with channels
    pub async fn get_user_servers_with_channels(&self, user_id: u16) -> Result<Vec<ServerWithChannels>, sqlx::Error> {
        let servers = self.get_user_servers(user_id).await?;
        let mut result = Vec::new();
        for server in servers {
            let channels = self.get_server_channels(server.id).await?;
            result.push(ServerWithChannels { server, channels });
        }
        Ok(result)
    }

    // ========================================================================
    // CHANNEL OPERATIONS
    // ========================================================================

    pub async fn create_channel(
        &self,
        server_id: u16,
        name: &str,
        description: &str,
        channel_type: &ChannelType,
        password_hash: Option<&str>,
    ) -> Result<u16, sqlx::Error> {
        let result = sqlx::query(
            "INSERT INTO channels (server_id, name, description, channel_type, password_hash) VALUES (?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(server_id as i64)
        .bind(name)
        .bind(description)
        .bind(channel_type.as_str())
        .bind(password_hash)
        .fetch_one(&self.pool)
        .await?;
        Ok(result.get::<i64, _>("id") as u16)
    }

    pub async fn get_channel(&self, channel_id: u16) -> Result<Option<Channel>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT id, server_id, name, description, channel_type, password_hash FROM channels WHERE id = ?",
        )
        .bind(channel_id as i64)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => {
                let ch_type = ChannelType::from_str(row.get::<String, _>("channel_type").as_str());
                let channel = match row.get::<Option<String>, _>("password_hash") {
                    Some(ph) => Channel::new_with_password(
                        row.get::<i64, _>("id") as u16,
                        row.get::<i64, _>("server_id") as u16,
                        row.get("name"),
                        row.get("description"),
                        ch_type,
                        ph,
                    ),
                    None => Channel::new(
                        row.get::<i64, _>("id") as u16,
                        row.get::<i64, _>("server_id") as u16,
                        row.get("name"),
                        row.get("description"),
                        ch_type,
                    ),
                };
                Ok(Some(channel))
            }
            None => Ok(None),
        }
    }

    pub async fn get_server_channels(&self, server_id: u16) -> Result<Vec<ChannelInfo>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, server_id, name, description, channel_type, password_hash FROM channels WHERE server_id = ? ORDER BY channel_type, name",
        )
        .bind(server_id as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|row| ChannelInfo {
            id: row.get::<i64, _>("id") as u16,
            server_id: row.get::<i64, _>("server_id") as u16,
            name: row.get("name"),
            description: row.get("description"),
            channel_type: ChannelType::from_str(row.get::<String, _>("channel_type").as_str()),
            is_password_protected: row.get::<Option<String>, _>("password_hash").is_some(),
        }).collect())
    }

    // ========================================================================
    // MESSAGE OPERATIONS
    // ========================================================================

    pub async fn save_message(
        &self,
        channel_id: u16,
        sender_id: u16,
        sender_username: &str,
        content: &str,
        message_type: &str,
    ) -> Result<i64, sqlx::Error> {
        let result = sqlx::query(
            "INSERT INTO messages (channel_id, sender_id, sender_username, content, message_type) VALUES (?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(channel_id as i64)
        .bind(sender_id as i64)
        .bind(sender_username)
        .bind(content)
        .bind(message_type)
        .fetch_one(&self.pool)
        .await?;
        Ok(result.get("id"))
    }

    pub async fn get_channel_messages(&self, channel_id: u16, limit: i64) -> Result<Vec<StoredMessage>, sqlx::Error> {
        let rows = sqlx::query(
            r#"SELECT id, channel_id, sender_id, sender_username, content, message_type, timestamp
               FROM messages WHERE channel_id = ? ORDER BY timestamp DESC LIMIT ?"#,
        )
        .bind(channel_id as i64)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let mut messages: Vec<StoredMessage> = rows.into_iter().map(|row| StoredMessage {
            id: row.get("id"),
            channel_id: row.get::<i64, _>("channel_id") as u16,
            sender_id: row.get::<i64, _>("sender_id") as u16,
            sender_username: row.get("sender_username"),
            content: row.get("content"),
            timestamp: row.get("timestamp"),
            message_type: row.get("message_type"),
        }).collect();
        messages.reverse();
        Ok(messages)
    }

    pub async fn get_recent_messages(&self, channel_id: u16) -> Result<Vec<StoredMessage>, sqlx::Error> {
        self.get_channel_messages(channel_id, 50).await
    }
}
