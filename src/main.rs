/*
 Entry point oh yeah!
*/

mod resc;
mod server;
mod client;
use server::ChatServer;
use client::ChatClient;
use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();
    
    if args.len() > 1 && args[1] == "client" {
        let username = args.get(2).unwrap_or(&"Anonymous".to_string()).clone();
        println!("Starting TCP Chat Client as '{}'...", username);
        
        if let Err(e) = ChatClient::start_client("127.0.0.1:8080", &username) {
            eprintln!("Failed to connect to server: {}", e);
        }
    } else {
        println!("Starting TCP Chat Server...");
        
        if let Err(e) = ChatServer::start_server("127.0.0.1:8080") {
            eprintln!("Failed to start server: {}", e);
        }
    }
}