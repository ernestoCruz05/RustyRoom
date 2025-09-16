use rustyroom_alpha::server::AsyncChatServer;

#[tokio::main]
async fn main() {
	let args: Vec<String> = std::env::args().collect();
	let bind_address = args.get(1).cloned().unwrap_or_else(|| "127.0.0.1:8080".to_string());
	let current_dir = std::env::current_dir().expect("cwd");
	let db_path = current_dir.join("chat.db");
	let db_url = format!("sqlite:{}", db_path.display());
	if let Err(e) = AsyncChatServer::start_async_server(&db_url, &bind_address).await {
		eprintln!("Server error: {}", e);
	}
}

