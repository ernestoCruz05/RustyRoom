mod resc;
mod server;
mod client;
mod tui_client;
mod database;
use server::{ChatServer, AsyncChatServer};
use client::ChatClient;
use tui_client::TuiChatClient;
use std::env;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    
    match args.get(1).map(|s| s.as_str()) {
        Some("server") => {
            println!("Starting TCP Chat Server...");
            if let Err(e) = ChatServer::start_server("127.0.0.1:8080") {
                eprintln!("Failed to start server: {}", e);
            }
        }
        Some("async-server") => {
            println!("Starting Async TCP Chat Server with database...");
            
            // Ensure we can write to the current directory
            let current_dir = match env::current_dir() {
                Ok(dir) => dir,
                Err(e) => {
                    eprintln!("Cannot determine current directory: {}", e);
                    return;
                }
            };
            
            let db_path = current_dir.join("chat.db");
            println!("Database file: {}", db_path.display());
            
            // Check if we can write to this directory
            if let Err(e) = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .open(&db_path) {
                eprintln!("Cannot create database file at {}: {}", db_path.display(), e);
                eprintln!("Please check directory permissions or run from a writable directory");
                return;
            }
            
            let db_url = format!("sqlite:{}", db_path.display());
            
            if let Err(e) = AsyncChatServer::start_async_server(&db_url, "127.0.0.1:8080").await {
                eprintln!("Failed to start async server: {}", e);
                eprintln!("Fallback: Try running 'cargo run server' for non-persistent chat");
            }
        }
        Some("client") => {
            let username = args.get(2).unwrap_or(&"Anonymous".to_string()).clone();
            println!("Starting CLI Chat Client as '{}'...", username);
            if let Err(e) = ChatClient::start_client("127.0.0.1:8080", &username) {
                eprintln!("Failed to connect to server: {}", e);
            }
        }
        Some("tui") => {
            if let Err(e) = TuiChatClient::start_tui("127.0.0.1:8080").await {
                eprintln!("Failed to start TUI client: {}", e);
            }
        }
        _ => {
            println!("FCA - TCP Chat Application");
            println!();
            println!("Usage:");
            println!("  cargo run server              - Start chat server");
            println!("  cargo run client <username>   - Start CLI client");
            println!("  cargo run tui                 - Start TUI client (recommended)");
            println!();
            println!("Examples:");
            println!("  cargo run server");
            println!("  cargo run client Alice");
            println!("  cargo run tui");
        }
    }
}