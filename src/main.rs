mod resc;
mod server;
mod client;
mod tui_client;
mod database;
use server::AsyncChatServer;
use client::ChatClient;
use tui_client::TuiChatClient;
use std::env;

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
                .open(&db_path) {
                eprintln!("Cannot create database file at {}: {}", db_path.display(), e);
                eprintln!("Please check directory permissions or run from a writable directory");
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
            println!("Starting CLI Chat Client as '{}' connecting to '{}'...", username, server_address);
            if let Err(e) = ChatClient::start_client(&server_address, &username) {
                eprintln!("Failed to connect to server: {}", e);
            }
        }
        Some("tui") => {
            let server_address = args.get(2).unwrap_or(&"127.0.0.1:8080".to_string()).clone();
            println!("Starting TUI Chat Client connecting to '{}'...", server_address);
            if let Err(e) = TuiChatClient::start_tui(&server_address).await {
                eprintln!("Failed to start TUI client: {}", e);
            }
        }
        _ => {
            println!("RustyRoom - TCP Chat Application");
            println!();
            println!("Usage:");
            println!("  cargo run server [bind_address]              - Start async chat server with database");
            println!("  cargo run client <username> [server_address] - Start CLI client");
            println!("  cargo run tui [server_address]               - Start TUI client (recommended)");
            println!();
            println!("Examples:");
            println!("  cargo run server                             - Start server on 127.0.0.1:8080");
            println!("  cargo run server 0.0.0.0:8080               - Start server on all interfaces");
            println!("  cargo run client Alice                       - Connect to 127.0.0.1:8080");
            println!("  cargo run client Alice 192.168.1.100:8080   - Connect to specific server");
            println!("  cargo run tui                                - Connect TUI to 127.0.0.1:8080");
            println!("  cargo run tui 192.168.1.100:8080            - Connect TUI to specific server");
            println!();
            println!("VPS Deployment:");
            println!("  On VPS: cargo run server 0.0.0.0:8080");
            println!("  Client: cargo run tui <VPS_IP>:8080");
        }
    }
}
