use rustyroom_alpha::client::cli::main::ChatClient;

fn main() {
	let args: Vec<String> = std::env::args().collect();
	let username = args.get(1).cloned().unwrap_or_else(|| "Anonymous".to_string());
	let server_address = args.get(2).cloned().unwrap_or_else(|| "127.0.0.1:8080".to_string());
	if let Err(e) = ChatClient::start_client(&server_address, &username) {
		eprintln!("Client error: {}", e);
	}
}

