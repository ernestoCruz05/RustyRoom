use rustyroom_alpha::client::tui::main::TuiChatClient;

#[tokio::main]
async fn main() {
	let args: Vec<String> = std::env::args().collect();
	let server_address = args.get(1).cloned().unwrap_or_else(|| "127.0.0.1:8080".to_string());
	if let Err(e) = TuiChatClient::start_tui(&server_address).await {
		eprintln!("TUI client error: {}", e);
	}
}

