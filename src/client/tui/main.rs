use crate::protocol::message::{Message, MessageType, RoomInfo, UserStatus};
use crossterm::{
	event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
	execute,
	terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
	backend::CrosstermBackend,
	layout::{Alignment, Constraint, Direction, Layout, Rect},
	style::{Color, Modifier, Style},
	text::{Line, Span},
	widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs, Wrap},
	Frame, Terminal,
};
use std::{
	collections::HashMap,
	io::{self, BufRead, BufReader, Write},
	net::TcpStream,
	thread,
	time::{Duration, Instant},
};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum UIUpdate {
	AuthSuccess { user_id: u16, username: String },
	AuthFailure { reason: String },
	RoomCreated { room_id: u16, name: String, description: String },
	RoomJoined { room_id: u16, name: String, description: String },
	RoomMessage { room_id: u16, sender: String, content: String },
	ServerMessage { content: String, is_error: bool },
	UserListUpdate { users: Vec<UserStatus> },
	RoomList { rooms: Vec<RoomInfo> },
	PrivateMessage { from_user_id: u16, sender: String, content: String },
	VoiceChannelJoined { room_id: u16, ssrc: u32, udp_port: u16 },
	VoiceChannelLeft { room_id: u16 },
	VoiceUserJoined { user_id: u16, username: String, room_id: u16, ssrc: u32 },
	VoiceUserLeft { user_id: u16, room_id: u16 },
	VoiceUserSpeaking { user_id: u16, room_id: u16, ssrc: u32, speaking: bool },
}

#[derive(Debug, Clone)]
pub enum ChatMessageType {
	User,
	System,
	Private,
	Error,
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
	pub timestamp: String,
	pub sender: String,
	pub content: String,
	pub message_type: ChatMessageType,
}

#[derive(Debug, Clone)]
pub struct Room {
	pub id: u16,
	pub name: String,
	pub description: String,
	pub user_count: u16,
	pub messages: Vec<ChatMessage>,
}

#[derive(Debug)]
pub struct TuiChatClient {
	server_address: String,
	authenticated: bool,
	user_id: u16,
	username: String,
	input: String,
	current_room: Option<u16>,
	rooms: HashMap<u16, Room>,
	joined_rooms: Vec<u16>,
	show_help: bool,
	show_room_browser: bool,
	show_settings: bool,
	show_private_messages: bool,
	cursor_position: usize,
	available_rooms: Vec<RoomInfo>,
	selected_room_index: usize,
	private_conversations: HashMap<u16, Vec<ChatMessage>>,
	selected_private_user: Option<u16>,
	selected_user_index: usize,
	auth_mode: AuthMode,
	auth_username: String,
	auth_password: String,
	auth_field: AuthField,
	auth_error: Option<String>,
	message_sender: Option<mpsc::UnboundedSender<Message>>,
	ui_receiver: Option<mpsc::UnboundedReceiver<UIUpdate>>,
	online_users: Vec<UserStatus>,
	cursor_visible: bool,
	last_cursor_toggle: Instant,
}

#[derive(Debug, Clone, PartialEq)]
enum AuthMode {
	Login,
	Register,
}

#[derive(Debug, Clone, PartialEq)]
enum AuthField {
	Username,
	Password,
}

impl TuiChatClient {
	pub fn new(server_address: String) -> Self {
		Self {
			server_address,
			authenticated: false,
			user_id: 0,
			username: String::new(),
			input: String::new(),
			current_room: None,
			rooms: HashMap::new(),
			joined_rooms: Vec::new(),
			show_help: false,
			show_room_browser: false,
			show_settings: false,
			show_private_messages: false,
			cursor_position: 0,
			auth_mode: AuthMode::Login,
			auth_username: String::new(),
			auth_password: String::new(),
			auth_field: AuthField::Username,
			auth_error: None,
			message_sender: None,
			ui_receiver: None,
			online_users: Vec::new(),
			cursor_visible: true,
			last_cursor_toggle: Instant::now(),
			available_rooms: Vec::new(),
			selected_room_index: 0,
			private_conversations: HashMap::new(),
			selected_private_user: None,
			selected_user_index: 0,
		}
	}

	pub async fn start_tui(server_address: &str) -> io::Result<()> {
		enable_raw_mode()?;
		let mut stdout = io::stdout();
		execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
		let backend = CrosstermBackend::new(stdout);
		let mut terminal = Terminal::new(backend)?;

		let mut app = TuiChatClient::new(server_address.to_string());
		let result = app.run(&mut terminal).await;

		disable_raw_mode()?;
		execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
		terminal.show_cursor()?;
		result
	}

	async fn run(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
		self.connect().await?;
		let mut last_tick = Instant::now();
		let tick_rate = Duration::from_millis(50);
	loop {
			if let Some(ui_receiver) = &mut self.ui_receiver {
				if let Ok(ui_update) = ui_receiver.try_recv() {
					match ui_update {
						UIUpdate::AuthSuccess { user_id, username } => {
							self.authenticated = true;
							self.user_id = user_id;
							self.username = username;
							self.auth_error = None;
							self.auth_password.clear();
						}
						UIUpdate::AuthFailure { reason } => self.auth_error = Some(reason),
						UIUpdate::UserListUpdate { users } => self.online_users = users,
						UIUpdate::RoomList { rooms } => {
							self.available_rooms = rooms;
							self.selected_room_index = 0;
						}
						UIUpdate::RoomJoined { room_id, name, description } => {
							if !self.joined_rooms.contains(&room_id) { self.joined_rooms.push(room_id); }
							if self.current_room.is_none() { self.current_room = Some(room_id); }
							let room = Room { id: room_id, name, description, user_count: 1, messages: Vec::new() };
							self.rooms.insert(room_id, room);
						}
						UIUpdate::RoomMessage { room_id, sender, content } => {
							if let Some(room) = self.rooms.get_mut(&room_id) {
								room.messages.push(ChatMessage { timestamp: Self::current_time(), sender, content, message_type: ChatMessageType::User });
							}
						}
						UIUpdate::PrivateMessage { from_user_id, sender, content } => {
							let message = ChatMessage { timestamp: Self::current_time(), sender, content, message_type: ChatMessageType::Private };
							self.private_conversations.entry(from_user_id).or_insert_with(Vec::new).push(message);
						}
						UIUpdate::VoiceChannelJoined { room_id, ssrc, udp_port } => {
							if let Some(room) = self.rooms.get_mut(&room_id) {
								room.messages.push(ChatMessage { timestamp: Self::current_time(), sender: "System".to_string(), content: format!("Joined voice channel (SSRC: {}, UDP: {})", ssrc, udp_port), message_type: ChatMessageType::System });
							}
						}
						UIUpdate::VoiceChannelLeft { room_id } => {
							if let Some(room) = self.rooms.get_mut(&room_id) {
								room.messages.push(ChatMessage { timestamp: Self::current_time(), sender: "System".to_string(), content: "Left voice channel".to_string(), message_type: ChatMessageType::System });
							}
						}
						UIUpdate::VoiceUserJoined { username, room_id, ssrc, .. } => {
							if let Some(room) = self.rooms.get_mut(&room_id) {
								room.messages.push(ChatMessage { timestamp: Self::current_time(), sender: "System".to_string(), content: format!("{} joined voice channel (SSRC: {})", username, ssrc), message_type: ChatMessageType::System });
							}
						}
						UIUpdate::VoiceUserLeft { room_id, .. } => {
							if let Some(room) = self.rooms.get_mut(&room_id) {
								room.messages.push(ChatMessage { timestamp: Self::current_time(), sender: "System".to_string(), content: "User left voice channel".to_string(), message_type: ChatMessageType::System });
							}
						}
						UIUpdate::VoiceUserSpeaking { room_id, ssrc, speaking, .. } => {
							if let Some(room) = self.rooms.get_mut(&room_id) {
								let status = if speaking { "speaking" } else { "stopped speaking" };
								room.messages.push(ChatMessage { timestamp: Self::current_time(), sender: "System".to_string(), content: format!("Voice: User {} (SSRC: {})", status, ssrc), message_type: ChatMessageType::System });
							}
						}
						_ => {}
					}
				}
			}

			terminal.draw(|f| self.draw(f))?;

			if last_tick.elapsed() >= Duration::from_millis(500) {
				self.cursor_visible = !self.cursor_visible;
				self.last_cursor_toggle = Instant::now();
			}

			let timeout = tick_rate.checked_sub(last_tick.elapsed()).unwrap_or_else(|| Duration::from_secs(0));
			if crossterm::event::poll(timeout)? {
				if let Event::Key(key) = event::read()? {
					if key.kind == KeyEventKind::Press {
						if self.handle_key_event(key.code).await? { break; }
					}
				}
			}

			if last_tick.elapsed() >= tick_rate { last_tick = Instant::now(); }
	}

	Ok(())
	}

	async fn connect(&mut self) -> io::Result<()> {
		let stream = TcpStream::connect(&self.server_address)?;
		let read_stream = stream.try_clone()?;
		let write_stream = stream.try_clone()?;

		let (tx, mut rx) = mpsc::unbounded_channel();
		self.message_sender = Some(tx);
		let (ui_tx, ui_rx) = mpsc::unbounded_channel();
		self.ui_receiver = Some(ui_rx);
		let username = self.auth_username.clone();

		thread::spawn(move || { Self::message_receiver(read_stream, ui_tx, username); });
		tokio::spawn(async move {
			let mut stream = write_stream;
			while let Some(message) = rx.recv().await {
				if let Ok(json) = message.to_json_file() {
					let _ = writeln!(stream, "{}", json);
					let _ = stream.flush();
				}
			}
		});

		if let Some(sender) = &self.message_sender {
			let request = Message::new(0, MessageType::RequestUserList);
			let _ = sender.send(request);
		}
		Ok(())
	}

	fn message_receiver(stream: TcpStream, ui_sender: mpsc::UnboundedSender<UIUpdate>, username: String) {
		let reader = BufReader::new(stream);
		for line in reader.lines() {
			match line {
				Ok(json_message) => {
					if let Ok(message) = Message::from_json_file(&json_message) {
						match &message.message_type {
							MessageType::AuthSuccess { user_id, .. } => { let _ = ui_sender.send(UIUpdate::AuthSuccess { user_id: *user_id, username: username.clone() }); }
							MessageType::AuthFailure { reason } => { let _ = ui_sender.send(UIUpdate::AuthFailure { reason: reason.clone() }); }
							MessageType::RoomCreated { room_id: _, name, description: _ } => { let _ = ui_sender.send(UIUpdate::ServerMessage { content: format!("Room '{}' created successfully!", name), is_error: false }); }
							MessageType::RoomJoined { room_id, name, description } => { let _ = ui_sender.send(UIUpdate::RoomJoined { room_id: *room_id, name: name.clone(), description: description.clone() }); }
							MessageType::RoomMessage { room_id, sender_username, content } => { let _ = ui_sender.send(UIUpdate::RoomMessage { room_id: *room_id, sender: sender_username.clone(), content: content.clone() }); }
							MessageType::UserListUpdate { users } => { let _ = ui_sender.send(UIUpdate::UserListUpdate { users: users.clone() }); }
							MessageType::RoomList { rooms } => { let _ = ui_sender.send(UIUpdate::RoomList { rooms: rooms.clone() }); }
							MessageType::PrivateMessage { to_user_id: _, sender_username, content } => { let _ = ui_sender.send(UIUpdate::PrivateMessage { from_user_id: message.sender_id, sender: sender_username.clone(), content: content.clone() }); }
							MessageType::VoiceChannelJoined { room_id, ssrc, udp_port } => { let _ = ui_sender.send(UIUpdate::VoiceChannelJoined { room_id: *room_id, ssrc: *ssrc, udp_port: *udp_port }); }
							MessageType::VoiceChannelLeft { room_id } => { let _ = ui_sender.send(UIUpdate::VoiceChannelLeft { room_id: *room_id }); }
							MessageType::VoiceUserJoined { user_id, username, room_id, ssrc } => { let _ = ui_sender.send(UIUpdate::VoiceUserJoined { user_id: *user_id, username: username.clone(), room_id: *room_id, ssrc: *ssrc }); }
							MessageType::VoiceUserLeft { user_id, room_id } => { let _ = ui_sender.send(UIUpdate::VoiceUserLeft { user_id: *user_id, room_id: *room_id }); }
							MessageType::VoiceUserSpeaking { user_id, room_id, ssrc, speaking } => { let _ = ui_sender.send(UIUpdate::VoiceUserSpeaking { user_id: *user_id, room_id: *room_id, ssrc: *ssrc, speaking: *speaking }); }
							_ => {}
						}
					}
				}
				Err(_) => break,
			}
		}
	}

	fn current_time() -> String {
		use std::time::{SystemTime, UNIX_EPOCH};
		let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
		let secs = now.as_secs();
		let hours = (secs / 3600) % 24;
		let minutes = (secs / 60) % 60;
		format!("{:02}:{:02}", hours, minutes)
	}

	async fn handle_key_event(&mut self, key: KeyCode) -> io::Result<bool> {
		if !self.authenticated { return Ok(self.handle_auth_key_event(key).await?); }
		match key {
			KeyCode::Esc => {
				if self.show_help || self.show_room_browser || self.show_settings || self.show_private_messages {
					self.show_help = false; self.show_room_browser = false; self.show_settings = false; self.show_private_messages = false;
				} else { return Ok(true); }
			}
			KeyCode::F(1) => self.show_help = !self.show_help,
			KeyCode::F(2) => { self.show_room_browser = !self.show_room_browser; if self.show_room_browser { if let Some(sender) = &self.message_sender { let message = Message::new(self.user_id, MessageType::ListRooms); let _ = sender.send(message); } } }
			KeyCode::F(3) => { self.show_private_messages = !self.show_private_messages; if self.show_private_messages { self.selected_user_index = 0; self.update_selected_private_user(); } }
			KeyCode::F(4) => self.show_settings = !self.show_settings,
			KeyCode::Enter => {
				if self.show_room_browser {
					if !self.available_rooms.is_empty() && self.selected_room_index < self.available_rooms.len() {
						let room = &self.available_rooms[self.selected_room_index];
						if let Some(sender) = &self.message_sender { let message = Message::new(self.user_id, MessageType::Join { room_id: room.id, password: None }); let _ = sender.send(message); }
						self.show_room_browser = false;
					}
				} else if self.show_private_messages { self.update_selected_private_user(); }
				else if !self.input.is_empty() {
					if self.show_private_messages && self.selected_private_user.is_some() {
						if let Some(sender) = &self.message_sender { let message = Message::new(self.user_id, MessageType::PrivateMessage { to_user_id: self.selected_private_user.unwrap(), sender_username: self.username.clone(), content: self.input.clone() }); let _ = sender.send(message); }
					} else { self.send_message().await?; }
					self.input.clear(); self.cursor_position = 0;
				}
			}
			KeyCode::Backspace => { if self.cursor_position > 0 { self.input.remove(self.cursor_position - 1); self.cursor_position -= 1; } }
			KeyCode::Left => { if self.show_room_browser { if self.selected_room_index > 0 { self.selected_room_index -= 1; } } else if self.cursor_position > 0 { self.cursor_position -= 1; } }
			KeyCode::Right => { if self.show_room_browser { if self.selected_room_index < self.available_rooms.len().saturating_sub(1) { self.selected_room_index += 1; } } else if self.cursor_position < self.input.len() { self.cursor_position += 1; } }
			KeyCode::Up => { if self.show_room_browser { if self.selected_room_index > 0 { self.selected_room_index -= 1; } } else if self.show_private_messages { if self.selected_user_index > 0 { self.selected_user_index -= 1; self.update_selected_private_user(); } } }
			KeyCode::Down => { if self.show_room_browser { if self.selected_room_index < self.available_rooms.len().saturating_sub(1) { self.selected_room_index += 1; } } else if self.show_private_messages { let user_count = self.online_users.iter().filter(|user| user.user_id != self.user_id).count(); if self.selected_user_index < user_count.saturating_sub(1) { self.selected_user_index += 1; self.update_selected_private_user(); } } }
			KeyCode::Tab => { self.switch_room(); }
			KeyCode::Char(c) => { self.input.insert(self.cursor_position, c); self.cursor_position += 1; }
			_ => {}
		}
		Ok(false)
	}

	async fn handle_auth_key_event(&mut self, key: KeyCode) -> io::Result<bool> {
		match key {
			KeyCode::Esc => return Ok(true),
			KeyCode::Tab => { self.auth_field = match self.auth_field { AuthField::Username => AuthField::Password, AuthField::Password => AuthField::Username }; }
			KeyCode::F(2) => { self.auth_mode = match self.auth_mode { AuthMode::Login => AuthMode::Register, AuthMode::Register => AuthMode::Login }; self.auth_error = None; }
			KeyCode::Enter => { if !self.auth_username.is_empty() && !self.auth_password.is_empty() { self.send_auth_message().await?; } }
			KeyCode::Backspace => { match self.auth_field { AuthField::Username => { self.auth_username.pop(); } AuthField::Password => { self.auth_password.pop(); } } }
			KeyCode::Char(c) => { match self.auth_field { AuthField::Username => { self.auth_username.push(c); } AuthField::Password => { self.auth_password.push(c); } } }
			_ => {}
		}
		Ok(false)
	}

	async fn send_auth_message(&mut self) -> io::Result<()> {
		if let Some(sender) = &self.message_sender {
			let message = match self.auth_mode {
				AuthMode::Login => Message::new(0, MessageType::Login { username: self.auth_username.clone(), password: self.auth_password.clone() }),
				AuthMode::Register => Message::new(0, MessageType::Register { username: self.auth_username.clone(), password: self.auth_password.clone() }),
			};
			let _ = sender.send(message);
		}
		Ok(())
	}

	async fn send_message(&mut self) -> io::Result<()> {
		if let Some(sender) = &self.message_sender {
			if self.input.starts_with('/') { self.handle_command().await?; }
			else if let Some(room_id) = self.current_room {
				let message = Message::new(self.user_id, MessageType::RoomMessage { room_id, sender_username: self.username.clone(), content: self.input.clone() });
				let _ = sender.send(message);
			}
		}
		Ok(())
	}

	async fn handle_command(&mut self) -> io::Result<()> {
		let parts: Vec<&str> = self.input.split_whitespace().collect();
		if let Some(sender) = &self.message_sender {
			match parts.get(0) {
				Some(&"/join") => { if let Some(room_id_str) = parts.get(1) { if let Ok(room_id) = room_id_str.parse::<u16>() { let password = if parts.len() > 2 { Some(parts[2].to_string()) } else { None }; let message = Message::new(self.user_id, MessageType::Join { room_id, password }); let _ = sender.send(message); } } }
				Some(&"/create") => { if let Some(args) = Self::parse_quoted_args(&self.input) { if args.len() >= 4 { if let Ok(room_id) = args[1].parse::<u16>() { let name = args[2].clone(); let description = args[3].clone(); let password = if args.len() > 4 { Some(args[4].clone()) } else { None }; let message = Message::new(self.user_id, MessageType::CreateRoom { room_id, name, description, password }); let _ = sender.send(message); } } } }
				Some(&"/leave") => { if let Some(room_id_str) = parts.get(1) { if let Ok(room_id) = room_id_str.parse::<u16>() { let message = Message::new(self.user_id, MessageType::Leave { room_id }); let _ = sender.send(message); self.joined_rooms.retain(|&id| id != room_id); if self.current_room == Some(room_id) { self.current_room = self.joined_rooms.first().copied(); } } } }
				_ => { println!("Unknown command. Use /join, /create, /leave"); }
			}
		}
		Ok(())
	}

	fn parse_quoted_args(input: &str) -> Option<Vec<String>> {
		let mut args = Vec::new();
		let mut current_arg = String::new();
		let mut in_quotes = false;
		let mut chars = input.chars().peekable();
		while let Some(ch) = chars.next() {
			match ch { '"' => { in_quotes = !in_quotes; } ' ' if !in_quotes => { if !current_arg.is_empty() { args.push(current_arg.trim().to_string()); current_arg.clear(); } } _ => { current_arg.push(ch); } }
		}
		if !current_arg.is_empty() { args.push(current_arg.trim().to_string()); }
		if args.is_empty() { None } else { Some(args) }
	}

	fn update_selected_private_user(&mut self) {
		let other_users: Vec<&UserStatus> = self.online_users.iter().filter(|user| user.user_id != self.user_id).collect();
		if !other_users.is_empty() && self.selected_user_index < other_users.len() { self.selected_private_user = Some(other_users[self.selected_user_index].user_id); } else { self.selected_private_user = None; }
	}

	fn switch_room(&mut self) {
		if let Some(current) = self.current_room {
			if let Some(current_index) = self.joined_rooms.iter().position(|&id| id == current) {
				let next_index = (current_index + 1) % self.joined_rooms.len();
				self.current_room = self.joined_rooms.get(next_index).copied();
			}
		}
	}

	fn draw(&self, f: &mut Frame) { if !self.authenticated { self.draw_auth_screen(f); } else { self.draw_main_screen(f); } }

	fn draw_auth_screen(&self, f: &mut Frame) {
		let size = f.size();
		let popup_area = Self::centered_rect(60, 40, size);
		f.render_widget(Clear, popup_area);
		let block = Block::default().title(format!("RustyRoom - {}", if self.auth_mode == AuthMode::Login { "Login" } else { "Register" })).borders(Borders::ALL).style(Style::default().fg(Color::Cyan));
		f.render_widget(block, popup_area);
	let inner = popup_area.inner(&ratatui::layout::Margin { horizontal: 2, vertical: 2 });
		let chunks = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(3), Constraint::Length(3), Constraint::Length(3), Constraint::Min(1)]).split(inner);
		let username_style = if self.auth_field == AuthField::Username { Style::default().fg(Color::Yellow) } else { Style::default() };
		let username = Paragraph::new(self.auth_username.as_str()).block(Block::default().title("Username").borders(Borders::ALL).style(username_style));
		f.render_widget(username, chunks[0]);
		let password_style = if self.auth_field == AuthField::Password { Style::default().fg(Color::Yellow) } else { Style::default() };
		let password_display = "*".repeat(self.auth_password.len());
		let password = Paragraph::new(password_display.as_str()).block(Block::default().title("Password").borders(Borders::ALL).style(password_style));
		f.render_widget(password, chunks[1]);
		let instructions = vec![Line::from(vec![Span::raw("Tab: Switch fields | F2: Toggle "), Span::styled(if self.auth_mode == AuthMode::Login { "Register" } else { "Login" }, Style::default().fg(Color::Green))]), Line::from("Enter: Submit | Esc: Quit")];
		let help = Paragraph::new(instructions).block(Block::default().title("Controls").borders(Borders::ALL));
		f.render_widget(help, chunks[2]);
		if let Some(error) = &self.auth_error { let error_msg = Paragraph::new(error.as_str()).style(Style::default().fg(Color::Red)).wrap(Wrap { trim: true }); f.render_widget(error_msg, chunks[3]); }
	}

	fn draw_main_screen(&self, f: &mut Frame) {
		let size = f.size();
		let main_chunks = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Percentage(75), Constraint::Percentage(25)]).split(size);
		let left_chunks = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(3), Constraint::Length(3), Constraint::Min(1), Constraint::Length(3), Constraint::Length(1)]).split(main_chunks[0]);
		let header = Paragraph::new(format!("RustyRoom - Connected as: {}", self.username)).style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)).block(Block::default().borders(Borders::ALL));
		f.render_widget(header, left_chunks[0]);
		let room_titles: Vec<String> = self.joined_rooms.iter().filter_map(|&id| self.rooms.get(&id)).map(|room| format!("{}({})", room.name, room.user_count)).collect();
		let selected_tab = self.current_room.and_then(|current| self.joined_rooms.iter().position(|&id| id == current)).unwrap_or(0);
		let tabs = Tabs::new(room_titles).block(Block::default().borders(Borders::ALL).title("Rooms")).style(Style::default().fg(Color::White)).highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)).select(selected_tab);
		f.render_widget(tabs, left_chunks[1]);
		let chat_area = left_chunks[2];
		self.draw_chat_area(f, chat_area);
		let input_text = if self.cursor_visible { let mut text = self.input.clone(); text.insert(self.cursor_position, '│'); text } else { self.input.clone() };
		let input = Paragraph::new(input_text.as_str()).style(Style::default().fg(Color::White)).block(Block::default().borders(Borders::ALL).title("Message"));
		f.render_widget(input, left_chunks[3]);
		let status = Paragraph::new("Commands: F1-Help F2-Rooms F3-Private F4-Settings ESC-Quit").style(Style::default().fg(Color::Gray));
		f.render_widget(status, left_chunks[4]);
		self.draw_user_panel(f, main_chunks[1]);
		if self.show_help { self.draw_help_popup(f); }
		if self.show_room_browser { self.draw_room_browser(f); }
		if self.show_settings { self.draw_settings(f); }
		if self.show_private_messages { self.draw_private_messages(f); }
	}

	fn draw_user_panel(&self, f: &mut Frame, area: Rect) {
		let block = Block::default().title("Online Users").borders(Borders::ALL).style(Style::default().fg(Color::Green));
		if self.online_users.is_empty() {
			let empty_text = Paragraph::new("No users online\n\nPress F3 for\nprivate messages").style(Style::default().fg(Color::Gray)).alignment(Alignment::Center).block(block);
			f.render_widget(empty_text, area);
		} else {
			let user_items: Vec<ListItem> = self.online_users.iter().map(|user| {
				let status_icon = if user.is_online { "●" } else { "○" };
				let style = if user.is_online { Style::default().fg(Color::Green) } else { Style::default().fg(Color::Gray) };
				let display = format!("{} {}", status_icon, user.username);
				ListItem::new(display).style(style)
			}).collect();
			let user_list = List::new(user_items).block(block);
			f.render_widget(user_list, area);
		}
	}

	fn draw_chat_area(&self, f: &mut Frame, area: Rect) {
		if let Some(room_id) = self.current_room {
			if let Some(room) = self.rooms.get(&room_id) {
				let title = format!("Room: {}", room.name);
				let block = Block::default().borders(Borders::ALL).title(title);
				let messages: Vec<ListItem> = room.messages.iter().map(|msg| {
					let style = match msg.message_type { ChatMessageType::User => Style::default().fg(Color::White), ChatMessageType::System => Style::default().fg(Color::Green), ChatMessageType::Private => Style::default().fg(Color::Magenta), ChatMessageType::Error => Style::default().fg(Color::Red) };
					let content = format!("[{}] {}: {}", msg.timestamp, msg.sender, msg.content);
					ListItem::new(content).style(style)
				}).collect();
				let chat = List::new(messages).block(block);
				f.render_widget(chat, area);
			}
		} else {
			let block = Block::default().borders(Borders::ALL).title("No Room Selected");
			let empty = Paragraph::new("Join a room to start chatting!").style(Style::default().fg(Color::Gray)).alignment(Alignment::Center).block(block);
			f.render_widget(empty, area);
		}
	}

	fn draw_help_popup(&self, f: &mut Frame) {
		let popup_area = Self::centered_rect(80, 60, f.size());
		f.render_widget(Clear, popup_area);
		let help_text = vec![
			Line::from(vec![Span::styled("RustyRoom Help", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))]),
			Line::from(""),
			Line::from("Navigation:"),
			Line::from("  Tab       - Switch between rooms"),
			Line::from("  F1        - Toggle this help"),
			Line::from("  F2        - Room browser"),
			Line::from("  F3        - Private messages"),
			Line::from("  F4        - Settings"),
			Line::from("  Esc       - Close dialogs or quit"),
			Line::from(""),
			Line::from("Chat Commands:"),
			Line::from("  /join <id> [password] - Join a room"),
			Line::from("  /create <id> <name> <desc> [pass] - Create room"),
			Line::from("  /leave <id> - Leave a room"),
			Line::from(""),
			Line::from("Input:"),
			Line::from("  Enter     - Send message"),
			Line::from("  Backspace - Delete character"),
			Line::from("  Arrow keys - Move cursor"),
		];
		let help = Paragraph::new(help_text).block(Block::default().title("Help").borders(Borders::ALL)).wrap(Wrap { trim: true });
		f.render_widget(help, popup_area);
	}

	fn draw_room_browser(&self, f: &mut Frame) {
		let popup_area = Self::centered_rect(70, 50, f.size());
		f.render_widget(Clear, popup_area);
		let block = Block::default().title("Room Browser").borders(Borders::ALL).style(Style::default().fg(Color::Cyan));
		if self.available_rooms.is_empty() {
			let content = Paragraph::new("Loading rooms...\n\nPress Esc to close").block(block).alignment(Alignment::Center).wrap(Wrap { trim: true });
			f.render_widget(content, popup_area);
		} else {
			let inner = popup_area.inner(&ratatui::layout::Margin { horizontal: 2, vertical: 2 });
			let chunks = Layout::default().direction(Direction::Vertical).constraints([Constraint::Min(1), Constraint::Length(3)]).split(inner);
			let room_items: Vec<ListItem> = self.available_rooms.iter().enumerate().map(|(i, room)| {
				let style = if i == self.selected_room_index { Style::default().bg(Color::Yellow).fg(Color::Black).add_modifier(Modifier::BOLD) } else { Style::default() };
				let lock_icon = if room.is_password_protected { "🔒 " } else { "" };
				let content = format!("  [{}] {}{} - {}", room.id, lock_icon, room.name, room.description);
				ListItem::new(content).style(style)
			}).collect();
			let room_list = List::new(room_items).block(Block::default().borders(Borders::ALL).title("Available Rooms"));
			f.render_widget(room_list, chunks[0]);
			let instructions = vec![Line::from("↑↓ Navigate | Enter: Join Room | Esc: Close"), Line::from("🔒 = Password Protected Room")];
			let help = Paragraph::new(instructions).block(Block::default().borders(Borders::ALL).title("Controls")).alignment(Alignment::Center);
			f.render_widget(help, chunks[1]);
			f.render_widget(block, popup_area);
		}
	}

	fn draw_private_messages(&self, f: &mut Frame) {
		let popup_area = Self::centered_rect(70, 60, f.size());
		f.render_widget(Clear, popup_area);
		let block = Block::default().title("Private Messages").borders(Borders::ALL).style(Style::default().fg(Color::Magenta));
	let inner = popup_area.inner(&ratatui::layout::Margin { horizontal: 2, vertical: 2 });
		let chunks = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Percentage(30), Constraint::Percentage(70)]).split(inner);
		let user_items: Vec<ListItem> = self.online_users.iter().filter(|user| user.user_id != self.user_id).map(|user| {
			let style = if Some(user.user_id) == self.selected_private_user { Style::default().bg(Color::Magenta).fg(Color::White).add_modifier(Modifier::BOLD) } else if user.is_online { Style::default().fg(Color::Green) } else { Style::default().fg(Color::Gray) };
			let status_icon = if user.is_online { "●" } else { "○" };
			let display = format!("{} {}", status_icon, user.username);
			ListItem::new(display).style(style)
		}).collect();
		let user_list = List::new(user_items).block(Block::default().borders(Borders::ALL).title("Users"));
		f.render_widget(user_list, chunks[0]);
		if let Some(selected_user) = self.selected_private_user { if let Some(messages) = self.private_conversations.get(&selected_user) { let message_items: Vec<ListItem> = messages.iter().map(|msg| { let content = format!("[{}] {}: {}", msg.timestamp, msg.sender, msg.content); ListItem::new(content).style(Style::default().fg(Color::White)) }).collect(); let message_list = List::new(message_items).block(Block::default().borders(Borders::ALL).title("Messages")); f.render_widget(message_list, chunks[1]); } else { let empty_msg = Paragraph::new("No messages yet.\nType a message to start conversation.").block(Block::default().borders(Borders::ALL).title("Messages")).alignment(Alignment::Center); f.render_widget(empty_msg, chunks[1]); } } else { let instructions = Paragraph::new("Select a user from the list\nto start a private conversation.\n\n↑↓ Navigate | Enter: Select\nEsc: Close").block(Block::default().borders(Borders::ALL).title("Instructions")).alignment(Alignment::Center); f.render_widget(instructions, chunks[1]); }
		f.render_widget(block, popup_area);
	}

	fn draw_settings(&self, f: &mut Frame) {
		let popup_area = Self::centered_rect(50, 30, f.size());
		f.render_widget(Clear, popup_area);
		let block = Block::default().title("Settings").borders(Borders::ALL);
		let content = Paragraph::new("Settings not yet implemented.").block(block);
		f.render_widget(content, popup_area);
	}

	fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
		let popup_layout = Layout::default().direction(Direction::Vertical).constraints([Constraint::Percentage((100 - percent_y) / 2), Constraint::Percentage(percent_y), Constraint::Percentage((100 - percent_y) / 2)]).split(r);
		Layout::default().direction(Direction::Horizontal).constraints([Constraint::Percentage((100 - percent_x) / 2), Constraint::Percentage(percent_x), Constraint::Percentage((100 - percent_x) / 2)]).split(popup_layout[1])[1]
	}
}

