mod audio;
mod client;
mod database;
mod resc;
mod server;
mod tui_client;
mod voice;

use client::ChatClient;
use server::AsyncChatServer;
use std::env;
use tui_client::TuiChatClient;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        Some("server") => {
            println!("Starting Async TCP Chat Server with database...");

            let bind_address = args.get(2).unwrap_or(&"127.0.0.1:8080".to_string()).clone();
            println!("Server will bind to: {}", bind_address);

            let current_dir = match env::current_dir() {
                Ok(dir) => dir,
                Err(e) => {
                    eprintln!("Cannot determine current directory: {}", e);
                    return;
                }
            };

            let db_path = current_dir.join("chat.db");
            println!("Database file: {}", db_path.display());

            if let Err(e) = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .open(&db_path)
            {
                eprintln!(
                    "Cannot create database file at {}: {}",
                    db_path.display(),
                    e
                );
                return;
            }

            let db_url = format!("sqlite:{}", db_path.display());

            if let Err(e) = AsyncChatServer::start_async_server(&db_url, &bind_address).await {
                eprintln!("Failed to start async server: {}", e);
            }
        }
        Some("client") => {
            let username = args.get(2).unwrap_or(&"Anonymous".to_string()).clone();
            let server_address = args.get(3).unwrap_or(&"127.0.0.1:8080".to_string()).clone();
            println!(
                "Starting CLI Chat Client as '{}' connecting to '{}'...",
                username, server_address
            );
            if let Err(e) = ChatClient::start_client(&server_address, &username) {
                eprintln!("Failed to connect to server: {}", e);
            }
        }
        Some("tui") => {
            let server_address = args.get(2).unwrap_or(&"127.0.0.1:8080".to_string()).clone();
            println!(
                "Starting TUI Chat Client connecting to '{}'...",
                server_address
            );
            if let Err(e) = TuiChatClient::start_tui(&server_address).await {
                eprintln!("Failed to start TUI client: {}", e);
            }
        }
        _ => {
            println!("RustyRoom - TCP Chat Application");
            println!("Usage: cargo run [server|client|tui] ...");
        }
    }
}
