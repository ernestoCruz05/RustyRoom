//! Command-Line Chat Client Module for FCA
//!
//! This module provides a simple command-line interface client for the FCA chat system.
//! It handles:
//! - TCP connection to the server
//! - User authentication (login/register)
//! - Sending and receiving messages
//! - Voice chat via UDP with Opus codec
//!
//! Note: For a more user-friendly interface, see the TUI client in tui_client.rs

#![allow(dead_code)]

use crate::audio::{self, FrameCollector, FRAME_SIZE};
use crate::resc::{Message, MessageType};
use crate::voice::{
    OpusDecoder, OpusEncoder, VoiceActivityDetector, VoicePacket, VoicePacketHeader,
    JitterBuffer, VAD_THRESHOLD, JITTER_BUFFER_TARGET_MS,
};
use ringbuf::traits::*;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// ============================================================================
// CHAT CLIENT
// ============================================================================

/// Command-line chat client for connecting to FCA servers
///
/// This client provides basic text-based interaction with the chat server.
/// For a graphical interface, use TuiChatClient instead.
#[allow(dead_code)]
#[derive(Clone)]
pub struct ChatClient {
    /// Server address in "host:port" format
    server_address: String,
    /// User's unique ID (assigned after authentication)
    user_id: u16,
    /// User's display name
    username: String,
    /// Whether the user has been authenticated
    authenticated: bool,
}

impl ChatClient {
    /// Creates a new ChatClient instance
    ///
    /// # Arguments
    /// * `server_address` - Server address in "host:port" format
    /// * `username` - Desired username for this session
    pub fn new(server_address: String, username: String) -> Self {
        ChatClient {
            server_address,
            user_id: 0,
            username,
            authenticated: false,
        }
    }

    /// Connects to the server and starts the main client loop
    ///
    /// This method:
    /// 1. Establishes a TCP connection to the server
    /// 2. Spawns a thread to receive incoming messages
    /// 3. Handles user input in the main thread
    pub fn connect(&mut self) -> io::Result<()> {
        let stream = TcpStream::connect(&self.server_address)?;
        println!("Connected to server at {}", self.server_address);

        let read_stream = stream.try_clone()?;
        let mut write_stream = stream;

        let client_clone = Arc::new(Mutex::new(self.clone()));
        let client_for_receiver = Arc::clone(&client_clone);

        thread::spawn(move || {
            Self::message_receiver(read_stream, client_for_receiver);
        });

        Self::show_auth_commands();

        self.handle_user_input(&mut write_stream, client_clone)?;

        Ok(())
    }

    /// Background thread that receives and processes messages from the server
    ///
    /// Handles various message types including:
    /// - Voice credentials (starts UDP voice client)
    /// - Authentication responses
    /// - Room messages and private messages
    fn message_receiver(stream: TcpStream, client: Arc<Mutex<ChatClient>>) {
        let reader = BufReader::new(stream);

        let get_server_ip = |client: &Arc<Mutex<ChatClient>>| -> String {
            let lock = client.lock().unwrap();
            let addr = &lock.server_address;
            addr.split(':').next().unwrap_or("127.0.0.1").to_string()
        };

        for line in reader.lines() {
            match line {
                Ok(json_message) => {
                    if let Ok(message) = Message::from_json_file(&json_message) {
                        if let MessageType::VoiceCredentials { token, udp_port, channel_id: _ } =
                            &message.message_type
                        {
                            println!("[Voice] Received credentials. Connecting...");

                            let server_ip = get_server_ip(&client);
                            let token_clone = token.clone();
                            let port_clone = *udp_port;

                            thread::spawn(move || {
                                Self::start_udp_echo_client(server_ip, port_clone, token_clone);
                            });

                            continue;
                        }

                        if let MessageType::AuthSuccess { user_id, .. } = &message.message_type {
                            {
                                let mut client_lock = client.lock().unwrap();
                                client_lock.authenticated = true;
                                client_lock.user_id = *user_id;
                            }
                            Self::display_message(message);
                            Self::show_chat_commands();
                        } else {
                            Self::display_message(message);
                        }
                    }
                }
                Err(_) => {
                    println!("Disconnected from server");
                    break;
                }
            }
        }
    }

    fn start_udp_echo_client(server_ip: String, udp_port: u16, token: String) {
        // Initialize audio manager
        let (_manager, mut mic_consumer, mut speaker_producer) = match audio::AudioManager::new() {
            Ok(v) => v,
            Err(e) => {
                println!("[VOICE][ERROR] Audio init failed: {}", e);
                return;
            }
        };

        // Initialize Opus encoder/decoder
        let mut encoder = match OpusEncoder::new() {
            Ok(e) => e,
            Err(e) => {
                println!("[VOICE][ERROR] Opus encoder init failed: {}", e);
                return;
            }
        };

        let mut decoder = match OpusDecoder::new() {
            Ok(d) => d,
            Err(e) => {
                println!("[VOICE][ERROR] Opus decoder init failed: {}", e);
                return;
            }
        };

        // Initialize voice activity detector
        let mut vad = VoiceActivityDetector::new(VAD_THRESHOLD);

        // Initialize jitter buffer for smooth playback
        let mut jitter_buffer = JitterBuffer::new(JITTER_BUFFER_TARGET_MS, 50);

        // Frame collector for accumulating samples
        let mut frame_collector = FrameCollector::new(FRAME_SIZE);

        // UDP socket setup
        let socket = match UdpSocket::bind("0.0.0.0:0") {
            Ok(s) => s,
            Err(e) => {
                println!("[VOICE][ERROR] UDP bind failed: {}", e);
                return;
            }
        };

        let server_addr = format!("{}:{}", server_ip, udp_port);

        // Send authentication token
        if let Err(e) = socket.send_to(token.as_bytes(), &server_addr) {
            println!("[VOICE][ERROR] Handshake failed: {}", e);
            return;
        }

        let mut buf = [0u8; 4096];
        let _ = socket.set_read_timeout(Some(Duration::from_millis(5)));

        // Packet sequence number
        let mut sequence: u16 = 0;
        let mut timestamp: u32 = 0;

        // Voice state
        let running = Arc::new(AtomicBool::new(true));
        let is_connected = Arc::new(AtomicBool::new(false));

        // Clone for Ctrl+C handler
        let running_clone = Arc::clone(&running);
        ctrlc_handler(running_clone);

        println!("[VOICE] Starting voice loop...");

        while running.load(Ordering::Relaxed) {
            // ===== CAPTURE & ENCODE =====
            // Collect samples from microphone
            while let Some(sample) = mic_consumer.try_pop() {
                frame_collector.push_samples(&[sample]);
            }

            // Process complete frames
            while let Some(frame) = frame_collector.pop_frame() {
                // Voice activity detection - only send if speaking
                if vad.process(&frame) {
                    // Encode with Opus
                    match encoder.encode(&frame) {
                        Ok(encoded_data) => {
                            // Create voice packet
                            let header = VoicePacketHeader::new_audio(
                                0, // user_id will be filled by server
                                0, // room_id will be filled by server
                                sequence,
                                timestamp,
                                encoded_data.len() as u16,
                            );
                            let packet = VoicePacket::new(header, encoded_data);
                            let packet_bytes = packet.encode();

                            // Send packet
                            if let Err(e) = socket.send_to(&packet_bytes, &server_addr) {
                                if running.load(Ordering::Relaxed) {
                                    eprintln!("[VOICE] Send error: {}", e);
                                }
                            }

                            sequence = sequence.wrapping_add(1);
                        }
                        Err(e) => {
                            eprintln!("[VOICE] Encode error: {}", e);
                        }
                    }
                }
                timestamp = timestamp.wrapping_add(FRAME_SIZE as u32);
            }

            // ===== RECEIVE & DECODE =====
            match socket.recv_from(&mut buf) {
                Ok((size, _src)) => {
                    // Check for connection acknowledgment
                    if size == 15 && &buf[..15] == b"VOICE_CONNECTED" {
                        is_connected.store(true, Ordering::Relaxed);
                        println!("[VOICE] Connected 🟢 (Opus codec active)");
                        continue;
                    }

                    // Try to decode as voice packet
                    if let Some(packet) = VoicePacket::decode(&buf[..size]) {
                        // Add to jitter buffer
                        jitter_buffer.push(packet);
                    } else {
                        // Legacy: raw float samples (backwards compatibility)
                        let sample_count = size / 4;
                        if sample_count > 0 && size % 4 == 0 {
                            let samples: &[f32] = unsafe {
                                std::slice::from_raw_parts(buf.as_ptr() as *const f32, sample_count)
                            };
                            for &sample in samples {
                                let _ = speaker_producer.try_push(sample);
                            }
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // Timeout, continue
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    // Timeout, continue
                }
                Err(e) => {
                    if running.load(Ordering::Relaxed) {
                        eprintln!("[VOICE] Receive error: {}", e);
                    }
                }
            }

            // ===== PLAYBACK FROM JITTER BUFFER =====
            while let Some(packet) = jitter_buffer.pop() {
                match decoder.decode(&packet.payload) {
                    Ok(decoded_samples) => {
                        for sample in decoded_samples {
                            let _ = speaker_producer.try_push(sample);
                        }
                    }
                    Err(e) => {
                        eprintln!("[VOICE] Decode error: {}", e);
                        // Use packet loss concealment
                        if let Ok(plc_samples) = decoder.decode_loss() {
                            for sample in plc_samples {
                                let _ = speaker_producer.try_push(sample);
                            }
                        }
                    }
                }
            }
        }

        // Print statistics on exit
        let (received, dropped) = jitter_buffer.get_stats();
        println!("[VOICE] Disconnected. Stats: {} packets received, {} dropped", received, dropped);
    }

    fn display_message(message: Message) {
        match message.message_type {
            MessageType::AuthSuccess {
                user_id,
                message: msg,
            } => {
                println!("[SUCCESS] {}", msg);
                println!("Your user ID is: {}", user_id);
            }
            MessageType::AuthFailure { reason } => {
                println!("[ERROR] {}", reason);
            }
            MessageType::ChannelMessage {
                channel_id,
                sender_username,
                content,
            } => {
                println!("[Channel {}] {}: {}", channel_id, sender_username, content);
            }
            MessageType::PrivateMessage {
                to_user_id: _,
                sender_username,
                content,
            } => {
                println!("[Private from {}]: {}", sender_username, content);
            }
            MessageType::ServerResponse { success, content } => {
                if success {
                    println!("[Server]: {}", content);
                } else {
                    println!("[Server Error]: {}", content);
                }
            }
            MessageType::ServerCreated {
                server_id,
                name,
                description,
            } => {
                println!(
                    "[SUCCESS] Server '{}' (ID: {}) created successfully!",
                    name, server_id
                );
                println!("         Description: {}", description);
            }
            MessageType::ServerJoined {
                server_id,
                name,
                description,
                channels,
            } => {
                println!("╔══════════════════════════════════════════════════════════════╗");
                println!("║ Joined server '{}' (ID: {})", name, server_id);
                println!("║ {}", description);
                println!("║ Channels:");
                for ch in channels {
                    println!("║   [{}] {} ({})", ch.id, ch.name, ch.channel_type.as_str());
                }
                println!("╚══════════════════════════════════════════════════════════════╝");
            }
            MessageType::ServerLeft { server_id } => {
                println!("[INFO] Left server {}", server_id);
            }
            MessageType::ServerList { servers } => {
                println!("Available servers:");
                for s in servers {
                    println!("  [{}] {} - {} ({} members)", s.id, s.name, s.description, s.member_count);
                }
            }
            MessageType::ChannelCreated { server_id, channel_id, name, channel_type, .. } => {
                println!("[INFO] Channel '{}' (ID: {}, {}) created in server {}", name, channel_id, channel_type.as_str(), server_id);
            }
            MessageType::ChannelJoined { channel_id, name, channel_type, .. } => {
                println!("[INFO] Joined channel '{}' (ID: {}, {})", name, channel_id, channel_type.as_str());
            }
            MessageType::ChannelList { server_id, channels } => {
                println!("Channels in server {}:", server_id);
                for ch in channels {
                    println!("  [{}] {} ({})", ch.id, ch.name, ch.channel_type.as_str());
                }
            }
            MessageType::UserServersSync { servers } => {
                println!("Your servers:");
                for swc in servers {
                    println!("  [{}] {}", swc.server.id, swc.server.name);
                    for ch in &swc.channels {
                        println!("    [{}] {} ({})", ch.id, ch.name, ch.channel_type.as_str());
                    }
                }
            }
            MessageType::VoiceError { message } => {
                println!("[VOICE ERROR] {}", message);
            }
            MessageType::Register { .. } | MessageType::Login { .. } => {
                println!("[Debug] Received unexpected auth message");
            }
            MessageType::UserListUpdate { users } => {
                println!("Online users:");
                for user in users {
                    let status = if user.is_online { "●" } else { "○" };
                    println!("  {} {}", status, user.username);
                }
            }
            MessageType::RequestUserList => {}
            MessageType::ListServers => {}
            _ => {
                println!("[Debug] Received other message type");
            }
        }
    }

    fn handle_user_input(
        &mut self,
        stream: &mut TcpStream,
        client_clone: Arc<Mutex<ChatClient>>,
    ) -> io::Result<()> {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            let input = line?;

            if input == "/quit" {
                break;
            }

            let is_authenticated = {
                let client_lock = client_clone.lock().unwrap();
                client_lock.authenticated
            };

            if let Some(message) = self.parse_command(&input, is_authenticated) {
                let json = message.to_json_file().unwrap();
                writeln!(stream, "{}", json)?;
                stream.flush()?;
            }
        }

        Ok(())
    }

    fn parse_command(&self, input: &str, is_authenticated: bool) -> Option<Message> {
        let parts: Vec<&str> = input.split_whitespace().collect();

        match parts.get(0)? {
            &"/register" => {
                if is_authenticated {
                    println!("You are already logged in!");
                    return None;
                }
                if parts.len() < 3 {
                    println!("Usage: /register <username> <password>");
                    return None;
                }
                let username = parts[1].to_string();
                let password = parts[2].to_string();
                Some(Message::new(
                    0,
                    MessageType::Register { username, password },
                ))
            }
            &"/login" => {
                if is_authenticated {
                    println!("You are already logged in!");
                    return None;
                }
                if parts.len() < 3 {
                    println!("Usage: /login <username> <password>");
                    return None;
                }
                let username = parts[1].to_string();
                let password = parts[2].to_string();
                Some(Message::new(0, MessageType::Login { username, password }))
            }
            &"/join" => {
                if !is_authenticated {
                    println!("Please login or register first!");
                    return None;
                }
                if parts.len() < 2 {
                    println!("Usage: /join <server_id>");
                    return None;
                }
                let server_id: u16 = parts.get(1)?.parse().ok()?;
                Some(Message::new(
                    self.user_id,
                    MessageType::JoinServer { server_id },
                ))
            }
            &"/leave" => {
                if !is_authenticated {
                    println!("Please login or register first!");
                    return None;
                }
                if parts.len() < 2 {
                    println!("Usage: /leave <server_id>");
                    return None;
                }
                let server_id: u16 = parts.get(1)?.parse().ok()?;
                Some(Message::new(self.user_id, MessageType::LeaveServer { server_id }))
            }
            &"/create" => {
                if !is_authenticated {
                    println!("Please login or register first!");
                    return None;
                }

                let args = Self::parse_quoted_args(input)?;
                if args.len() < 3 {
                    println!("Usage: /create <name> <description>");
                    println!(
                        "Example: /create \"My Server\" \"A cool server\""
                    );
                    return None;
                }

                let name = args.get(1)?.clone();
                let description = args.get(2)?.clone();
                Some(Message::new(
                    self.user_id,
                    MessageType::CreateServer {
                        name,
                        description,
                    },
                ))
            }
            &"/msg" => {
                if !is_authenticated {
                    println!("Please login or register first!");
                    return None;
                }
                if parts.len() < 3 {
                    println!("Usage: /msg <channel_id> <message>");
                    return None;
                }
                let channel_id: u16 = parts.get(1)?.parse().ok()?;
                let content = parts[2..].join(" ");
                Some(Message::new(
                    self.user_id,
                    MessageType::ChannelMessage {
                        channel_id,
                        sender_username: String::new(),
                        content,
                    },
                ))
            }
            &"/private" => {
                if !is_authenticated {
                    println!("Please login or register first!");
                    return None;
                }
                if parts.len() < 3 {
                    println!("Usage: /private <user_id> <message>");
                    return None;
                }
                let to_user_id: u16 = parts.get(1)?.parse().ok()?;
                let content = parts[2..].join(" ");
                Some(Message::new(
                    self.user_id,
                    MessageType::PrivateMessage {
                        to_user_id,
                        sender_username: String::new(),
                        content,
                    },
                ))
            }
            &"/voice" => {
                if !is_authenticated {
                    println!("Please login first!");
                    return None;
                }
                if parts.len() < 2 {
                    println!("Usage: /voice <channel_id>");
                    return None;
                }
                let channel_id: u16 = parts.get(1)?.parse().ok()?;
                println!("[Command] Requesting voice connection to channel {}...", channel_id);
                Some(Message::new(self.user_id, MessageType::RequestVoice { channel_id }))
            }
            _ => {
                println!("Unknown command: {}", input);
                if !is_authenticated {
                    println!("Available commands: /register, /login, /quit");
                } else {
                    println!(
                        "Available commands: /join, /leave, /create, /msg, /private, /voice, /quit"
                    );
                }
                None
            }
        }
    }

    fn parse_quoted_args(input: &str) -> Option<Vec<String>> {
        let mut args = Vec::new();
        let mut current_arg = String::new();
        let mut in_quotes = false;
        let mut chars = input.chars().peekable();

        while let Some(ch) = chars.next() {
            match ch {
                '"' => {
                    in_quotes = !in_quotes;
                }
                ' ' if !in_quotes => {
                    if !current_arg.is_empty() {
                        args.push(current_arg.trim().to_string());
                        current_arg.clear();
                    }
                }
                _ => {
                    current_arg.push(ch);
                }
            }
        }

        if !current_arg.is_empty() {
            args.push(current_arg.trim().to_string());
        }

        if args.is_empty() { None } else { Some(args) }
    }

    fn show_auth_commands() {
        println!("Welcome to FCA Chat!");
        println!("Authentication Commands:");
        println!("  /register <username> <password> - Create new account");
        println!("  /login <username> <password> - Login to existing account");
        println!("  /quit - Disconnect");
        println!();
    }

    fn show_chat_commands() {
        println!("\n=== Successfully Authenticated! ===");
        println!("Chat Commands:");
        println!("  /join <room_id> [password] - Join a room (use password if room is private)");
        println!("  /leave <room_id> - Leave a room");
        println!("  /create <name> <description> [password] - Create a new room");
        println!("  /msg <room_id> <message> - Send message to room");
        println!("  /private <user_id> <message> - Send private message");
        println!("  /voice - Start voice chat (Experimental)");
        println!("  /quit - Disconnect");
        println!();
        println!("   Tips:");
        println!(
            "   • Use quotes for multi-word names: /create \"My Room\" \"A cool place to chat\""
        );
        println!(
            "   • Add a password to make your room private: /create \"Secret\" \"Private room\" mypassword"
        );
        println!();
    }

    pub fn start_client(server_address: &str, username: &str) -> io::Result<()> {
        let mut client = ChatClient::new(server_address.to_string(), username.to_string());
        client.connect()
    }
}

/// Helper function to set up Ctrl+C signal handler for graceful shutdown
fn ctrlc_handler(running: Arc<AtomicBool>) {
    // Note: In a full implementation, you might use the `ctrlc` crate
    // For now, this is a placeholder that works with the voice loop timeout
    std::thread::spawn(move || {
        // This thread will set running to false when we want to stop
        // The actual signal handling would be done by a signal handler library
        loop {
            std::thread::sleep(Duration::from_secs(1));
            // Check if we should exit (this would be triggered by actual signal)
            if !running.load(Ordering::Relaxed) {
                break;
            }
        }
    });
}
