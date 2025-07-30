/*
 Entry point oh yeah!
*/

mod resc;
mod server;
mod client;
mod tui_client;
use server::ChatServer;
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