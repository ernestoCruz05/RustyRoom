//! TUI Chat Client for RustyRoom
//!
//! This module provides a terminal-based user interface for the chat application
//! using the `ratatui` library. It supports real-time messaging, room management,
//! voice chat with configurable audio devices, private messaging, and user presence.
//!
//! The client runs in a terminal alternate screen and handles keyboard input
//! for navigation, text entry, and command execution.

use crate::audio::{self, AudioDeviceInfo};
use crate::resc::{Message, MessageType, RoomInfo, UserStatus};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap, Gauge},
};
use ringbuf::traits::*;
use std::{
    collections::HashMap,
    io::{self, BufRead, BufReader, Write},
    net::{TcpStream, UdpSocket},
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

/// Types of toast notifications displayed in the UI.
///
/// Used to style notifications with appropriate colors and icons.
#[derive(Debug, Clone)]
pub enum NotificationType {
    /// Operation completed successfully (green)
    Success,
    /// Informational message (blue)
    Info,
    /// Warning that requires attention (yellow)
    Warning,
    /// Error or failure (red)
    Error,
}

/// A toast notification displayed temporarily in the UI.
///
/// Notifications appear at the top of the screen and automatically
/// disappear after their duration expires.
#[derive(Debug, Clone)]
pub struct Notification {
    /// The notification message text
    pub message: String,
    /// The type/severity of the notification
    pub notification_type: NotificationType,
    /// When the notification was created
    pub created_at: Instant,
    /// How long the notification should be displayed
    pub duration: Duration,
}

impl Notification {
    pub fn new(message: String, notification_type: NotificationType) -> Self {
        Self {
            message,
            notification_type,
            created_at: Instant::now(),
            duration: Duration::from_secs(3),
        }
    }

    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed() > self.duration
    }
}

/// UI state update messages received from background threads.
///
/// These messages are sent from the network receiver thread to update
/// the UI state in response to server events.
#[derive(Debug, Clone)]
pub enum UIUpdate {
    /// Authentication was successful
    AuthSuccess {
        user_id: u16,
    },
    AuthFailure {
        reason: String,
    },
    RoomCreated {
        room_id: u16,
        name: String,
        description: String,
    },
    RoomJoined {
        room_id: u16,
        name: String,
        description: String,
    },
    RoomMessage {
        room_id: u16,
        sender: String,
        content: String,
    },
    ServerMessage {
        content: String,
        is_error: bool,
    },
    UserListUpdate {
        users: Vec<UserStatus>,
    },
    RoomList {
        rooms: Vec<RoomInfo>,
    },
    PrivateMessage {
        from_user_id: u16,
        sender: String,
        content: String,
    },
    VoiceStatus {
        connected: bool,
        message: String,
    },
    VoiceLevel {
        level: f32,
    },
}

/// Types of chat messages for styling and display purposes.
#[derive(Debug, Clone)]
pub enum ChatMessageType {
    /// Regular message from a user
    User,
    /// System notification or status message
    System,
    /// Private/direct message
    Private,
    /// Error message
    Error,
}

/// A single chat message displayed in the message list.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    /// Formatted timestamp string (HH:MM:SS)
    pub timestamp: String,
    /// Username of the message sender
    pub sender: String,
    /// The message content
    pub content: String,
    /// Type of message for styling
    pub message_type: ChatMessageType,
}

/// A chat room with its messages and metadata.
#[derive(Debug, Clone)]
pub struct Room {
    /// Unique room identifier
    pub id: u16,
    /// Display name of the room
    pub name: String,
    /// Room description
    pub description: String,
    /// Number of users currently in the room
    pub user_count: u16,
    /// Message history for this room
    pub messages: Vec<ChatMessage>,
}

/// Tab selection for the settings screen.
#[derive(Debug, Clone, PartialEq)]
enum SettingsTab {
    /// Audio device configuration (input/output devices, volumes)
    Audio,
    /// Voice chat settings
    Voice,
    /// UI appearance and behavior settings
    Interface,
}

/// Audio device configuration and volume settings.
///
/// Stores the list of available audio devices and the user's
/// selected input/output devices along with volume levels.
#[derive(Debug)]
struct AudioSettings {
    /// Available audio input (microphone) devices
    input_devices: Vec<AudioDeviceInfo>,
    /// Available audio output (speaker) devices
    output_devices: Vec<AudioDeviceInfo>,
    /// Index of the selected input device
    selected_input_index: usize,
    /// Index of the selected output device
    selected_output_index: usize,
    /// Input volume level (0-100)
    input_volume: u8,
    /// Output volume level (0-100)
    output_volume: u8,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            input_devices: Vec::new(),
            output_devices: Vec::new(),
            selected_input_index: 0,
            selected_output_index: 0,
            input_volume: 100,
            output_volume: 100,
        }
    }
}

/// The main TUI chat client application.
///
/// Manages all client state including authentication, room membership,
/// message history, voice chat, and UI components. The client connects
/// to the server over TCP and handles real-time message updates.
#[derive(Debug)]
pub struct TuiChatClient {
    server_address: String,

    // Authentication state
    authenticated: bool,
    user_id: u16,
    username: String,

    // Chat state
    input: String,
    current_room: Option<u16>,
    rooms: HashMap<u16, Room>,
    joined_rooms: Vec<u16>,

    // UI state
    show_help: bool,
    show_room_browser: bool,
    show_settings: bool,
    show_private_messages: bool,
    cursor_position: usize,

    // Room browser state
    available_rooms: Vec<RoomInfo>,
    selected_room_index: usize,
    
    // Private messages state
    private_conversations: HashMap<u16, Vec<ChatMessage>>,
    selected_private_user: Option<u16>,
    selected_user_index: usize,

    // Auth form state
    auth_mode: AuthMode,
    auth_username: String,
    auth_password: String,
    auth_field: AuthField,
    auth_error: Option<String>,

    // Network state
    message_sender: Option<mpsc::UnboundedSender<Message>>,
    ui_receiver: Option<mpsc::UnboundedReceiver<UIUpdate>>,
    online_users: Vec<UserStatus>,
    
    // Cursor animation
    cursor_visible: bool,
    last_cursor_toggle: Instant,
    
    // Voice state
    voice_connected: bool,
    voice_muted: bool,
    voice_deafened: bool,
    voice_level: f32,
    voice_stop_signal: Option<Arc<AtomicBool>>,
    
    // Audio settings
    audio_settings: AudioSettings,
    settings_tab: SettingsTab,
    settings_field: usize,
    
    // Notifications
    notifications: Vec<Notification>,
    
    // Message scroll state
    message_scroll_offset: usize,
    messages_list_state: ListState,
}

/// Authentication screen mode selection.
#[derive(Debug, Clone, PartialEq)]
enum AuthMode {
    /// Login with existing credentials
    Login,
    /// Register a new account
    Register,
}

#[derive(Debug, Clone, PartialEq)]
enum AuthField {
    Username,
    Password,
}

impl TuiChatClient {
    /// Creates a new TUI client instance.
    ///
    /// Initializes the client with the given server address and loads
    /// available audio devices. Does not connect to the server yet.
    pub fn new(server_address: String) -> Self {
        // Suppress ALSA error messages on Linux (they're noisy but harmless)
        audio::suppress_alsa_errors();
        
        // Load audio devices on startup
        let input_devices = audio::list_input_devices();
        let output_devices = audio::list_output_devices();
        
        // Find default device indices
        let selected_input_index = input_devices
            .iter()
            .position(|d| d.is_default)
            .unwrap_or(0);
        let selected_output_index = output_devices
            .iter()
            .position(|d| d.is_default)
            .unwrap_or(0);
        
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
            voice_connected: false,
            voice_muted: false,
            voice_deafened: false,
            voice_level: 0.0,
            voice_stop_signal: None,
            audio_settings: AudioSettings {
                input_devices,
                output_devices,
                selected_input_index,
                selected_output_index,
                input_volume: 100,
                output_volume: 100,
            },
            settings_tab: SettingsTab::Audio,
            settings_field: 0,
            notifications: Vec::new(),
            message_scroll_offset: 0,
            messages_list_state: ListState::default(),
        }
    }
    
    /// Add a notification toast
    fn add_notification(&mut self, message: String, notification_type: NotificationType) {
        self.notifications.push(Notification::new(message, notification_type));
        // Keep only last 5 notifications
        if self.notifications.len() > 5 {
            self.notifications.remove(0);
        }
    }
    
    /// Refresh audio device list
    fn refresh_audio_devices(&mut self) {
        self.audio_settings.input_devices = audio::list_input_devices();
        self.audio_settings.output_devices = audio::list_output_devices();
        
        // Clamp indices
        if self.audio_settings.selected_input_index >= self.audio_settings.input_devices.len() {
            self.audio_settings.selected_input_index = 0;
        }
        if self.audio_settings.selected_output_index >= self.audio_settings.output_devices.len() {
            self.audio_settings.selected_output_index = 0;
        }
    }
    
    /// Get selected input device name
    fn selected_input_device(&self) -> Option<&str> {
        self.audio_settings
            .input_devices
            .get(self.audio_settings.selected_input_index)
            .map(|d| d.name.as_str())
    }
    
    /// Get selected output device name  
    fn selected_output_device(&self) -> Option<&str> {
        self.audio_settings
            .output_devices
            .get(self.audio_settings.selected_output_index)
            .map(|d| d.name.as_str())
    }

    /// Starts the TUI application.
    ///
    /// Sets up the terminal in raw mode, creates the client instance,
    /// and runs the main event loop. Restores terminal state on exit.
    pub async fn start_tui(server_address: &str) -> io::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let mut app = TuiChatClient::new(server_address.to_string());
        let result = app.run(&mut terminal).await;

        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        result
    }

    /// Main event loop that processes input and updates the UI.
    ///
    /// Connects to the server, then continuously polls for keyboard events
    /// and UI updates from background threads. Redraws the screen each tick.
    async fn run(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<()> {
        self.connect().await?;

        let mut last_tick = Instant::now();
        let mut last_refresh = Instant::now();
        let tick_rate = Duration::from_millis(50);
        let refresh_rate = Duration::from_secs(10); // Refresh user list every 10 seconds

        loop {
            if let Some(ui_receiver) = &mut self.ui_receiver {
                if let Ok(ui_update) = ui_receiver.try_recv() {
                    match ui_update {
                        UIUpdate::AuthSuccess { user_id } => {
                            self.authenticated = true;
                            self.user_id = user_id;
                            // FIX: Use the username from the input field
                            self.username = self.auth_username.clone();
                            self.auth_error = None;
                            self.auth_password.clear();
                        }
                        UIUpdate::AuthFailure { reason } => {
                            self.auth_error = Some(reason);
                        }
                        UIUpdate::UserListUpdate { users } => {
                            self.online_users = users;
                        }
                        UIUpdate::RoomList { rooms } => {
                            self.available_rooms = rooms;
                            self.selected_room_index = 0;
                        }
                        UIUpdate::RoomJoined {
                            room_id,
                            name,
                            description,
                        } => {
                            if !self.joined_rooms.contains(&room_id) {
                                self.joined_rooms.push(room_id);
                            }
                            if self.current_room.is_none() {
                                self.current_room = Some(room_id);
                            }
                            let room = Room {
                                id: room_id,
                                name,
                                description,
                                user_count: 1,
                                messages: Vec::new(),
                            };
                            self.rooms.insert(room_id, room);
                        }
                        UIUpdate::RoomMessage {
                            room_id,
                            sender,
                            content,
                        } => {
                            if let Some(room) = self.rooms.get_mut(&room_id) {
                                let message = ChatMessage {
                                    timestamp: Self::current_time(),
                                    sender,
                                    content,
                                    message_type: ChatMessageType::User,
                                };
                                room.messages.push(message);
                            }
                        }
                        UIUpdate::PrivateMessage {
                            from_user_id,
                            sender,
                            content,
                        } => {
                            let message = ChatMessage {
                                timestamp: Self::current_time(),
                                sender,
                                content,
                                message_type: ChatMessageType::Private,
                            };
                            self.private_conversations
                                .entry(from_user_id)
                                .or_insert_with(Vec::new)
                                .push(message);
                        }
                        UIUpdate::ServerMessage { content, is_error } => {
                            if let Some(room_id) = self.current_room {
                                if let Some(room) = self.rooms.get_mut(&room_id) {
                                    let message_type = if is_error {
                                        ChatMessageType::Error
                                    } else {
                                        ChatMessageType::System
                                    };
                                    room.messages.push(ChatMessage {
                                        timestamp: Self::current_time(),
                                        sender: "System".to_string(),
                                        content,
                                        message_type,
                                    });
                                }
                            }
                        }
                        UIUpdate::VoiceStatus { connected, message } => {
                            let was_connected = self.voice_connected;
                            self.voice_connected = connected;
                            
                            // Add notification
                            if connected && !was_connected {
                                self.add_notification("Voice connected!".to_string(), NotificationType::Success);
                            } else if !connected && was_connected {
                                self.add_notification("Voice disconnected".to_string(), NotificationType::Info);
                            }
                            
                            if let Some(room_id) = self.current_room {
                                if let Some(room) = self.rooms.get_mut(&room_id) {
                                    room.messages.push(ChatMessage {
                                        timestamp: Self::current_time(),
                                        sender: "[Voice]".to_string(),
                                        content: message,
                                        message_type: ChatMessageType::System,
                                    });
                                }
                            }
                        }
                        UIUpdate::VoiceLevel { level } => {
                            self.voice_level = level;
                        }
                        _ => {}
                    }
                }
            }
            
            // Clean up expired notifications
            self.notifications.retain(|n| !n.is_expired());

            terminal.draw(|f| self.draw(f))?;

            if last_tick.elapsed() >= Duration::from_millis(500) {
                self.cursor_visible = !self.cursor_visible;
                self.last_cursor_toggle = Instant::now();
            }

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
            
            // Periodically refresh the user list to keep it current
            if self.authenticated && last_refresh.elapsed() >= refresh_rate {
                if let Some(sender) = &self.message_sender {
                    let _ = sender.send(Message::new(self.user_id, MessageType::RequestUserList));
                }
                last_refresh = Instant::now();
            }
        }

        Ok(())
    }

    /// Establishes TCP connection to the server.
    ///
    /// Creates reader and writer threads for message handling and
    /// sets up channels for UI updates.
    async fn connect(&mut self) -> io::Result<()> {
        let stream = TcpStream::connect(&self.server_address)?;
        
        // Set TCP_NODELAY for lower latency
        let _ = stream.set_nodelay(true);
        
        // Set read timeout to detect dead connections
        // If no data for 60 seconds, the read will fail and trigger reconnection
        let _ = stream.set_read_timeout(Some(Duration::from_secs(120)));
        
        let read_stream = stream.try_clone()?;
        let write_stream = stream.try_clone()?;

        let (tx, mut rx) = mpsc::unbounded_channel();
        self.message_sender = Some(tx);

        let (ui_tx, ui_rx) = mpsc::unbounded_channel();
        self.ui_receiver = Some(ui_rx);

        // CHANGED: Removed capturing of self.auth_username here
        let server_address = self.server_address.clone();

        thread::spawn(move || {
            Self::message_receiver(read_stream, ui_tx, server_address);
        });

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

    // CHANGED: Removed 'username' argument
    fn message_receiver(
        stream: TcpStream,
        ui_sender: mpsc::UnboundedSender<UIUpdate>,
        server_address: String,
    ) {
        let reader = BufReader::new(stream);
        let server_ip = server_address
            .split(':')
            .next()
            .unwrap_or("127.0.0.1")
            .to_string();

        for line in reader.lines() {
            match line {
                Ok(json_message) => {
                    if let Ok(message) = Message::from_json_file(&json_message) {
                        match &message.message_type {
                            MessageType::VoiceCredentials { token, udp_port } => {
                                let ip_clone = server_ip.clone();
                                let token_clone = token.clone();
                                let port_clone = *udp_port;
                                let ui_sender_clone = ui_sender.clone();

                                let _ = ui_sender.send(UIUpdate::ServerMessage {
                                    content: "Connecting to voice server...".to_string(),
                                    is_error: false,
                                });

                                thread::spawn(move || {
                                    Self::start_udp_voice(
                                        ip_clone,
                                        port_clone,
                                        token_clone,
                                        ui_sender_clone,
                                    );
                                });
                            }
                            MessageType::AuthSuccess {
                                user_id,
                                message: _,
                            } => {
                                // CHANGED: Removed username from message
                                let _ = ui_sender.send(UIUpdate::AuthSuccess { user_id: *user_id });
                            }
                            MessageType::AuthFailure { reason } => {
                                let _ = ui_sender.send(UIUpdate::AuthFailure {
                                    reason: reason.clone(),
                                });
                            }
                            MessageType::RoomCreated {
                                room_id: _,
                                name,
                                description: _,
                            } => {
                                let _ = ui_sender.send(UIUpdate::ServerMessage {
                                    content: format!("Room '{}' created", name),
                                    is_error: false,
                                });
                            }
                            MessageType::RoomJoined {
                                room_id,
                                name,
                                description,
                            } => {
                                let _ = ui_sender.send(UIUpdate::RoomJoined {
                                    room_id: *room_id,
                                    name: name.clone(),
                                    description: description.clone(),
                                });
                            }
                            MessageType::RoomMessage {
                                room_id,
                                sender_username,
                                content,
                            } => {
                                let _ = ui_sender.send(UIUpdate::RoomMessage {
                                    room_id: *room_id,
                                    sender: sender_username.clone(),
                                    content: content.clone(),
                                });
                            }
                            MessageType::UserListUpdate { users } => {
                                let _ = ui_sender.send(UIUpdate::UserListUpdate {
                                    users: users.clone(),
                                });
                            }
                            MessageType::RoomList { rooms } => {
                                let _ = ui_sender.send(UIUpdate::RoomList {
                                    rooms: rooms.clone(),
                                });
                            }
                            MessageType::PrivateMessage {
                                to_user_id: _,
                                sender_username,
                                content,
                            } => {
                                let _ = ui_sender.send(UIUpdate::PrivateMessage {
                                    from_user_id: message.sender_id,
                                    sender: sender_username.clone(),
                                    content: content.clone(),
                                });
                            }
                            MessageType::ServerResponse { success, content } => {
                                let _ = ui_sender.send(UIUpdate::ServerMessage {
                                    content: content.clone(),
                                    is_error: !success,
                                });
                            }
                            _ => {}
                        }
                    }
                }
                Err(_) => break,
            }
        }
    }

    fn start_udp_voice(
        server_ip: String,
        udp_port: u16,
        token: String,
        ui_sender: mpsc::UnboundedSender<UIUpdate>,
    ) {
        let (_manager, mut mic_consumer, mut speaker_producer) = match audio::AudioManager::new() {
            Ok(v) => v,
            Err(e) => {
                let _ = ui_sender.send(UIUpdate::VoiceStatus {
                    connected: false,
                    message: format!("Audio Init Failed: {}", e),
                });
                return;
            }
        };

        let socket = match UdpSocket::bind("0.0.0.0:0") {
            Ok(s) => s,
            Err(e) => {
                let _ = ui_sender.send(UIUpdate::VoiceStatus {
                    connected: false,
                    message: format!("UDP Bind Failed: {}", e),
                });
                return;
            }
        };

        let server_addr = format!("{}:{}", server_ip, udp_port);
        if let Err(e) = socket.send_to(token.as_bytes(), &server_addr) {
            let _ = ui_sender.send(UIUpdate::VoiceStatus {
                connected: false,
                message: format!("Handshake Failed: {}", e),
            });
            return;
        }

        let mut buf = [0u8; 4096];
        let mut audio_chunk = Vec::with_capacity(1024);
        let _ = socket.set_read_timeout(Some(Duration::from_millis(5)));

        let _ = ui_sender.send(UIUpdate::VoiceStatus {
            connected: true,
            message: "Voice connection initialized...".to_string(),
        });

        loop {
            // Sending
            audio_chunk.clear();
            while let Some(sample) = mic_consumer.try_pop() {
                audio_chunk.push(sample);
                if audio_chunk.len() >= 256 {
                    break;
                }
            }

            if !audio_chunk.is_empty() {
                let byte_data: &[u8] = unsafe {
                    std::slice::from_raw_parts(
                        audio_chunk.as_ptr() as *const u8,
                        audio_chunk.len() * 4,
                    )
                };
                let _ = socket.send_to(byte_data, &server_addr);
            }

            // Receiving
            match socket.recv_from(&mut buf) {
                Ok((size, _)) => {
                    if size == 15 && &buf[..15] == b"VOICE_CONNECTED" {
                        let _ = ui_sender.send(UIUpdate::VoiceStatus {
                            connected: true,
                            message: "Voice Connected! :3".to_string(),
                        });
                        continue;
                    }

                    let sample_count = size / 4;
                    let samples: &[f32] = unsafe {
                        std::slice::from_raw_parts(buf.as_ptr() as *const f32, sample_count)
                    };

                    for &sample in samples {
                        let _ = speaker_producer.try_push(sample);
                    }
                }
                Err(_) => {}
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

    /// Handles keyboard input and dispatches to appropriate handlers.
    ///
    /// Returns `true` if the application should exit, `false` otherwise.
    /// Delegates to auth screen handler when not authenticated.
    async fn handle_key_event(&mut self, key: KeyCode) -> io::Result<bool> {
        if !self.authenticated {
            return Ok(self.handle_auth_key_event(key).await?);
        }

        match key {
            KeyCode::Esc => {
                if self.show_help
                    || self.show_room_browser
                    || self.show_settings
                    || self.show_private_messages
                {
                    self.show_help = false;
                    self.show_room_browser = false;
                    self.show_settings = false;
                    self.show_private_messages = false;
                } else {
                    return Ok(true);
                }
            }
            KeyCode::F(1) => self.show_help = !self.show_help,
            KeyCode::F(2) => {
                self.show_room_browser = !self.show_room_browser;
                if self.show_room_browser {
                    if let Some(sender) = &self.message_sender {
                        let _ = sender.send(Message::new(self.user_id, MessageType::ListRooms));
                    }
                }
            }
            KeyCode::F(3) => {
                self.show_private_messages = !self.show_private_messages;
                if self.show_private_messages {
                    self.selected_user_index = 0;
                    self.update_selected_private_user();
                }
            }
            KeyCode::F(4) => {
                self.show_settings = !self.show_settings;
                if self.show_settings {
                    self.refresh_audio_devices();
                    self.settings_field = 0;
                }
            }
            KeyCode::F(5) => {
                if self.voice_connected {
                    // Stop voice
                    if let Some(stop_signal) = &self.voice_stop_signal {
                        stop_signal.store(true, Ordering::Relaxed);
                    }
                    self.voice_connected = false;
                    self.voice_stop_signal = None;
                    self.add_notification("Voice disconnected".to_string(), NotificationType::Info);
                } else {
                    // Start voice
                    if let Some(sender) = &self.message_sender {
                        let _ = sender.send(Message::new(self.user_id, MessageType::RequestVoice));
                    }
                }
            }
            // Voice mute toggle (Ctrl+M or when voice is connected and not in input mode)
            KeyCode::F(6) => {
                if self.voice_connected {
                    self.voice_muted = !self.voice_muted;
                    let status = if self.voice_muted { "Microphone muted" } else { "Microphone unmuted" };
                    self.add_notification(status.to_string(), NotificationType::Info);
                }
            }
            // Voice deafen toggle
            KeyCode::F(7) => {
                if self.voice_connected {
                    self.voice_deafened = !self.voice_deafened;
                    let status = if self.voice_deafened { "Audio deafened" } else { "Audio undeafened" };
                    self.add_notification(status.to_string(), NotificationType::Info);
                }
            }
            KeyCode::Enter => {
                if self.show_settings {
                    // Handle settings selection
                    self.handle_settings_enter();
                } else if self.show_room_browser {
                    if !self.available_rooms.is_empty()
                        && self.selected_room_index < self.available_rooms.len()
                    {
                        let room = &self.available_rooms[self.selected_room_index];
                        if let Some(sender) = &self.message_sender {
                            let message = Message::new(
                                self.user_id,
                                MessageType::Join {
                                    room_id: room.id,
                                    password: None,
                                },
                            );
                            let _ = sender.send(message);
                        }
                        self.show_room_browser = false;
                    }
                } else if self.show_private_messages {
                    self.update_selected_private_user();
                } else if !self.input.is_empty() {
                    if self.show_private_messages && self.selected_private_user.is_some() {
                        if let Some(sender) = &self.message_sender {
                            let message = Message::new(
                                self.user_id,
                                MessageType::PrivateMessage {
                                    to_user_id: self.selected_private_user.unwrap(),
                                    sender_username: self.username.clone(),
                                    content: self.input.clone(),
                                },
                            );
                            let _ = sender.send(message);
                        }
                    } else {
                        self.send_message().await?;
                    }
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
                if self.show_settings {
                    self.handle_settings_left();
                } else if self.show_room_browser {
                    if self.selected_room_index > 0 {
                        self.selected_room_index -= 1;
                    }
                } else if self.cursor_position > 0 {
                    self.cursor_position -= 1;
                }
            }
            KeyCode::Right => {
                if self.show_settings {
                    self.handle_settings_right();
                } else if self.show_room_browser {
                    if self.selected_room_index < self.available_rooms.len().saturating_sub(1) {
                        self.selected_room_index += 1;
                    }
                } else if self.cursor_position < self.input.len() {
                    self.cursor_position += 1;
                }
            }
            KeyCode::Up => {
                if self.show_settings {
                    if self.settings_field > 0 {
                        self.settings_field -= 1;
                    }
                } else if self.show_room_browser {
                    if self.selected_room_index > 0 {
                        self.selected_room_index -= 1;
                    }
                } else if self.show_private_messages {
                    if self.selected_user_index > 0 {
                        self.selected_user_index -= 1;
                        self.update_selected_private_user();
                    }
                } else {
                    // Scroll messages up
                    self.message_scroll_offset = self.message_scroll_offset.saturating_add(1);
                }
            }
            KeyCode::Down => {
                if self.show_settings {
                    self.settings_field += 1;
                    // Clamp based on current tab
                    let max_field = match self.settings_tab {
                        SettingsTab::Audio => 3,  // input device, output device, input vol, output vol
                        SettingsTab::Voice => 2,  // push-to-talk, vad threshold
                        SettingsTab::Interface => 1,
                    };
                    if self.settings_field > max_field {
                        self.settings_field = max_field;
                    }
                } else if self.show_room_browser {
                    if self.selected_room_index < self.available_rooms.len().saturating_sub(1) {
                        self.selected_room_index += 1;
                    }
                } else if self.show_private_messages {
                    let user_count = self
                        .online_users
                        .iter()
                        .filter(|user| user.user_id != self.user_id)
                        .count();
                    if self.selected_user_index < user_count.saturating_sub(1) {
                        self.selected_user_index += 1;
                        self.update_selected_private_user();
                    }
                } else {
                    // Scroll messages down
                    self.message_scroll_offset = self.message_scroll_offset.saturating_sub(1);
                }
            }
            KeyCode::PageUp => {
                self.message_scroll_offset = self.message_scroll_offset.saturating_add(10);
            }
            KeyCode::PageDown => {
                self.message_scroll_offset = self.message_scroll_offset.saturating_sub(10);
            }
            KeyCode::Home => {
                self.cursor_position = 0;
            }
            KeyCode::End => {
                self.cursor_position = self.input.len();
            }
            KeyCode::Tab => {
                if self.show_settings {
                    // Cycle through settings tabs
                    self.settings_tab = match self.settings_tab {
                        SettingsTab::Audio => SettingsTab::Voice,
                        SettingsTab::Voice => SettingsTab::Interface,
                        SettingsTab::Interface => SettingsTab::Audio,
                    };
                    self.settings_field = 0;
                } else {
                    self.switch_room();
                }
            }
            KeyCode::Char(c) => {
                self.input.insert(self.cursor_position, c);
                self.cursor_position += 1;
            }
            _ => {}
        }
        Ok(false)
    }
    
    /// Handle settings field selection with Enter
    fn handle_settings_enter(&mut self) {
        match self.settings_tab {
            SettingsTab::Audio => {
                // Cycle through devices or toggle
                match self.settings_field {
                    0 => {
                        // Input device - cycle to next
                        if !self.audio_settings.input_devices.is_empty() {
                            self.audio_settings.selected_input_index = 
                                (self.audio_settings.selected_input_index + 1) 
                                % self.audio_settings.input_devices.len();
                            if let Some(name) = self.selected_input_device() {
                                self.add_notification(
                                    format!("Input: {}", name), 
                                    NotificationType::Info
                                );
                            }
                        }
                    }
                    1 => {
                        // Output device - cycle to next
                        if !self.audio_settings.output_devices.is_empty() {
                            self.audio_settings.selected_output_index = 
                                (self.audio_settings.selected_output_index + 1) 
                                % self.audio_settings.output_devices.len();
                            if let Some(name) = self.selected_output_device() {
                                self.add_notification(
                                    format!("Output: {}", name), 
                                    NotificationType::Info
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
            SettingsTab::Voice => {}
            SettingsTab::Interface => {}
        }
    }
    
    /// Handle left arrow in settings (decrease values)
    fn handle_settings_left(&mut self) {
        match self.settings_tab {
            SettingsTab::Audio => {
                match self.settings_field {
                    0 => {
                        // Previous input device
                        if !self.audio_settings.input_devices.is_empty() {
                            if self.audio_settings.selected_input_index > 0 {
                                self.audio_settings.selected_input_index -= 1;
                            } else {
                                self.audio_settings.selected_input_index = 
                                    self.audio_settings.input_devices.len() - 1;
                            }
                        }
                    }
                    1 => {
                        // Previous output device
                        if !self.audio_settings.output_devices.is_empty() {
                            if self.audio_settings.selected_output_index > 0 {
                                self.audio_settings.selected_output_index -= 1;
                            } else {
                                self.audio_settings.selected_output_index = 
                                    self.audio_settings.output_devices.len() - 1;
                            }
                        }
                    }
                    2 => {
                        // Decrease input volume
                        self.audio_settings.input_volume = 
                            self.audio_settings.input_volume.saturating_sub(5);
                    }
                    3 => {
                        // Decrease output volume
                        self.audio_settings.output_volume = 
                            self.audio_settings.output_volume.saturating_sub(5);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    
    /// Handle right arrow in settings (increase values)
    fn handle_settings_right(&mut self) {
        match self.settings_tab {
            SettingsTab::Audio => {
                match self.settings_field {
                    0 => {
                        // Next input device
                        if !self.audio_settings.input_devices.is_empty() {
                            self.audio_settings.selected_input_index = 
                                (self.audio_settings.selected_input_index + 1) 
                                % self.audio_settings.input_devices.len();
                        }
                    }
                    1 => {
                        // Next output device
                        if !self.audio_settings.output_devices.is_empty() {
                            self.audio_settings.selected_output_index = 
                                (self.audio_settings.selected_output_index + 1) 
                                % self.audio_settings.output_devices.len();
                        }
                    }
                    2 => {
                        // Increase input volume
                        self.audio_settings.input_volume = 
                            (self.audio_settings.input_volume + 5).min(100);
                    }
                    3 => {
                        // Increase output volume
                        self.audio_settings.output_volume = 
                            (self.audio_settings.output_volume + 5).min(100);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    async fn handle_auth_key_event(&mut self, key: KeyCode) -> io::Result<bool> {
        match key {
            KeyCode::Esc => return Ok(true),
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
                if !self.auth_username.is_empty() && !self.auth_password.is_empty() {
                    self.send_auth_message().await?;
                }
            }
            KeyCode::Backspace => match self.auth_field {
                AuthField::Username => {
                    self.auth_username.pop();
                }
                AuthField::Password => {
                    self.auth_password.pop();
                }
            },
            KeyCode::Char(c) => match self.auth_field {
                AuthField::Username => {
                    self.auth_username.push(c);
                }
                AuthField::Password => {
                    self.auth_password.push(c);
                }
            },
            _ => {}
        }
        Ok(false)
    }

    async fn send_auth_message(&mut self) -> io::Result<()> {
        if let Some(sender) = &self.message_sender {
            let message = match self.auth_mode {
                AuthMode::Login => Message::new(
                    0,
                    MessageType::Login {
                        username: self.auth_username.clone(),
                        password: self.auth_password.clone(),
                    },
                ),
                AuthMode::Register => Message::new(
                    0,
                    MessageType::Register {
                        username: self.auth_username.clone(),
                        password: self.auth_password.clone(),
                    },
                ),
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
                let message = Message::new(
                    self.user_id,
                    MessageType::RoomMessage {
                        room_id,
                        sender_username: self.username.clone(),
                        content: self.input.clone(),
                    },
                );
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
                            let _ = sender.send(Message::new(
                                self.user_id,
                                MessageType::Join { room_id, password },
                            ));
                        }
                    }
                }
                Some(&"/create") => {
                    if let Some(args) = Self::parse_quoted_args(&self.input) {
                        if args.len() >= 3 {
                            let name = args[1].clone();
                            let description = args[2].clone();
                            let password = if args.len() > 3 {
                                Some(args[3].clone())
                            } else {
                                None
                            };
                            let _ = sender.send(Message::new(
                                self.user_id,
                                MessageType::CreateRoom {
                                    name,
                                    description,
                                    password,
                                },
                            ));
                        }
                    }
                }
                Some(&"/leave") => {
                    if let Some(room_id_str) = parts.get(1) {
                        if let Ok(room_id) = room_id_str.parse::<u16>() {
                            let _ = sender
                                .send(Message::new(self.user_id, MessageType::Leave { room_id }));
                            self.joined_rooms.retain(|&id| id != room_id);
                            if self.current_room == Some(room_id) {
                                self.current_room = self.joined_rooms.first().copied();
                            }
                        }
                    }
                }
                _ => {}
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
            match ch {
                '"' => in_quotes = !in_quotes,
                ' ' if !in_quotes => {
                    if !current_arg.is_empty() {
                        args.push(current_arg.trim().to_string());
                        current_arg.clear();
                    }
                }
                _ => current_arg.push(ch),
            }
        }
        if !current_arg.is_empty() {
            args.push(current_arg.trim().to_string());
        }
        if args.is_empty() { None } else { Some(args) }
    }

    fn update_selected_private_user(&mut self) {
        let other_users: Vec<&UserStatus> = self
            .online_users
            .iter()
            .filter(|user| user.user_id != self.user_id)
            .collect();

        if !other_users.is_empty() && self.selected_user_index < other_users.len() {
            self.selected_private_user = Some(other_users[self.selected_user_index].user_id);
        } else {
            self.selected_private_user = None;
        }
    }

    fn switch_room(&mut self) {
        if let Some(current) = self.current_room {
            if let Some(current_index) = self.joined_rooms.iter().position(|&id| id == current) {
                let next_index = (current_index + 1) % self.joined_rooms.len();
                self.current_room = self.joined_rooms.get(next_index).copied();
            }
        }
    }

    /// Renders the UI to the terminal frame.
    ///
    /// Displays either the authentication screen or the main chat interface
    /// depending on the current authentication state.
    fn draw(&self, f: &mut Frame) {
        if !self.authenticated {
            self.draw_auth_screen(f);
        } else {
            self.draw_main_screen(f);
        }
    }

    fn draw_auth_screen(&self, f: &mut Frame) {
        let size = f.area();
        let popup_area = Self::centered_rect_fixed(60, 15, size);
        f.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(format!(
                "FCA - {}",
                if self.auth_mode == AuthMode::Login {
                    "Login"
                } else {
                    "Register"
                }
            ))
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::Cyan));
        f.render_widget(block, popup_area);

        let inner = popup_area.inner(ratatui::layout::Margin {
            horizontal: 2,
            vertical: 2,
        });
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(1),
            ])
            .split(inner);

        let username_style = if self.auth_field == AuthField::Username {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        let username = Paragraph::new(self.auth_username.as_str()).block(
            Block::default()
                .title("Username")
                .borders(Borders::ALL)
                .style(username_style),
        );
        f.render_widget(username, chunks[0]);

        let password_style = if self.auth_field == AuthField::Password {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        let password_display = "*".repeat(self.auth_password.len());
        let password = Paragraph::new(password_display.as_str()).block(
            Block::default()
                .title("Password")
                .borders(Borders::ALL)
                .style(password_style),
        );
        f.render_widget(password, chunks[1]);

        let instructions = vec![
            Line::from(vec![
                Span::raw("Tab: Switch fields | F2: Toggle "),
                Span::styled(
                    if self.auth_mode == AuthMode::Login {
                        "Register"
                    } else {
                        "Login"
                    },
                    Style::default().fg(Color::Green),
                ),
            ]),
            Line::from("Enter: Submit | Esc: Quit"),
        ];
        let help = Paragraph::new(instructions)
            .block(Block::default().title("Controls").borders(Borders::ALL));
        f.render_widget(help, chunks[2]);

        if let Some(error) = &self.auth_error {
            let error_msg = Paragraph::new(error.as_str())
                .style(Style::default().fg(Color::Red))
                .wrap(Wrap { trim: true });
            f.render_widget(error_msg, chunks[3]);
        }
    }

    fn draw_main_screen(&self, f: &mut Frame) {
        let size = f.area();
        let main_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(75), Constraint::Percentage(25)])
            .split(size);

        let left_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),  // Header with voice indicator
                Constraint::Length(3),  // Room tabs
                Constraint::Min(1),     // Chat area
                Constraint::Length(3),  // Input
                Constraint::Length(2),  // Status bar
            ])
            .split(main_chunks[0]);

        // Header with username and voice status
        let header_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(20), Constraint::Length(15)])
            .split(left_chunks[0]);

        let header = Paragraph::new(format!("FCA - Connected as: {}", self.username))
            .style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .block(Block::default().borders(Borders::ALL));
        f.render_widget(header, header_chunks[0]);

        // Voice status indicator in header
        if self.voice_connected {
            let voice_status = if self.voice_muted {
                ("MUTED", Color::Red)
            } else if self.voice_deafened {
                ("DEAF", Color::Red)
            } else {
                ("LIVE", Color::Green)
            };
            let voice_indicator = Paragraph::new(voice_status.0)
                .style(Style::default().fg(voice_status.1).add_modifier(Modifier::BOLD))
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(voice_indicator, header_chunks[1]);
        } else {
            let voice_off = Paragraph::new("Voice OFF")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(voice_off, header_chunks[1]);
        }

        // Room tabs
        let room_titles: Vec<String> = self
            .joined_rooms
            .iter()
            .filter_map(|&id| self.rooms.get(&id))
            .map(|room| format!(" {} ({}) ", room.name, room.user_count))
            .collect();

        let selected_tab = self
            .current_room
            .and_then(|current| self.joined_rooms.iter().position(|&id| id == current))
            .unwrap_or(0);

        let tabs = Tabs::new(room_titles)
            .block(Block::default().borders(Borders::ALL).title("Rooms (Tab to switch)"))
            .style(Style::default().fg(Color::White))
            .highlight_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .select(selected_tab);
        f.render_widget(tabs, left_chunks[1]);

        // Chat area
        self.draw_chat_area(f, left_chunks[2]);

        // Input field with cursor
        let input_text = if self.cursor_visible {
            let mut text = self.input.clone();
            if self.cursor_position <= text.len() {
                text.insert(self.cursor_position, '│');
            }
            text
        } else {
            self.input.clone()
        };

        let input_title = if let Some(room_id) = self.current_room {
            format!("Message (Room {})", room_id)
        } else {
            "Message (join a room first)".to_string()
        };

        let input = Paragraph::new(input_text.as_str())
            .style(Style::default().fg(Color::White))
            .block(Block::default().borders(Borders::ALL).title(input_title));
        f.render_widget(input, left_chunks[3]);

        // Improved status bar
        let voice_controls = if self.voice_connected {
            format!("F5-Disconnect F6-Mute({}) F7-Deafen({})", 
                if self.voice_muted { "ON" } else { "off" },
                if self.voice_deafened { "ON" } else { "off" })
        } else {
            "F5-Voice".to_string()
        };

        let status_spans = vec![
            Span::styled("F1", Style::default().fg(Color::Yellow)),
            Span::raw("-Help "),
            Span::styled("F2", Style::default().fg(Color::Yellow)),
            Span::raw("-Rooms "),
            Span::styled("F3", Style::default().fg(Color::Yellow)),
            Span::raw("-DM "),
            Span::styled("F4", Style::default().fg(Color::Yellow)),
            Span::raw("-Settings "),
            Span::styled(&voice_controls, Style::default().fg(if self.voice_connected { Color::Green } else { Color::Gray })),
            Span::raw(" "),
            Span::styled("ESC", Style::default().fg(Color::Red)),
            Span::raw("-Quit"),
        ];
        let status = Paragraph::new(Line::from(status_spans))
            .style(Style::default().fg(Color::Gray))
            .alignment(Alignment::Center);
        f.render_widget(status, left_chunks[4]);

        // User panel on the right
        self.draw_user_panel(f, main_chunks[1]);

        // Draw notifications overlay
        self.draw_notifications(f);

        // Popups (render last so they're on top)
        if self.show_help {
            self.draw_help_popup(f);
        }
        if self.show_room_browser {
            self.draw_room_browser(f);
        }
        if self.show_settings {
            self.draw_settings(f);
        }
        if self.show_private_messages {
            self.draw_private_messages(f);
        }
    }

    fn draw_user_panel(&self, f: &mut Frame, area: Rect) {
        let online_count = self.online_users.iter().filter(|u| u.is_online).count();
        let title = format!("Users ({} online)", online_count);
        
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::Green));

        if self.online_users.is_empty() {
            let empty_text = Paragraph::new("No users yet\n\nPress F3 for\nprivate messages")
                .style(Style::default().fg(Color::Gray))
                .alignment(Alignment::Center)
                .block(block);
            f.render_widget(empty_text, area);
        } else {
            // Sort users: online first, then alphabetically
            let mut sorted_users = self.online_users.clone();
            sorted_users.sort_by(|a, b| {
                match (a.is_online, b.is_online) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => a.username.to_lowercase().cmp(&b.username.to_lowercase()),
                }
            });
            
            let user_items: Vec<ListItem> = sorted_users
                .iter()
                .map(|user| {
                    if user.is_online {
                        // Online user - green dot, white name
                        ListItem::new(Line::from(vec![
                            Span::styled("● ", Style::default().fg(Color::Green)),
                            Span::styled(&user.username, Style::default().fg(Color::White)),
                        ]))
                    } else {
                        // Offline user - gray dot, gray name with last seen
                        let last_seen = if user.last_seen.len() > 10 {
                            // Show just time portion if today, or date
                            &user.last_seen[11..16] // HH:MM
                        } else {
                            &user.last_seen
                        };
                        ListItem::new(Line::from(vec![
                            Span::styled("○ ", Style::default().fg(Color::DarkGray)),
                            Span::styled(&user.username, Style::default().fg(Color::DarkGray)),
                            Span::styled(format!(" ({})", last_seen), Style::default().fg(Color::DarkGray)),
                        ]))
                    }
                })
                .collect();
            f.render_widget(List::new(user_items).block(block), area);
        }
    }

    fn draw_chat_area(&self, f: &mut Frame, area: Rect) {
        if let Some(room_id) = self.current_room {
            if let Some(room) = self.rooms.get(&room_id) {
                let block = Block::default()
                    .borders(Borders::ALL)
                    .title(format!("Room: {}", room.name));
                let messages: Vec<ListItem> = room
                    .messages
                    .iter()
                    .map(|msg| {
                        let style = match msg.message_type {
                            ChatMessageType::User => Style::default().fg(Color::White),
                            ChatMessageType::System => Style::default().fg(Color::Green),
                            ChatMessageType::Private => Style::default().fg(Color::Magenta),
                            ChatMessageType::Error => Style::default().fg(Color::Red),
                        };
                        ListItem::new(format!(
                            "[{}] {}: {}",
                            msg.timestamp, msg.sender, msg.content
                        ))
                        .style(style)
                    })
                    .collect();
                f.render_widget(List::new(messages).block(block), area);
            }
        } else {
            let block = Block::default()
                .borders(Borders::ALL)
                .title("No Room Selected");
            let empty = Paragraph::new("Join a room to start chatting!")
                .style(Style::default().fg(Color::Gray))
                .alignment(Alignment::Center)
                .block(block);
            f.render_widget(empty, area);
        }
    }

    fn draw_help_popup(&self, f: &mut Frame) {
        let popup_area = Self::centered_rect_fixed(65, 24, f.area());
        f.render_widget(Clear, popup_area);

        let help_text = vec![
            Line::from(vec![Span::styled(
                "FCA Chat - Help",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )]),
            Line::from(""),
            Line::from(vec![Span::styled("Navigation:", Style::default().fg(Color::Yellow))]),
            Line::from("  Tab         Switch between rooms"),
            Line::from("  ↑/↓         Scroll messages / Navigate lists"),
            Line::from("  PgUp/PgDn   Scroll messages faster"),
            Line::from("  Home/End    Jump to start/end of input"),
            Line::from(""),
            Line::from(vec![Span::styled("Function Keys:", Style::default().fg(Color::Yellow))]),
            Line::from("  F1          Toggle this help"),
            Line::from("  F2          Room browser"),
            Line::from("  F3          Private messages"),
            Line::from("  F4          Settings (audio devices, voice)"),
            Line::from("  F5          Toggle voice chat"),
            Line::from("  F6          Toggle mute (when in voice)"),
            Line::from("  F7          Toggle deafen (when in voice)"),
            Line::from("  Esc         Close dialogs or quit"),
            Line::from(""),
            Line::from(vec![Span::styled("Chat Commands:", Style::default().fg(Color::Yellow))]),
            Line::from("  /join <id> [password]          Join a room"),
            Line::from("  /create <name> <desc> [pass]   Create a room"),
            Line::from("  /leave <id>                    Leave a room"),
        ];

        let help = Paragraph::new(help_text)
            .block(Block::default()
                .title("Help")
                .borders(Borders::ALL)
                .style(Style::default().fg(Color::Cyan)))
            .wrap(Wrap { trim: true });
        f.render_widget(help, popup_area);
    }

    fn draw_room_browser(&self, f: &mut Frame) {
        let popup_area = Self::centered_rect_fixed(70, 20, f.area());
        f.render_widget(Clear, popup_area);

        let block = Block::default()
            .title("Room Browser")
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::Cyan));

        if self.available_rooms.is_empty() {
            let content = Paragraph::new("Loading rooms...\n\nPress Esc to close")
                .block(block)
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true });
            f.render_widget(content, popup_area);
        } else {
            let inner = popup_area.inner(ratatui::layout::Margin {
                horizontal: 2,
                vertical: 2,
            });
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(3)])
                .split(inner);

            let room_items: Vec<ListItem> = self
                .available_rooms
                .iter()
                .enumerate()
                .map(|(i, room)| {
                    let style = if i == self.selected_room_index {
                        Style::default()
                            .bg(Color::White)
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    let lock_icon = if room.is_password_protected {
                        "[LOCKED] "
                    } else {
                        ""
                    };
                    ListItem::new(format!(
                        "  [{}] {}{} - {}",
                        room.id, lock_icon, room.name, room.description
                    ))
                    .style(style)
                })
                .collect();

            f.render_widget(
                List::new(room_items).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Available Rooms"),
                ),
                chunks[0],
            );

            let instructions = vec![
                Line::from("Up/Down: Navigate | Enter: Join Room | Esc: Close"),
                Line::from("[LOCKED] = Password Protected Room"),
            ];
            f.render_widget(
                Paragraph::new(instructions)
                    .block(Block::default().borders(Borders::ALL).title("Controls"))
                    .alignment(Alignment::Center),
                chunks[1],
            );

            f.render_widget(block, popup_area);
        }
    }

    fn draw_private_messages(&self, f: &mut Frame) {
        let popup_area = Self::centered_rect_fixed(80, 25, f.area());
        f.render_widget(Clear, popup_area);

        let block = Block::default()
            .title("Private Messages")
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::Magenta));
        let inner = popup_area.inner(ratatui::layout::Margin {
            horizontal: 2,
            vertical: 2,
        });
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
            .split(inner);

        let user_items: Vec<ListItem> = self
            .online_users
            .iter()
            .filter(|user| user.user_id != self.user_id)
            .map(|user| {
                let style = if Some(user.user_id) == self.selected_private_user {
                    Style::default()
                        .bg(Color::White)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD)
                } else if user.is_online {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::Gray)
                };
                ListItem::new(format!(
                    "{} {}",
                    if user.is_online { "●" } else { "○" },
                    user.username
                ))
                .style(style)
            })
            .collect();

        f.render_widget(
            List::new(user_items).block(Block::default().borders(Borders::ALL).title("Users")),
            chunks[0],
        );

        if let Some(selected_user) = self.selected_private_user {
            if let Some(messages) = self.private_conversations.get(&selected_user) {
                let message_items: Vec<ListItem> = messages
                    .iter()
                    .map(|msg| {
                        ListItem::new(format!(
                            "[{}] {}: {}",
                            msg.timestamp, msg.sender, msg.content
                        ))
                        .style(Style::default().fg(Color::White))
                    })
                    .collect();
                f.render_widget(
                    List::new(message_items)
                        .block(Block::default().borders(Borders::ALL).title("Messages")),
                    chunks[1],
                );
            } else {
                f.render_widget(
                    Paragraph::new("No messages yet.")
                        .block(Block::default().borders(Borders::ALL).title("Messages"))
                        .alignment(Alignment::Center),
                    chunks[1],
                );
            }
        } else {
            f.render_widget(
                Paragraph::new("Select a user...")
                    .block(Block::default().borders(Borders::ALL).title("Instructions"))
                    .alignment(Alignment::Center),
                chunks[1],
            );
        }
        f.render_widget(block, popup_area);
    }

    fn draw_settings(&self, f: &mut Frame) {
        let popup_area = Self::centered_rect_fixed(70, 22, f.area());
        f.render_widget(Clear, popup_area);

        let block = Block::default()
            .title("Settings")
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::Cyan));
        f.render_widget(block, popup_area);

        let inner = popup_area.inner(ratatui::layout::Margin {
            horizontal: 2,
            vertical: 1,
        });

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),  // Tabs
                Constraint::Min(10),    // Content
                Constraint::Length(3),  // Controls
            ])
            .split(inner);

        // Tab bar
        let tab_titles = vec!["Audio", "Voice", "Interface"];
        let selected_tab = match self.settings_tab {
            SettingsTab::Audio => 0,
            SettingsTab::Voice => 1,
            SettingsTab::Interface => 2,
        };
        let tabs = Tabs::new(tab_titles)
            .block(Block::default().borders(Borders::BOTTOM))
            .style(Style::default().fg(Color::White))
            .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            .select(selected_tab);
        f.render_widget(tabs, chunks[0]);

        // Content based on selected tab
        match self.settings_tab {
            SettingsTab::Audio => self.draw_audio_settings(f, chunks[1]),
            SettingsTab::Voice => self.draw_voice_settings(f, chunks[1]),
            SettingsTab::Interface => self.draw_interface_settings(f, chunks[1]),
        }

        // Controls
        let controls = Paragraph::new("Tab: Switch tabs | ↑↓: Navigate | ←→: Adjust | Esc: Close")
            .style(Style::default().fg(Color::Gray))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::TOP));
        f.render_widget(controls, chunks[2]);
    }

    fn draw_audio_settings(&self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(3),  // Input device
                Constraint::Length(3),  // Output device
                Constraint::Length(3),  // Input volume
                Constraint::Length(3),  // Output volume
                Constraint::Min(0),     // Spacer
            ])
            .split(area);

        // Input device selector
        let input_device_name = self.selected_input_device().unwrap_or("No device");
        let input_style = if self.settings_field == 0 {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let input_device = Paragraph::new(format!("◄ {} ►", input_device_name))
            .alignment(Alignment::Center)
            .block(Block::default()
                .title("Input Device")
                .borders(Borders::ALL)
                .style(input_style));
        f.render_widget(input_device, chunks[0]);

        // Output device selector
        let output_device_name = self.selected_output_device().unwrap_or("No device");
        let output_style = if self.settings_field == 1 {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let output_device = Paragraph::new(format!("◄ {} ►", output_device_name))
            .alignment(Alignment::Center)
            .block(Block::default()
                .title("Output Device")
                .borders(Borders::ALL)
                .style(output_style));
        f.render_widget(output_device, chunks[1]);

        // Input volume
        let input_vol_style = if self.settings_field == 2 {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Green)
        };
        let input_vol = Gauge::default()
            .block(Block::default()
                .title("Input Volume")
                .borders(Borders::ALL)
                .style(if self.settings_field == 2 {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default()
                }))
            .gauge_style(input_vol_style)
            .percent(self.audio_settings.input_volume as u16)
            .label(format!("{}%", self.audio_settings.input_volume));
        f.render_widget(input_vol, chunks[2]);

        // Output volume
        let output_vol_style = if self.settings_field == 3 {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Blue)
        };
        let output_vol = Gauge::default()
            .block(Block::default()
                .title("Output Volume")
                .borders(Borders::ALL)
                .style(if self.settings_field == 3 {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default()
                }))
            .gauge_style(output_vol_style)
            .percent(self.audio_settings.output_volume as u16)
            .label(format!("{}%", self.audio_settings.output_volume));
        f.render_widget(output_vol, chunks[3]);
    }

    fn draw_voice_settings(&self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(0),
            ])
            .split(area);

        let mute_status = if self.voice_muted { "ON " } else { "OFF" };
        let mute_style = if self.settings_field == 0 {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let mute_toggle = Paragraph::new(format!("[ {} ] (F6 to toggle)", mute_status))
            .alignment(Alignment::Center)
            .block(Block::default()
                .title("Mute Microphone")
                .borders(Borders::ALL)
                .style(mute_style));
        f.render_widget(mute_toggle, chunks[0]);

        let deafen_status = if self.voice_deafened { "ON " } else { "OFF" };
        let deafen_style = if self.settings_field == 1 {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let deafen_toggle = Paragraph::new(format!("[ {} ] (F7 to toggle)", deafen_status))
            .alignment(Alignment::Center)
            .block(Block::default()
                .title("Deafen (Mute Speakers)")
                .borders(Borders::ALL)
                .style(deafen_style));
        f.render_widget(deafen_toggle, chunks[1]);
    }

    fn draw_interface_settings(&self, f: &mut Frame, area: Rect) {
        let content = Paragraph::new(vec![
            Line::from(""),
            Line::from("Interface customization coming soon!"),
            Line::from(""),
            Line::from("Planned features:"),
            Line::from("  • Color themes"),
            Line::from("  • Timestamp format"),
            Line::from("  • Message display options"),
        ])
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::Gray))
        .block(Block::default().borders(Borders::ALL));
        f.render_widget(content, area);
    }

    fn draw_notifications(&self, f: &mut Frame) {
        if self.notifications.is_empty() {
            return;
        }

        let area = f.area();
        let notification_height = self.notifications.len().min(5) as u16;
        let notification_area = Rect {
            x: area.width.saturating_sub(42),
            y: 1,
            width: 40,
            height: notification_height * 2 + 1,
        };

        let items: Vec<ListItem> = self.notifications
            .iter()
            .rev()
            .take(5)
            .map(|n| {
                let (prefix, color) = match n.notification_type {
                    NotificationType::Success => ("[OK]", Color::Green),
                    NotificationType::Info => ("[i]", Color::Cyan),
                    NotificationType::Warning => ("[!]", Color::Yellow),
                    NotificationType::Error => ("[X]", Color::Red),
                };
                ListItem::new(format!(" {} {}", prefix, n.message))
                    .style(Style::default().fg(color))
            })
            .collect();

        if !items.is_empty() {
            let block = Block::default()
                .borders(Borders::ALL)
                .style(Style::default().fg(Color::DarkGray));
            f.render_widget(Clear, notification_area);
            f.render_widget(List::new(items).block(block), notification_area);
        }
    }

    fn draw_voice_indicator(&self, f: &mut Frame, area: Rect) {
        if !self.voice_connected {
            return;
        }

        let voice_status = if self.voice_muted {
            ("MUTED", Color::Red)
        } else if self.voice_deafened {
            ("DEAF", Color::Red)
        } else {
            ("LIVE", Color::Green)
        };

        let indicator = Paragraph::new(voice_status.0)
            .style(Style::default().fg(voice_status.1).add_modifier(Modifier::BOLD))
            .alignment(Alignment::Right);
        f.render_widget(indicator, area);
    }

    fn centered_rect_fixed(width: u16, height: u16, r: Rect) -> Rect {
        let popup_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length((r.height.saturating_sub(height)) / 2),
                Constraint::Length(height),
                Constraint::Min(0),
            ])
            .split(r);

        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length((r.width.saturating_sub(width)) / 2),
                Constraint::Length(width),
                Constraint::Min(0),
            ])
            .split(popup_layout[1])[1]
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
