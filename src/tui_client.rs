/*
 TUI Chat Client for FCA

 A terminal user interface for the FCA chat application using ratatui.
 Provides a modern, interactive chat experience with multiple rooms,
 real-time messaging, and intuitive navigation.
*/

use crate::resc::{Message, MessageType};
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
pub struct ChatMessage {
    pub timestamp: String,
    pub sender: String,
    pub content: String,
    pub message_type: ChatMessageType,
}

#[derive(Debug, Clone)]
pub enum ChatMessageType {
    User,
    System,
    Private,
    Error,
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
    // Connection state
    server_address: String,
    
    // Authentication
    authenticated: bool,
    user_id: u16,
    username: String,
    
    // UI state
    input: String,
    current_room: Option<u16>,
    rooms: HashMap<u16, Room>,
    joined_rooms: Vec<u16>,
    
    // UI control
    show_help: bool,
    show_room_browser: bool,
    show_settings: bool,
    cursor_position: usize,
    
    // Authentication screen state
    auth_mode: AuthMode,
    auth_username: String,
    auth_password: String,
    auth_field: AuthField,
    auth_error: Option<String>,
    
    // Channels for async communication
    message_sender: Option<mpsc::UnboundedSender<Message>>,
    auth_receiver: Option<mpsc::UnboundedReceiver<bool>>,
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
            cursor_position: 0,
            auth_mode: AuthMode::Login,
            auth_username: String::new(),
            auth_password: String::new(),
            auth_field: AuthField::Username,
            auth_error: None,
            message_sender: None,
            auth_receiver: None,
        }
    }

    pub async fn start_tui(server_address: &str) -> io::Result<()> {
        // Setup terminal
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let mut app = TuiChatClient::new(server_address.to_string());
        let result = app.run(&mut terminal).await;

        // Restore terminal
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        result
    }

    async fn run(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
        // Connect to server
        self.connect().await?;

        let mut last_tick = Instant::now();
        let tick_rate = Duration::from_millis(50);

        loop {
            // Check for authentication updates
            if let Some(auth_receiver) = &mut self.auth_receiver {
                if let Ok(auth_success) = auth_receiver.try_recv() {
                    if auth_success {
                        self.authenticated = true;
                        self.username = self.auth_username.clone();
                        self.auth_error = None;
                        // Clear password for security
                        self.auth_password.clear();
                    } else {
                        self.auth_error = Some("Authentication failed".to_string());
                    }
                }
            }

            terminal.draw(|f| self.draw(f))?;

            let timeout = tick_rate
                .checked_sub(last_tick.elapsed())
                .unwrap_or_else(|| Duration::from_secs(0));

            if crossterm::event::poll(timeout)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        if self.handle_key_event(key.code).await? {
                            break;
                        }
                    }
                }
            }

            if last_tick.elapsed() >= tick_rate {
                last_tick = Instant::now();
            }
        }

        Ok(())
    }

    async fn connect(&mut self) -> io::Result<()> {
        let stream = TcpStream::connect(&self.server_address)?;
        let read_stream = stream.try_clone()?;
        let write_stream = stream.try_clone()?;

        // Setup message channels
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.message_sender = Some(tx);

        // Setup authentication feedback channel
        let (auth_tx, auth_rx) = mpsc::unbounded_channel();
        self.auth_receiver = Some(auth_rx);

        // Start message receiver thread
        thread::spawn(move || {
            Self::message_receiver(read_stream, auth_tx);
        });

        // Start message sender task
        tokio::spawn(async move {
            let mut stream = write_stream;
            while let Some(message) = rx.recv().await {
                if let Ok(json) = message.to_json_file() {
                    let _ = writeln!(stream, "{}", json);
                    let _ = stream.flush();
                }
            }
        });

        Ok(())
    }

    fn message_receiver(stream: TcpStream, auth_sender: mpsc::UnboundedSender<bool>) {
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            match line {
                Ok(json_message) => {
                    if let Ok(message) = Message::from_json_file(&json_message) {
                        // Handle authentication responses
                        match &message.message_type {
                            MessageType::AuthSuccess { user_id: _, message: msg } => {
                                println!("Authentication successful! Message: {}", msg);
                                let _ = auth_sender.send(true);
                            }
                            MessageType::AuthFailure { reason } => {
                                println!("Authentication failed: {}", reason);
                                let _ = auth_sender.send(false);
                            }
                            MessageType::RoomCreated { room_id: _, name, description: _ } => {
                                println!("Room '{}' created successfully!", name);
                            }
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
        if !self.authenticated {
            return Ok(self.handle_auth_key_event(key).await?);
        }

        match key {
            KeyCode::Esc => {
                if self.show_help || self.show_room_browser || self.show_settings {
                    self.show_help = false;
                    self.show_room_browser = false;
                    self.show_settings = false;
                } else {
                    return Ok(true); // Exit
                }
            }
            KeyCode::F(1) => self.show_help = !self.show_help,
            KeyCode::F(2) => self.show_room_browser = !self.show_room_browser,
            KeyCode::F(3) => {
                // TODO: Implement private message interface
            }
            KeyCode::F(4) => self.show_settings = !self.show_settings,
            KeyCode::Enter => {
                if !self.input.is_empty() {
                    self.send_message().await?;
                    self.input.clear();
                    self.cursor_position = 0;
                }
            }
            KeyCode::Backspace => {
                if self.cursor_position > 0 {
                    self.input.remove(self.cursor_position - 1);
                    self.cursor_position -= 1;
                }
            }
            KeyCode::Left => {
                if self.cursor_position > 0 {
                    self.cursor_position -= 1;
                }
            }
            KeyCode::Right => {
                if self.cursor_position < self.input.len() {
                    self.cursor_position += 1;
                }
            }
            KeyCode::Tab => {
                self.switch_room();
            }
            KeyCode::Char(c) => {
                self.input.insert(self.cursor_position, c);
                self.cursor_position += 1;
            }
            _ => {}
        }
        Ok(false)
    }

    async fn handle_auth_key_event(&mut self, key: KeyCode) -> io::Result<bool> {
        match key {
            KeyCode::Esc => return Ok(true), // Exit
            KeyCode::Tab => {
                self.auth_field = match self.auth_field {
                    AuthField::Username => AuthField::Password,
                    AuthField::Password => AuthField::Username,
                };
            }
            KeyCode::F(2) => {
                self.auth_mode = match self.auth_mode {
                    AuthMode::Login => AuthMode::Register,
                    AuthMode::Register => AuthMode::Login,
                };
                self.auth_error = None;
            }
            KeyCode::Enter => {
                println!("Enter key pressed in auth mode"); // Debug
                if !self.auth_username.is_empty() && !self.auth_password.is_empty() {
                    println!("Attempting to send auth message"); // Debug
                    self.send_auth_message().await?;
                } else {
                    println!("Auth fields empty - username: '{}', password: '{}'", 
                            self.auth_username, "*".repeat(self.auth_password.len())); // Debug
                }
            }
            KeyCode::Backspace => {
                match self.auth_field {
                    AuthField::Username => {
                        self.auth_username.pop();
                    }
                    AuthField::Password => {
                        self.auth_password.pop();
                    }
                }
            }
            KeyCode::Char(c) => {
                match self.auth_field {
                    AuthField::Username => {
                        self.auth_username.push(c);
                    }
                    AuthField::Password => {
                        self.auth_password.push(c);
                    }
                }
            }
            _ => {}
        }
        Ok(false)
    }

    async fn send_auth_message(&mut self) -> io::Result<()> {
        if let Some(sender) = &self.message_sender {
            let message = match self.auth_mode {
                AuthMode::Login => Message::new(0, MessageType::Login {
                    username: self.auth_username.clone(),
                    password: self.auth_password.clone(),
                }),
                AuthMode::Register => Message::new(0, MessageType::Register {
                    username: self.auth_username.clone(),
                    password: self.auth_password.clone(),
                }),
            };
            let _ = sender.send(message);
        }
        Ok(())
    }

    async fn send_message(&mut self) -> io::Result<()> {
        if let Some(sender) = &self.message_sender {
            if self.input.starts_with('/') {
                self.handle_command().await?;
            } else if let Some(room_id) = self.current_room {
                let message = Message::new(self.user_id, MessageType::RoomMessage {
                    room_id,
                    sender_username: self.username.clone(),
                    content: self.input.clone(),
                });
                let _ = sender.send(message);
            }
        }
        Ok(())
    }

    async fn handle_command(&mut self) -> io::Result<()> {
        let parts: Vec<&str> = self.input.split_whitespace().collect();
        if let Some(sender) = &self.message_sender {
            match parts.get(0) {
                Some(&"/join") => {
                    if let Some(room_id_str) = parts.get(1) {
                        if let Ok(room_id) = room_id_str.parse::<u16>() {
                            let password = if parts.len() > 2 {
                                Some(parts[2].to_string())
                            } else {
                                None
                            };
                            let message = Message::new(self.user_id, MessageType::Join { room_id, password });
                            let _ = sender.send(message);
                        }
                    }
                }
                Some(&"/create") => {
                    if parts.len() >= 4 {
                        if let Ok(room_id) = parts[1].parse::<u16>() {
                            let name = parts[2].to_string();
                            let description = parts[3].to_string();
                            let password = if parts.len() > 4 {
                                Some(parts[4].to_string())
                            } else {
                                None
                            };
                            let message = Message::new(self.user_id, MessageType::CreateRoom {
                                room_id, name, description, password
                            });
                            let _ = sender.send(message);
                        }
                    }
                }
                Some(&"/leave") => {
                    if let Some(room_id_str) = parts.get(1) {
                        if let Ok(room_id) = room_id_str.parse::<u16>() {
                            let message = Message::new(self.user_id, MessageType::Leave { room_id });
                            let _ = sender.send(message);
                            self.joined_rooms.retain(|&id| id != room_id);
                            if self.current_room == Some(room_id) {
                                self.current_room = self.joined_rooms.first().copied();
                            }
                        }
                    }
                }
                _ => {
                    println!("Unknown command. Press F1 for help.");
                }
            }
        }
        Ok(())
    }

    fn switch_room(&mut self) {
        if let Some(current) = self.current_room {
            if let Some(current_index) = self.joined_rooms.iter().position(|&id| id == current) {
                let next_index = (current_index + 1) % self.joined_rooms.len();
                self.current_room = self.joined_rooms.get(next_index).copied();
            }
        }
    }

    fn draw(&self, f: &mut Frame) {
        if !self.authenticated {
            self.draw_auth_screen(f);
        } else {
            self.draw_main_screen(f);
        }
    }

    fn draw_auth_screen(&self, f: &mut Frame) {
        let size = f.area();
        
        // Center the auth dialog
        let popup_area = Self::centered_rect(60, 40, size);
        f.render_widget(Clear, popup_area);
        
        let block = Block::default()
            .title(format!("FCA - {}", if self.auth_mode == AuthMode::Login { "Login" } else { "Register" }))
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::Cyan));
        f.render_widget(block, popup_area);

        let inner = popup_area.inner(ratatui::layout::Margin { horizontal: 2, vertical: 2 });
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(1),
            ])
            .split(inner);

        // Username field
        let username_style = if self.auth_field == AuthField::Username {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        let username = Paragraph::new(self.auth_username.as_str())
            .block(Block::default().title("Username").borders(Borders::ALL).style(username_style));
        f.render_widget(username, chunks[0]);

        // Password field
        let password_style = if self.auth_field == AuthField::Password {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        let password_display = "*".repeat(self.auth_password.len());
        let password = Paragraph::new(password_display.as_str())
            .block(Block::default().title("Password").borders(Borders::ALL).style(password_style));
        f.render_widget(password, chunks[1]);

        // Instructions
        let instructions = vec![
            Line::from(vec![
                Span::raw("Tab: Switch fields | F2: Toggle "),
                Span::styled(
                    if self.auth_mode == AuthMode::Login { "Register" } else { "Login" },
                    Style::default().fg(Color::Green)
                ),
            ]),
            Line::from("Enter: Submit | Esc: Quit"),
        ];
        let help = Paragraph::new(instructions)
            .block(Block::default().title("Controls").borders(Borders::ALL));
        f.render_widget(help, chunks[2]);

        // Error message
        if let Some(error) = &self.auth_error {
            let error_msg = Paragraph::new(error.as_str())
                .style(Style::default().fg(Color::Red))
                .wrap(Wrap { trim: true });
            f.render_widget(error_msg, chunks[3]);
        }
    }

    fn draw_main_screen(&self, f: &mut Frame) {
        let size = f.area();
        
        // Main layout
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Header
                Constraint::Length(3), // Room tabs
                Constraint::Min(1),    // Chat area
                Constraint::Length(3), // Input
                Constraint::Length(1), // Status bar
            ])
            .split(size);

        // Header
        let header = Paragraph::new(format!("FCA - Connected as: {}", self.username))
            .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL));
        f.render_widget(header, chunks[0]);

        // Room tabs
        let room_titles: Vec<String> = self.joined_rooms.iter()
            .filter_map(|&id| self.rooms.get(&id))
            .map(|room| format!("{}({})", room.name, room.user_count))
            .collect();
        
        let selected_tab = self.current_room
            .and_then(|current| self.joined_rooms.iter().position(|&id| id == current))
            .unwrap_or(0);
        
        let tabs = Tabs::new(room_titles)
            .block(Block::default().borders(Borders::ALL).title("Rooms"))
            .style(Style::default().fg(Color::White))
            .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            .select(selected_tab);
        f.render_widget(tabs, chunks[1]);

        // Chat messages
        let chat_area = chunks[2];
        self.draw_chat_area(f, chat_area);

        // Input field
        let input = Paragraph::new(self.input.as_str())
            .style(Style::default().fg(Color::White))
            .block(Block::default().borders(Borders::ALL).title("Message"));
        f.render_widget(input, chunks[3]);

        // Status bar
        let status = Paragraph::new("Commands: F1-Help F2-Rooms F3-Private F4-Settings ESC-Quit")
            .style(Style::default().fg(Color::Gray));
        f.render_widget(status, chunks[4]);

        // Overlays
        if self.show_help {
            self.draw_help_popup(f);
        }
        if self.show_room_browser {
            self.draw_room_browser(f);
        }
        if self.show_settings {
            self.draw_settings(f);
        }
    }

    fn draw_chat_area(&self, f: &mut Frame, area: Rect) {
        if let Some(room_id) = self.current_room {
            if let Some(room) = self.rooms.get(&room_id) {
                let title = format!("Room: {}", room.name);
                let block = Block::default().borders(Borders::ALL).title(title);
                
                let messages: Vec<ListItem> = room.messages.iter()
                    .map(|msg| {
                        let style = match msg.message_type {
                            ChatMessageType::User => Style::default().fg(Color::White),
                            ChatMessageType::System => Style::default().fg(Color::Green),
                            ChatMessageType::Private => Style::default().fg(Color::Magenta),
                            ChatMessageType::Error => Style::default().fg(Color::Red),
                        };
                        
                        let content = format!("[{}] {}: {}", msg.timestamp, msg.sender, msg.content);
                        ListItem::new(content).style(style)
                    })
                    .collect();
                
                let chat = List::new(messages).block(block);
                f.render_widget(chat, area);
            }
        } else {
            let block = Block::default().borders(Borders::ALL).title("No Room Selected");
            let empty = Paragraph::new("Join a room to start chatting!")
                .style(Style::default().fg(Color::Gray))
                .alignment(Alignment::Center)
                .block(block);
            f.render_widget(empty, area);
        }
    }

    fn draw_help_popup(&self, f: &mut Frame) {
        let popup_area = Self::centered_rect(80, 60, f.area());
        f.render_widget(Clear, popup_area);
        
        let help_text = vec![
            Line::from(vec![Span::styled("FCA Chat Help", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))]),
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
        
        let help = Paragraph::new(help_text)
            .block(Block::default().title("Help").borders(Borders::ALL))
            .wrap(Wrap { trim: true });
        f.render_widget(help, popup_area);
    }

    fn draw_room_browser(&self, f: &mut Frame) {
        let popup_area = Self::centered_rect(60, 40, f.area());
        f.render_widget(Clear, popup_area);
        
        let block = Block::default().title("Room Browser").borders(Borders::ALL);
        let content = Paragraph::new("Room browser not yet implemented.\nUse /join <room_id> to join a room.")
            .block(block)
            .wrap(Wrap { trim: true });
        f.render_widget(content, popup_area);
    }

    fn draw_settings(&self, f: &mut Frame) {
        let popup_area = Self::centered_rect(50, 30, f.area());
        f.render_widget(Clear, popup_area);
        
        let block = Block::default().title("Settings").borders(Borders::ALL);
        let content = Paragraph::new("Settings not yet implemented.")
            .block(block);
        f.render_widget(content, popup_area);
    }

    fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
        let popup_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage((100 - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((100 - percent_y) / 2),
            ])
            .split(r);

        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ])
            .split(popup_layout[1])[1]
    }
}
