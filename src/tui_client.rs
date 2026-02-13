//! TUI Chat Client for RustyRoom
//!
//! This module provides a terminal-based user interface for the chat application
//! using the `ratatui` library. It supports real-time messaging, server/channel
//! management, voice chat with configurable audio devices, private messaging,
//! and user presence.
//!
//! The client runs in a terminal alternate screen and handles keyboard input
//! for navigation, text entry, and command execution.

use crate::audio::{self, AudioDeviceInfo};
use crate::resc::{ChannelInfo, ChannelType, Message, MessageType, ServerInfo, ServerWithChannels, UserStatus};
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
    widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Tabs, Wrap},
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

// Thin wrappers around libc fd operations for stderr redirection.
// These are used to redirect stderr to /dev/null while the TUI is active,
// preventing C libraries (ALSA, PulseAudio) from corrupting the display.
#[cfg(unix)]
unsafe fn nix_dup(fd: i32) -> Result<i32, ()> {
    unsafe extern "C" {
        fn dup(fd: i32) -> i32;
    }
    let r = unsafe { dup(fd) };
    if r < 0 { Err(()) } else { Ok(r) }
}

#[cfg(unix)]
unsafe fn nix_dup2(oldfd: i32, newfd: i32) {
    unsafe extern "C" {
        fn dup2(oldfd: i32, newfd: i32) -> i32;
    }
    unsafe {
        dup2(oldfd, newfd);
    }
}

#[cfg(unix)]
unsafe fn nix_close(fd: i32) {
    unsafe extern "C" {
        fn close(fd: i32) -> i32;
    }
    unsafe {
        close(fd);
    }
}

// ============================================================================
// NOTIFICATION SYSTEM
// ============================================================================

/// Types of toast notifications displayed in the UI.
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
#[derive(Debug, Clone)]
pub struct Notification {
    pub message: String,
    pub notification_type: NotificationType,
    pub created_at: Instant,
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

// ============================================================================
// UI UPDATE MESSAGES
// ============================================================================

/// UI state update messages received from background threads.
#[derive(Debug, Clone)]
pub enum UIUpdate {
    AuthSuccess { user_id: u16 },
    AuthFailure { reason: String },
    ServerCreated { server_id: u16, name: String, description: String },
    ServerJoined {
        server_id: u16,
        name: String,
        description: String,
        channels: Vec<ChannelInfo>,
    },
    ServerLeft { server_id: u16 },
    ChannelCreated {
        server_id: u16,
        channel_id: u16,
        name: String,
        description: String,
        channel_type: ChannelType,
    },
    ChannelJoined {
        server_id: u16,
        channel_id: u16,
        name: String,
        description: String,
        channel_type: ChannelType,
    },
    ChannelMessage { channel_id: u16, sender: String, content: String },
    ServerMessage { content: String, is_error: bool },
    UserListUpdate { users: Vec<UserStatus> },
    ServerList { servers: Vec<ServerInfo> },
    ChannelList { server_id: u16, channels: Vec<ChannelInfo> },
    ServersSync { servers: Vec<ServerWithChannels> },
    PrivateMessage { from_user_id: u16, sender: String, content: String },
    VoiceStatus { connected: bool, message: String },
    VoiceLevel { level: f32 },
    ServerMemberList { server_id: u16, members: Vec<UserStatus> },
}

// ============================================================================
// CHAT MESSAGE TYPES
// ============================================================================

/// Types of chat messages for styling and display purposes.
#[derive(Debug, Clone)]
pub enum ChatMessageType {
    User,
    System,
    Private,
    Error,
}

/// A single chat message displayed in the message list.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub timestamp: String,
    pub sender: String,
    pub content: String,
    pub message_type: ChatMessageType,
}

// ============================================================================
// SERVER DATA (CLIENT-SIDE)
// ============================================================================

/// A server (guild) with its channels and message history.
#[derive(Debug, Clone)]
pub struct ServerData {
    pub id: u16,
    pub name: String,
    pub description: String,
    pub channels: Vec<ChannelInfo>,
    pub channel_messages: HashMap<u16, Vec<ChatMessage>>,
}

// ============================================================================
// UI STATE
// ============================================================================

/// Tab selection for the settings screen.
#[derive(Debug, Clone, PartialEq)]
enum SettingsTab {
    Audio,
    Voice,
    Interface,
}

/// Audio device configuration and volume settings.
#[derive(Debug)]
struct AudioSettings {
    input_devices: Vec<AudioDeviceInfo>,
    output_devices: Vec<AudioDeviceInfo>,
    selected_input_index: usize,
    selected_output_index: usize,
    input_volume: u8,
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

/// Authentication screen mode selection.
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

// ============================================================================
// TUI CHAT CLIENT
// ============================================================================

/// The main TUI chat client application.
///
/// Manages all client state including authentication, server/channel membership,
/// message history, voice chat, and UI components.
#[derive(Debug)]
pub struct TuiChatClient {
    server_address: String,

    // Authentication state
    authenticated: bool,
    user_id: u16,
    username: String,

    // Chat state
    input: String,
    current_server: Option<u16>,
    current_channel: Option<u16>,
    servers: HashMap<u16, ServerData>,

    // UI state
    show_help: bool,
    show_server_browser: bool,
    show_settings: bool,
    show_private_messages: bool,
    cursor_position: usize,

    // Server browser state
    available_servers: Vec<ServerInfo>,
    selected_server_index: usize,

    // Sidebar channel selection index
    sidebar_channel_index: usize,

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
    voice_channel_id: Option<u16>,

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

impl TuiChatClient {
    /// Creates a new TUI client instance.
    pub fn new(server_address: String) -> Self {
        audio::suppress_alsa_errors();

        let input_devices = audio::list_input_devices();
        let output_devices = audio::list_output_devices();

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
            current_server: None,
            current_channel: None,
            servers: HashMap::new(),
            show_help: false,
            show_server_browser: false,
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
            available_servers: Vec::new(),
            selected_server_index: 0,
            sidebar_channel_index: 0,
            private_conversations: HashMap::new(),
            selected_private_user: None,
            selected_user_index: 0,
            voice_connected: false,
            voice_muted: false,
            voice_deafened: false,
            voice_level: 0.0,
            voice_stop_signal: None,
            voice_channel_id: None,
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

    // ========================================================================
    // HELPER METHODS
    // ========================================================================

    fn add_notification(&mut self, message: String, notification_type: NotificationType) {
        self.notifications
            .push(Notification::new(message, notification_type));
        if self.notifications.len() > 5 {
            self.notifications.remove(0);
        }
    }

    fn refresh_audio_devices(&mut self) {
        self.audio_settings.input_devices = audio::list_input_devices();
        self.audio_settings.output_devices = audio::list_output_devices();
        if self.audio_settings.selected_input_index >= self.audio_settings.input_devices.len() {
            self.audio_settings.selected_input_index = 0;
        }
        if self.audio_settings.selected_output_index >= self.audio_settings.output_devices.len() {
            self.audio_settings.selected_output_index = 0;
        }
    }

    fn selected_input_device(&self) -> Option<&str> {
        self.audio_settings
            .input_devices
            .get(self.audio_settings.selected_input_index)
            .map(|d| d.name.as_str())
    }

    fn selected_output_device(&self) -> Option<&str> {
        self.audio_settings
            .output_devices
            .get(self.audio_settings.selected_output_index)
            .map(|d| d.name.as_str())
    }

    fn joined_server_ids(&self) -> Vec<u16> {
        let mut ids: Vec<u16> = self.servers.keys().copied().collect();
        ids.sort();
        ids
    }

    fn current_time() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let secs = now.as_secs();
        let hours = (secs / 3600) % 24;
        let minutes = (secs / 60) % 60;
        format!("{:02}:{:02}", hours, minutes)
    }

    fn add_system_message(&mut self, content: String, msg_type: ChatMessageType) {
        if let Some(channel_id) = self.current_channel {
            if let Some(server_id) = self.current_server {
                if let Some(server) = self.servers.get_mut(&server_id) {
                    let messages = server
                        .channel_messages
                        .entry(channel_id)
                        .or_insert_with(Vec::new);
                    messages.push(ChatMessage {
                        timestamp: Self::current_time(),
                        sender: "System".to_string(),
                        content,
                        message_type: msg_type,
                    });
                }
            }
        }
    }

    // ========================================================================
    // APPLICATION LIFECYCLE
    // ========================================================================

    /// Starts the TUI application.
    ///
    /// Sets up the terminal in raw mode with alternate screen, creates the
    /// client instance, runs the main event loop, and restores terminal state
    /// on exit. Stderr is redirected to /dev/null to prevent ALSA/PulseAudio
    /// messages from corrupting the display.
    pub async fn start_tui(server_address: &str) -> io::Result<()> {
        audio::suppress_alsa_errors();

        #[cfg(unix)]
        let saved_stderr = {
            use std::fs::File;
            use std::os::unix::io::AsRawFd;

            let saved = unsafe { nix_dup(2) };
            if let Ok(devnull) = File::open("/dev/null") {
                unsafe {
                    nix_dup2(devnull.as_raw_fd(), 2);
                }
            }
            saved
        };

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

        #[cfg(unix)]
        {
            if let Ok(fd) = saved_stderr {
                unsafe {
                    nix_dup2(fd, 2);
                    nix_close(fd);
                }
            }
        }

        result
    }

    /// Main event loop.
    async fn run(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<()> {
        self.connect().await?;

        let mut last_tick = Instant::now();
        let mut last_refresh = Instant::now();
        let tick_rate = Duration::from_millis(50);
        let refresh_rate = Duration::from_secs(10);

        loop {
            // Process UI updates from background threads
            let mut updates = Vec::new();
            if let Some(ui_receiver) = &mut self.ui_receiver {
                while let Ok(update) = ui_receiver.try_recv() {
                    updates.push(update);
                }
            }
            for update in updates {
                self.handle_ui_update(update);
            }

            // Clean up expired notifications
            self.notifications.retain(|n| !n.is_expired());

            // Draw the UI
            terminal.draw(|f| self.draw(f))?;

            // Cursor blink
            if last_tick.elapsed() >= Duration::from_millis(500) {
                self.cursor_visible = !self.cursor_visible;
                self.last_cursor_toggle = Instant::now();
            }

            // Poll for input
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

            // Periodically refresh the user list
            if self.authenticated && last_refresh.elapsed() >= refresh_rate {
                if let Some(sender) = &self.message_sender {
                    let _ = sender.send(Message::new(self.user_id, MessageType::RequestUserList));
                }
                last_refresh = Instant::now();
            }
        }

        Ok(())
    }

    // ========================================================================
    // UI UPDATE HANDLER
    // ========================================================================

    fn handle_ui_update(&mut self, update: UIUpdate) {
        match update {
            UIUpdate::AuthSuccess { user_id } => {
                self.authenticated = true;
                self.user_id = user_id;
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
            UIUpdate::ServerList { servers } => {
                self.available_servers = servers;
                self.selected_server_index = 0;
            }
            UIUpdate::ServerCreated {
                server_id,
                name,
                description: _,
            } => {
                self.add_notification(
                    format!("Server '{}' created (ID: {})", name, server_id),
                    NotificationType::Success,
                );
            }
            UIUpdate::ServerJoined {
                server_id,
                name,
                description,
                channels,
            } => {
                let mut channel_messages = HashMap::new();
                for ch in &channels {
                    channel_messages.insert(ch.id, Vec::new());
                }

                let server_data = ServerData {
                    id: server_id,
                    name: name.clone(),
                    description,
                    channels: channels.clone(),
                    channel_messages,
                };
                self.servers.insert(server_id, server_data);

                // Auto-select this server if none selected
                if self.current_server.is_none() {
                    self.current_server = Some(server_id);
                    self.sidebar_channel_index = 0;
                    if let Some(first_text) =
                        channels.iter().find(|c| c.channel_type == ChannelType::Text)
                    {
                        self.current_channel = Some(first_text.id);
                        if let Some(sender) = &self.message_sender {
                            let _ = sender.send(Message::new(
                                self.user_id,
                                MessageType::JoinChannel {
                                    channel_id: first_text.id,
                                    password: None,
                                },
                            ));
                        }
                    }
                }

                self.add_notification(
                    format!("Joined server '{}'", name),
                    NotificationType::Success,
                );
            }
            UIUpdate::ServerLeft { server_id } => {
                if let Some(server) = self.servers.remove(&server_id) {
                    self.add_notification(
                        format!("Left server '{}'", server.name),
                        NotificationType::Info,
                    );
                }
                if self.current_server == Some(server_id) {
                    self.current_server = self.servers.keys().next().copied();
                    self.current_channel = None;
                    self.sidebar_channel_index = 0;
                    if let Some(sid) = self.current_server {
                        if let Some(server) = self.servers.get(&sid) {
                            if let Some(ch) = server
                                .channels
                                .iter()
                                .find(|c| c.channel_type == ChannelType::Text)
                            {
                                self.current_channel = Some(ch.id);
                            }
                        }
                    }
                }
            }
            UIUpdate::ChannelCreated {
                server_id,
                channel_id,
                name,
                description,
                channel_type,
            } => {
                if let Some(server) = self.servers.get_mut(&server_id) {
                    server.channels.push(ChannelInfo {
                        id: channel_id,
                        server_id,
                        name: name.clone(),
                        description,
                        channel_type,
                        is_password_protected: false,
                    });
                    server.channel_messages.insert(channel_id, Vec::new());
                }
                self.add_notification(
                    format!("Channel '{}' created", name),
                    NotificationType::Success,
                );
            }
            UIUpdate::ChannelJoined {
                server_id,
                channel_id,
                name,
                description,
                channel_type,
            } => {
                if let Some(server) = self.servers.get_mut(&server_id) {
                    if !server.channels.iter().any(|c| c.id == channel_id) {
                        server.channels.push(ChannelInfo {
                            id: channel_id,
                            server_id,
                            name,
                            description,
                            channel_type,
                            is_password_protected: false,
                        });
                    }
                    server
                        .channel_messages
                        .entry(channel_id)
                        .or_insert_with(Vec::new);
                }
            }
            UIUpdate::ChannelMessage {
                channel_id,
                sender,
                content,
            } => {
                for server in self.servers.values_mut() {
                    if server.channels.iter().any(|c| c.id == channel_id) {
                        let messages = server
                            .channel_messages
                            .entry(channel_id)
                            .or_insert_with(Vec::new);
                        messages.push(ChatMessage {
                            timestamp: Self::current_time(),
                            sender,
                            content,
                            message_type: ChatMessageType::User,
                        });
                        break;
                    }
                }
            }
            UIUpdate::ChannelList {
                server_id,
                channels,
            } => {
                if let Some(server) = self.servers.get_mut(&server_id) {
                    for ch in &channels {
                        server
                            .channel_messages
                            .entry(ch.id)
                            .or_insert_with(Vec::new);
                    }
                    server.channels = channels;
                }
            }
            UIUpdate::ServersSync { servers } => {
                for swc in servers {
                    let mut channel_messages = HashMap::new();
                    for ch in &swc.channels {
                        channel_messages.insert(ch.id, Vec::new());
                    }
                    let server_data = ServerData {
                        id: swc.server.id,
                        name: swc.server.name.clone(),
                        description: swc.server.description.clone(),
                        channels: swc.channels.clone(),
                        channel_messages,
                    };
                    self.servers.insert(swc.server.id, server_data);
                }
                // Auto-select first server if none selected
                if self.current_server.is_none() {
                    if let Some(&first_id) = self.servers.keys().next() {
                        self.current_server = Some(first_id);
                        self.sidebar_channel_index = 0;
                        if let Some(server) = self.servers.get(&first_id) {
                            if let Some(ch) = server
                                .channels
                                .iter()
                                .find(|c| c.channel_type == ChannelType::Text)
                            {
                                self.current_channel = Some(ch.id);
                                if let Some(sender) = &self.message_sender {
                                    let _ = sender.send(Message::new(
                                        self.user_id,
                                        MessageType::JoinChannel {
                                            channel_id: ch.id,
                                            password: None,
                                        },
                                    ));
                                }
                            }
                        }
                    }
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
                if is_error {
                    self.add_notification(content.clone(), NotificationType::Error);
                } else {
                    self.add_notification(content.clone(), NotificationType::Info);
                }
                let msg_type = if is_error {
                    ChatMessageType::Error
                } else {
                    ChatMessageType::System
                };
                self.add_system_message(content, msg_type);
            }
            UIUpdate::VoiceStatus { connected, message } => {
                let was_connected = self.voice_connected;
                self.voice_connected = connected;
                if connected && !was_connected {
                    self.add_notification(
                        "Voice connected!".to_string(),
                        NotificationType::Success,
                    );
                } else if !connected && was_connected {
                    self.add_notification(
                        "Voice disconnected".to_string(),
                        NotificationType::Info,
                    );
                    self.voice_channel_id = None;
                }
                self.add_system_message(message, ChatMessageType::System);
            }
            UIUpdate::VoiceLevel { level } => {
                self.voice_level = level;
            }
            UIUpdate::ServerMemberList {
                server_id: _,
                members,
            } => {
                self.online_users = members;
            }
        }
    }

    // ========================================================================
    // NETWORKING
    // ========================================================================

    /// Establishes TCP connection to the server.
    async fn connect(&mut self) -> io::Result<()> {
        let stream = TcpStream::connect(&self.server_address)?;
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(120)));

        let read_stream = stream.try_clone()?;
        let write_stream = stream.try_clone()?;

        let (tx, mut rx) = mpsc::unbounded_channel();
        self.message_sender = Some(tx);

        let (ui_tx, ui_rx) = mpsc::unbounded_channel();
        self.ui_receiver = Some(ui_rx);

        let server_address = self.server_address.clone();

        // Reader thread
        thread::spawn(move || {
            Self::message_receiver(read_stream, ui_tx, server_address);
        });

        // Writer task
        tokio::spawn(async move {
            let mut stream = write_stream;
            while let Some(message) = rx.recv().await {
                if let Ok(json) = message.to_json_file() {
                    let _ = writeln!(stream, "{}", json);
                    let _ = stream.flush();
                }
            }
        });

        // Request initial user list
        if let Some(sender) = &self.message_sender {
            let _ = sender.send(Message::new(0, MessageType::RequestUserList));
        }

        Ok(())
    }

    /// Reads messages from the server and sends UI updates.
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
                            MessageType::AuthSuccess { user_id, message: _ } => {
                                let _ =
                                    ui_sender.send(UIUpdate::AuthSuccess { user_id: *user_id });
                            }
                            MessageType::AuthFailure { reason } => {
                                let _ = ui_sender.send(UIUpdate::AuthFailure {
                                    reason: reason.clone(),
                                });
                            }
                            MessageType::ServerCreated {
                                server_id,
                                name,
                                description,
                            } => {
                                let _ = ui_sender.send(UIUpdate::ServerCreated {
                                    server_id: *server_id,
                                    name: name.clone(),
                                    description: description.clone(),
                                });
                            }
                            MessageType::ServerJoined {
                                server_id,
                                name,
                                description,
                                channels,
                            } => {
                                let _ = ui_sender.send(UIUpdate::ServerJoined {
                                    server_id: *server_id,
                                    name: name.clone(),
                                    description: description.clone(),
                                    channels: channels.clone(),
                                });
                            }
                            MessageType::ServerLeft { server_id } => {
                                let _ = ui_sender
                                    .send(UIUpdate::ServerLeft { server_id: *server_id });
                            }
                            MessageType::ChannelCreated {
                                server_id,
                                channel_id,
                                name,
                                description,
                                channel_type,
                            } => {
                                let _ = ui_sender.send(UIUpdate::ChannelCreated {
                                    server_id: *server_id,
                                    channel_id: *channel_id,
                                    name: name.clone(),
                                    description: description.clone(),
                                    channel_type: channel_type.clone(),
                                });
                            }
                            MessageType::ChannelJoined {
                                server_id,
                                channel_id,
                                name,
                                description,
                                channel_type,
                            } => {
                                let _ = ui_sender.send(UIUpdate::ChannelJoined {
                                    server_id: *server_id,
                                    channel_id: *channel_id,
                                    name: name.clone(),
                                    description: description.clone(),
                                    channel_type: channel_type.clone(),
                                });
                            }
                            MessageType::ChannelMessage {
                                channel_id,
                                sender_username,
                                content,
                            } => {
                                let _ = ui_sender.send(UIUpdate::ChannelMessage {
                                    channel_id: *channel_id,
                                    sender: sender_username.clone(),
                                    content: content.clone(),
                                });
                            }
                            MessageType::UserListUpdate { users } => {
                                let _ = ui_sender.send(UIUpdate::UserListUpdate {
                                    users: users.clone(),
                                });
                            }
                            MessageType::ServerList { servers } => {
                                let _ = ui_sender.send(UIUpdate::ServerList {
                                    servers: servers.clone(),
                                });
                            }
                            MessageType::ChannelList {
                                server_id,
                                channels,
                            } => {
                                let _ = ui_sender.send(UIUpdate::ChannelList {
                                    server_id: *server_id,
                                    channels: channels.clone(),
                                });
                            }
                            MessageType::UserServersSync { servers } => {
                                let _ = ui_sender.send(UIUpdate::ServersSync {
                                    servers: servers.clone(),
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
                            MessageType::VoiceCredentials {
                                token,
                                udp_port,
                                channel_id,
                            } => {
                                let ip = server_ip.clone();
                                let tok = token.clone();
                                let port = *udp_port;
                                let sender = ui_sender.clone();

                                let _ = ui_sender.send(UIUpdate::ServerMessage {
                                    content: format!(
                                        "Connecting to voice channel {}...",
                                        channel_id
                                    ),
                                    is_error: false,
                                });

                                thread::spawn(move || {
                                    Self::start_udp_voice(ip, port, tok, sender);
                                });
                            }
                            MessageType::VoiceError { message: msg } => {
                                let _ = ui_sender.send(UIUpdate::ServerMessage {
                                    content: format!("Voice error: {}", msg),
                                    is_error: true,
                                });
                            }
                            MessageType::ServerMemberList { server_id, members } => {
                                let _ = ui_sender.send(UIUpdate::ServerMemberList {
                                    server_id: *server_id,
                                    members: members.clone(),
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

    /// Starts a UDP voice connection loop.
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

        let _ = socket.set_read_timeout(Some(Duration::from_millis(5)));
        let _ = ui_sender.send(UIUpdate::VoiceStatus {
            connected: true,
            message: "Voice connection initialized...".to_string(),
        });

        let mut buf = [0u8; 4096];
        let mut audio_chunk = Vec::with_capacity(1024);

        loop {
            // Send microphone audio
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

            // Receive and play audio
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

    // ========================================================================
    // KEY EVENT HANDLERS
    // ========================================================================

    /// Handles keyboard input. Returns `true` if the app should exit.
    async fn handle_key_event(&mut self, key: KeyCode) -> io::Result<bool> {
        if !self.authenticated {
            return self.handle_auth_key_event(key).await;
        }

        match key {
            KeyCode::Esc => {
                if self.show_help
                    || self.show_server_browser
                    || self.show_settings
                    || self.show_private_messages
                {
                    self.show_help = false;
                    self.show_server_browser = false;
                    self.show_settings = false;
                    self.show_private_messages = false;
                } else {
                    return Ok(true);
                }
            }
            KeyCode::F(1) => self.show_help = !self.show_help,
            KeyCode::F(2) => {
                self.show_server_browser = !self.show_server_browser;
                if self.show_server_browser {
                    if let Some(sender) = &self.message_sender {
                        let _ = sender.send(Message::new(self.user_id, MessageType::ListServers));
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
            KeyCode::F(5) => self.toggle_voice(),
            KeyCode::F(6) => {
                if self.voice_connected {
                    self.voice_muted = !self.voice_muted;
                    let msg = if self.voice_muted {
                        "Microphone muted"
                    } else {
                        "Microphone unmuted"
                    };
                    self.add_notification(msg.to_string(), NotificationType::Info);
                }
            }
            KeyCode::F(7) => {
                if self.voice_connected {
                    self.voice_deafened = !self.voice_deafened;
                    let msg = if self.voice_deafened {
                        "Audio deafened"
                    } else {
                        "Audio undeafened"
                    };
                    self.add_notification(msg.to_string(), NotificationType::Info);
                }
            }
            KeyCode::Enter => self.handle_enter().await?,
            KeyCode::Backspace => {
                if self.cursor_position > 0 {
                    self.input.remove(self.cursor_position - 1);
                    self.cursor_position -= 1;
                }
            }
            KeyCode::Left => self.handle_left(),
            KeyCode::Right => self.handle_right(),
            KeyCode::Up => self.handle_up(),
            KeyCode::Down => self.handle_down(),
            KeyCode::PageUp => {
                self.message_scroll_offset = self.message_scroll_offset.saturating_add(10);
            }
            KeyCode::PageDown => {
                self.message_scroll_offset = self.message_scroll_offset.saturating_sub(10);
            }
            KeyCode::Home => self.cursor_position = 0,
            KeyCode::End => self.cursor_position = self.input.len(),
            KeyCode::Tab => {
                if self.show_settings {
                    self.settings_tab = match self.settings_tab {
                        SettingsTab::Audio => SettingsTab::Voice,
                        SettingsTab::Voice => SettingsTab::Interface,
                        SettingsTab::Interface => SettingsTab::Audio,
                    };
                    self.settings_field = 0;
                } else {
                    self.switch_server();
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

    fn toggle_voice(&mut self) {
        if self.voice_connected {
            if let Some(stop_signal) = &self.voice_stop_signal {
                stop_signal.store(true, Ordering::Relaxed);
            }
            self.voice_connected = false;
            self.voice_stop_signal = None;
            self.voice_channel_id = None;
            self.add_notification("Voice disconnected".to_string(), NotificationType::Info);
        } else {
            let voice_channel = self.find_voice_channel_to_join();
            if let Some(channel_id) = voice_channel {
                if let Some(sender) = &self.message_sender {
                    let _ = sender.send(Message::new(
                        self.user_id,
                        MessageType::RequestVoice { channel_id },
                    ));
                    self.voice_channel_id = Some(channel_id);
                }
            } else {
                self.add_notification(
                    "No voice channel available".to_string(),
                    NotificationType::Warning,
                );
            }
        }
    }

    fn find_voice_channel_to_join(&self) -> Option<u16> {
        let server_id = self.current_server?;
        let server = self.servers.get(&server_id)?;

        // Check sidebar selection
        if self.sidebar_channel_index < server.channels.len() {
            let ch = &server.channels[self.sidebar_channel_index];
            if ch.channel_type == ChannelType::Voice {
                return Some(ch.id);
            }
        }

        // Fall back to first voice channel
        server
            .channels
            .iter()
            .find(|c| c.channel_type == ChannelType::Voice)
            .map(|c| c.id)
    }

    async fn handle_enter(&mut self) -> io::Result<()> {
        if self.show_settings {
            self.handle_settings_enter();
        } else if self.show_server_browser {
            if !self.available_servers.is_empty()
                && self.selected_server_index < self.available_servers.len()
            {
                let server = &self.available_servers[self.selected_server_index];
                if let Some(sender) = &self.message_sender {
                    let _ = sender.send(Message::new(
                        self.user_id,
                        MessageType::JoinServer {
                            server_id: server.id,
                        },
                    ));
                }
                self.show_server_browser = false;
            }
        } else if self.show_private_messages {
            if !self.input.is_empty() {
                if let Some(to_user) = self.selected_private_user {
                    if let Some(sender) = &self.message_sender {
                        let _ = sender.send(Message::new(
                            self.user_id,
                            MessageType::PrivateMessage {
                                to_user_id: to_user,
                                sender_username: self.username.clone(),
                                content: self.input.clone(),
                            },
                        ));
                        let pm = ChatMessage {
                            timestamp: Self::current_time(),
                            sender: self.username.clone(),
                            content: self.input.clone(),
                            message_type: ChatMessageType::Private,
                        };
                        self.private_conversations
                            .entry(to_user)
                            .or_insert_with(Vec::new)
                            .push(pm);
                    }
                    self.input.clear();
                    self.cursor_position = 0;
                }
            } else {
                self.update_selected_private_user();
            }
        } else if !self.input.is_empty() {
            self.send_message().await?;
            self.input.clear();
            self.cursor_position = 0;
        }
        Ok(())
    }

    fn handle_left(&mut self) {
        if self.show_settings {
            self.handle_settings_left();
        } else if self.show_server_browser {
            if self.selected_server_index > 0 {
                self.selected_server_index -= 1;
            }
        } else if self.cursor_position > 0 {
            self.cursor_position -= 1;
        }
    }

    fn handle_right(&mut self) {
        if self.show_settings {
            self.handle_settings_right();
        } else if self.show_server_browser {
            if self.selected_server_index < self.available_servers.len().saturating_sub(1) {
                self.selected_server_index += 1;
            }
        } else if self.cursor_position < self.input.len() {
            self.cursor_position += 1;
        }
    }

    fn handle_up(&mut self) {
        if self.show_settings {
            if self.settings_field > 0 {
                self.settings_field -= 1;
            }
        } else if self.show_server_browser {
            if self.selected_server_index > 0 {
                self.selected_server_index -= 1;
            }
        } else if self.show_private_messages {
            if self.selected_user_index > 0 {
                self.selected_user_index -= 1;
                self.update_selected_private_user();
            }
        } else {
            self.navigate_sidebar_up();
        }
    }

    fn handle_down(&mut self) {
        if self.show_settings {
            let max_field = match self.settings_tab {
                SettingsTab::Audio => 3,
                SettingsTab::Voice => 2,
                SettingsTab::Interface => 1,
            };
            if self.settings_field < max_field {
                self.settings_field += 1;
            }
        } else if self.show_server_browser {
            if self.selected_server_index < self.available_servers.len().saturating_sub(1) {
                self.selected_server_index += 1;
            }
        } else if self.show_private_messages {
            let user_count = self
                .online_users
                .iter()
                .filter(|u| u.user_id != self.user_id)
                .count();
            if self.selected_user_index < user_count.saturating_sub(1) {
                self.selected_user_index += 1;
                self.update_selected_private_user();
            }
        } else {
            self.navigate_sidebar_down();
        }
    }

    // ========================================================================
    // SIDEBAR NAVIGATION
    // ========================================================================

    fn navigate_sidebar_up(&mut self) {
        if let Some(server_id) = self.current_server {
            if let Some(server) = self.servers.get(&server_id) {
                if !server.channels.is_empty() && self.sidebar_channel_index > 0 {
                    self.sidebar_channel_index -= 1;
                    self.select_sidebar_channel();
                }
            }
        }
    }

    fn navigate_sidebar_down(&mut self) {
        if let Some(server_id) = self.current_server {
            if let Some(server) = self.servers.get(&server_id) {
                if self.sidebar_channel_index < server.channels.len().saturating_sub(1) {
                    self.sidebar_channel_index += 1;
                    self.select_sidebar_channel();
                }
            }
        }
    }

    fn select_sidebar_channel(&mut self) {
        if let Some(server_id) = self.current_server {
            if let Some(server) = self.servers.get(&server_id) {
                if self.sidebar_channel_index < server.channels.len() {
                    let channel = &server.channels[self.sidebar_channel_index];
                    if channel.channel_type == ChannelType::Text {
                        let old_channel = self.current_channel;
                        self.current_channel = Some(channel.id);
                        self.message_scroll_offset = 0;
                        if old_channel != Some(channel.id) {
                            if let Some(sender) = &self.message_sender {
                                let _ = sender.send(Message::new(
                                    self.user_id,
                                    MessageType::JoinChannel {
                                        channel_id: channel.id,
                                        password: None,
                                    },
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    fn switch_server(&mut self) {
        let ids = self.joined_server_ids();
        if ids.is_empty() {
            return;
        }

        if let Some(current) = self.current_server {
            if let Some(idx) = ids.iter().position(|&id| id == current) {
                self.current_server = Some(ids[(idx + 1) % ids.len()]);
            } else {
                self.current_server = Some(ids[0]);
            }
        } else {
            self.current_server = Some(ids[0]);
        }

        self.sidebar_channel_index = 0;
        self.message_scroll_offset = 0;

        if let Some(server_id) = self.current_server {
            if let Some(server) = self.servers.get(&server_id) {
                if let Some(ch) = server
                    .channels
                    .iter()
                    .find(|c| c.channel_type == ChannelType::Text)
                {
                    self.current_channel = Some(ch.id);
                    if let Some(sender) = &self.message_sender {
                        let _ = sender.send(Message::new(
                            self.user_id,
                            MessageType::JoinChannel {
                                channel_id: ch.id,
                                password: None,
                            },
                        ));
                    }
                } else {
                    self.current_channel = None;
                }
            }
        }
    }

    // ========================================================================
    // SETTINGS HANDLERS
    // ========================================================================

    fn handle_settings_enter(&mut self) {
        match self.settings_tab {
            SettingsTab::Audio => match self.settings_field {
                0 => {
                    if !self.audio_settings.input_devices.is_empty() {
                        self.audio_settings.selected_input_index =
                            (self.audio_settings.selected_input_index + 1)
                                % self.audio_settings.input_devices.len();
                        if let Some(name) = self.selected_input_device() {
                            self.add_notification(
                                format!("Input: {}", name),
                                NotificationType::Info,
                            );
                        }
                    }
                }
                1 => {
                    if !self.audio_settings.output_devices.is_empty() {
                        self.audio_settings.selected_output_index =
                            (self.audio_settings.selected_output_index + 1)
                                % self.audio_settings.output_devices.len();
                        if let Some(name) = self.selected_output_device() {
                            self.add_notification(
                                format!("Output: {}", name),
                                NotificationType::Info,
                            );
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn handle_settings_left(&mut self) {
        if let SettingsTab::Audio = self.settings_tab {
            match self.settings_field {
                0 => {
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
                    self.audio_settings.input_volume =
                        self.audio_settings.input_volume.saturating_sub(5);
                }
                3 => {
                    self.audio_settings.output_volume =
                        self.audio_settings.output_volume.saturating_sub(5);
                }
                _ => {}
            }
        }
    }

    fn handle_settings_right(&mut self) {
        if let SettingsTab::Audio = self.settings_tab {
            match self.settings_field {
                0 => {
                    if !self.audio_settings.input_devices.is_empty() {
                        self.audio_settings.selected_input_index =
                            (self.audio_settings.selected_input_index + 1)
                                % self.audio_settings.input_devices.len();
                    }
                }
                1 => {
                    if !self.audio_settings.output_devices.is_empty() {
                        self.audio_settings.selected_output_index =
                            (self.audio_settings.selected_output_index + 1)
                                % self.audio_settings.output_devices.len();
                    }
                }
                2 => {
                    self.audio_settings.input_volume =
                        (self.audio_settings.input_volume + 5).min(100);
                }
                3 => {
                    self.audio_settings.output_volume =
                        (self.audio_settings.output_volume + 5).min(100);
                }
                _ => {}
            }
        }
    }

    // ========================================================================
    // AUTH HANDLERS
    // ========================================================================

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
                AuthField::Username => self.auth_username.push(c),
                AuthField::Password => self.auth_password.push(c),
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

    // ========================================================================
    // MESSAGING & COMMANDS
    // ========================================================================

    async fn send_message(&mut self) -> io::Result<()> {
        if self.message_sender.is_some() {
            if self.input.starts_with('/') {
                self.handle_command().await?;
            } else if let Some(channel_id) = self.current_channel {
                if let Some(sender) = &self.message_sender {
                    let _ = sender.send(Message::new(
                        self.user_id,
                        MessageType::ChannelMessage {
                            channel_id,
                            sender_username: self.username.clone(),
                            content: self.input.clone(),
                        },
                    ));
                }
            }
        }
        Ok(())
    }

    async fn handle_command(&mut self) -> io::Result<()> {
        let parts: Vec<&str> = self.input.split_whitespace().collect();
        if let Some(sender) = &self.message_sender {
            match parts.first() {
                Some(&"/server") => {
                    if parts.len() >= 3 {
                        let name = parts[1].to_string();
                        let description = parts[2..].join(" ");
                        let _ = sender.send(Message::new(
                            self.user_id,
                            MessageType::CreateServer { name, description },
                        ));
                    } else {
                        self.add_notification(
                            "Usage: /server <name> <description>".to_string(),
                            NotificationType::Warning,
                        );
                    }
                }
                Some(&"/channel") => {
                    if let Some(server_id) = self.current_server {
                        if parts.len() >= 3 {
                            let name = parts[1].to_string();
                            let description = parts[2].to_string();
                            let channel_type =
                                if parts.len() > 3 && parts[3].to_lowercase() == "voice" {
                                    ChannelType::Voice
                                } else {
                                    ChannelType::Text
                                };
                            let _ = sender.send(Message::new(
                                self.user_id,
                                MessageType::CreateChannel {
                                    server_id,
                                    name,
                                    description,
                                    channel_type,
                                    password: None,
                                },
                            ));
                        } else {
                            self.add_notification(
                                "Usage: /channel <name> <desc> [text|voice]".to_string(),
                                NotificationType::Warning,
                            );
                        }
                    } else {
                        self.add_notification(
                            "Select a server first".to_string(),
                            NotificationType::Warning,
                        );
                    }
                }
                Some(&"/join") => {
                    if let Some(id_str) = parts.get(1) {
                        if let Ok(server_id) = id_str.parse::<u16>() {
                            let _ = sender.send(Message::new(
                                self.user_id,
                                MessageType::JoinServer { server_id },
                            ));
                        } else {
                            self.add_notification(
                                "Invalid server ID".to_string(),
                                NotificationType::Warning,
                            );
                        }
                    } else {
                        self.add_notification(
                            "Usage: /join <server_id>".to_string(),
                            NotificationType::Warning,
                        );
                    }
                }
                Some(&"/leave") => {
                    if let Some(server_id) = self.current_server {
                        let _ = sender.send(Message::new(
                            self.user_id,
                            MessageType::LeaveServer { server_id },
                        ));
                    } else {
                        self.add_notification(
                            "Not in any server".to_string(),
                            NotificationType::Warning,
                        );
                    }
                }
                Some(&"/msg") | Some(&"/pm") => {
                    if parts.len() >= 3 {
                        if let Ok(to_user_id) = parts[1].parse::<u16>() {
                            let content = parts[2..].join(" ");
                            let _ = sender.send(Message::new(
                                self.user_id,
                                MessageType::PrivateMessage {
                                    to_user_id,
                                    sender_username: self.username.clone(),
                                    content: content.clone(),
                                },
                            ));
                            let pm = ChatMessage {
                                timestamp: Self::current_time(),
                                sender: self.username.clone(),
                                content,
                                message_type: ChatMessageType::Private,
                            };
                            self.private_conversations
                                .entry(to_user_id)
                                .or_insert_with(Vec::new)
                                .push(pm);
                        }
                    } else {
                        self.add_notification(
                            "Usage: /msg <user_id> <message>".to_string(),
                            NotificationType::Warning,
                        );
                    }
                }
                Some(&"/members") => {
                    if let Some(server_id) = self.current_server {
                        let _ = sender.send(Message::new(
                            self.user_id,
                            MessageType::RequestServerMembers { server_id },
                        ));
                    }
                }
                _ => {
                    self.add_notification(
                        "Unknown command. Press F1 for help.".to_string(),
                        NotificationType::Warning,
                    );
                }
            }
        }
        Ok(())
    }

    fn update_selected_private_user(&mut self) {
        let other_users: Vec<&UserStatus> = self
            .online_users
            .iter()
            .filter(|u| u.user_id != self.user_id)
            .collect();
        if !other_users.is_empty() && self.selected_user_index < other_users.len() {
            self.selected_private_user = Some(other_users[self.selected_user_index].user_id);
        } else {
            self.selected_private_user = None;
        }
    }

    // ========================================================================
    // DRAWING
    // ========================================================================

    fn draw(&self, f: &mut Frame) {
        if !self.authenticated {
            self.draw_auth_screen(f);
        } else {
            self.draw_main_screen(f);
        }
    }

    fn draw_auth_screen(&self, f: &mut Frame) {
        let size = f.area();
        let popup_area = Self::centered_rect_fixed(60, 20, size);
        f.render_widget(Clear, popup_area);

        let mode_label = if self.auth_mode == AuthMode::Login {
            "Login"
        } else {
            "Register"
        };
        let block = Block::default()
            .title(format!(" {} ", mode_label))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));
        f.render_widget(block, popup_area);

        let inner = popup_area.inner(ratatui::layout::Margin {
            horizontal: 2,
            vertical: 1,
        });
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(1),
            ])
            .split(inner);

        // Logo
        let logo_lines = vec![
            Line::from(Span::styled(
                "  \u{256d}\u{2500}\u{2500}\u{2500}\u{256e}      ",
                Style::default().fg(Color::Cyan),
            )),
            Line::from(vec![
                Span::styled("  \u{2502} ", Style::default().fg(Color::Cyan)),
                Span::styled("\u{256d}\u{2500}", Style::default().fg(Color::Blue)),
                Span::styled("\u{253c}", Style::default().fg(Color::Cyan)),
                Span::styled("\u{2500}\u{256e}  ", Style::default().fg(Color::Blue)),
            ]),
            Line::from(vec![
                Span::styled("  \u{2570}\u{2500}", Style::default().fg(Color::Cyan)),
                Span::styled("\u{253c}", Style::default().fg(Color::Blue)),
                Span::styled("\u{2500}\u{256f} ", Style::default().fg(Color::Cyan)),
                Span::styled("\u{2502}  ", Style::default().fg(Color::Blue)),
            ]),
            Line::from(Span::styled(
                "    \u{2570}\u{2500}\u{2500}\u{2500}\u{256f}    ",
                Style::default().fg(Color::Blue),
            )),
            Line::from(""),
        ];
        f.render_widget(
            Paragraph::new(logo_lines).alignment(Alignment::Center),
            chunks[0],
        );

        let username_style = if self.auth_field == AuthField::Username {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        f.render_widget(
            Paragraph::new(self.auth_username.as_str()).block(
                Block::default()
                    .title("Username")
                    .borders(Borders::ALL)
                    .style(username_style),
            ),
            chunks[2],
        );

        let password_style = if self.auth_field == AuthField::Password {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        f.render_widget(
            Paragraph::new("*".repeat(self.auth_password.len()).as_str()).block(
                Block::default()
                    .title("Password")
                    .borders(Borders::ALL)
                    .style(password_style),
            ),
            chunks[3],
        );

        let toggle_label = if self.auth_mode == AuthMode::Login {
            "Register"
        } else {
            "Login"
        };
        let instructions = vec![
            Line::from(vec![
                Span::raw("Tab: Switch fields | F2: Toggle "),
                Span::styled(toggle_label, Style::default().fg(Color::Green)),
            ]),
            Line::from("Enter: Submit | Esc: Quit"),
        ];
        f.render_widget(
            Paragraph::new(instructions)
                .block(Block::default().title("Controls").borders(Borders::ALL)),
            chunks[4],
        );

        if let Some(error) = &self.auth_error {
            f.render_widget(
                Paragraph::new(error.as_str())
                    .style(Style::default().fg(Color::Red))
                    .wrap(Wrap { trim: true }),
                chunks[5],
            );
        }
    }

    fn draw_main_screen(&self, f: &mut Frame) {
        let size = f.area();
        let tree_width: u16 = 28;

        // Two-column layout: tree panel | chat area
        let main_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(tree_width),
                Constraint::Min(1),
            ])
            .split(size);

        // Left panel: tree/sidebar
        self.draw_sidebar(f, main_chunks[0]);

        // Draw vertical separator between panels
        let sep_x = main_chunks[0].right().min(size.width.saturating_sub(1));
        let buf = f.buffer_mut();
        for y in size.y..size.y + size.height {
            if sep_x < size.width {
                let cell = &mut buf[(sep_x, y)];
                cell.set_symbol("│");
                cell.set_style(Style::default().fg(Color::DarkGray));
            }
        }

        // Right panel: title bar (1) + chat (fill) + status bar (1) + input (1)
        // Offset the chat area by 1 to leave room for the separator
        let chat_panel = Rect {
            x: main_chunks[1].x + 1,
            y: main_chunks[1].y,
            width: main_chunks[1].width.saturating_sub(1),
            height: main_chunks[1].height,
        };

        let chat_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // title bar
                Constraint::Min(1),    // chat messages
                Constraint::Length(1), // status bar
                Constraint::Length(1), // input
            ])
            .split(chat_panel);

        self.draw_chat_header(f, chat_chunks[0]);
        self.draw_chat_area(f, chat_chunks[1]);
        self.draw_status_bar(f, chat_chunks[2]);
        self.draw_input_area(f, chat_chunks[3]);

        // Overlays
        self.draw_notifications(f);

        if self.show_help {
            self.draw_help_popup(f);
        }
        if self.show_server_browser {
            self.draw_server_browser(f);
        }
        if self.show_settings {
            self.draw_settings(f);
        }
        if self.show_private_messages {
            self.draw_private_messages(f);
        }
    }

    fn draw_sidebar(&self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // app name + separator
                Constraint::Min(1),    // server/channel tree
                Constraint::Length(3), // user status
            ])
            .split(area);

        // App name header
        let sep_str: String = "─".repeat(area.width as usize);
        let header_lines = vec![
            Line::from(Span::styled(
                " RustyRoom",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(&sep_str, Style::default().fg(Color::DarkGray))),
        ];
        f.render_widget(Paragraph::new(header_lines), chunks[0]);

        // Server/Channel Tree
        let mut tree_items: Vec<ListItem> = Vec::new();
        let server_ids = self.joined_server_ids();

        for &sid in &server_ids {
            if let Some(server) = self.servers.get(&sid) {
                let is_current = self.current_server == Some(sid);
                let max_name = (area.width as usize).saturating_sub(4);
                let display_name = if server.name.len() > max_name {
                    format!("{}…", &server.name[..max_name.saturating_sub(1)])
                } else {
                    server.name.clone()
                };

                if is_current {
                    // Expanded server: ▾ ServerName
                    tree_items.push(ListItem::new(Line::from(vec![
                        Span::styled(" ▾ ", Style::default().fg(Color::Cyan)),
                        Span::styled(
                            display_name,
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ])));

                    // Channels with tree-drawing chars
                    let channel_count = server.channels.len();
                    for (idx, channel) in server.channels.iter().enumerate() {
                        let is_last = idx == channel_count - 1;
                        let is_selected = idx == self.sidebar_channel_index;
                        let is_active = self.current_channel == Some(channel.id);
                        let tree_char = if is_last { "╰── " } else { "├── " };

                        let (icon, name_str) = match channel.channel_type {
                            ChannelType::Text => ("# ", channel.name.clone()),
                            ChannelType::Voice => {
                                let icon = if self.voice_connected
                                    && self.voice_channel_id == Some(channel.id)
                                {
                                    "🔊 "
                                } else {
                                    "🔈 "
                                };
                                (icon, channel.name.clone())
                            }
                        };

                        let style = if is_selected && is_active {
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::White)
                                .add_modifier(Modifier::BOLD)
                        } else if is_selected {
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::White)
                        } else if is_active {
                            Style::default()
                                .fg(Color::White)
                                .bg(Color::DarkGray)
                                .add_modifier(Modifier::BOLD)
                        } else if channel.channel_type == ChannelType::Voice
                            && self.voice_connected
                            && self.voice_channel_id == Some(channel.id)
                        {
                            Style::default().fg(Color::Green)
                        } else {
                            Style::default().fg(Color::Gray)
                        };

                        let max_ch_len = (area.width as usize).saturating_sub(2);
                        let full = format!(" {}{}{}", tree_char, icon, name_str);
                        let display: String = if full.chars().count() > max_ch_len {
                            full.chars().take(max_ch_len).collect::<String>() + "…"
                        } else {
                            // Pad to full width for bg highlight
                            let pad = max_ch_len.saturating_sub(full.chars().count());
                            format!("{}{}", full, " ".repeat(pad))
                        };
                        tree_items
                            .push(ListItem::new(Line::from(Span::styled(display, style))));
                    }
                } else {
                    // Collapsed server: ▸ ServerName
                    tree_items.push(ListItem::new(Line::from(vec![
                        Span::styled(" ▸ ", Style::default().fg(Color::DarkGray)),
                        Span::styled(display_name, Style::default().fg(Color::DarkGray)),
                    ])));
                }
            }
        }

        f.render_widget(List::new(tree_items), chunks[1]);

        // User / Voice Status at bottom
        let voice_info = if self.voice_connected {
            if self.voice_muted {
                Span::styled("Muted", Style::default().fg(Color::Red))
            } else if self.voice_deafened {
                Span::styled("Deafened", Style::default().fg(Color::Red))
            } else {
                Span::styled("Voice ●", Style::default().fg(Color::Green))
            }
        } else {
            Span::styled("Voice Off", Style::default().fg(Color::DarkGray))
        };

        let bottom_sep: String = "─".repeat(area.width as usize);
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(&bottom_sep, Style::default().fg(Color::DarkGray))),
                Line::from(vec![
                    Span::styled(" ● ", Style::default().fg(Color::Green)),
                    Span::styled(
                        &self.username,
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(vec![Span::raw(" "), voice_info]),
            ]),
            chunks[2],
        );
    }

    fn draw_chat_header(&self, f: &mut Frame, area: Rect) {
        let title_str = if let Some(server_id) = self.current_server {
            if let Some(server) = self.servers.get(&server_id) {
                if let Some(channel_id) = self.current_channel {
                    if let Some(channel) = server.channels.iter().find(|c| c.id == channel_id) {
                        let prefix = match channel.channel_type {
                            ChannelType::Text => "#",
                            ChannelType::Voice => "🔊",
                        };
                        format!(" {}: {}{}", server.name, prefix, channel.name)
                    } else {
                        format!(" {} — Select a channel", server.name)
                    }
                } else {
                    format!(" {} — Select a channel", server.name)
                }
            } else {
                " Loading...".to_string()
            }
        } else {
            " Join a server to start chatting".to_string()
        };

        // Pad to full width for inverted bar look
        let padded = format!("{:<width$}", title_str, width = area.width as usize);
        let bar_style = Style::default().fg(Color::Black).bg(Color::White);
        f.render_widget(
            Paragraph::new(Span::styled(padded, bar_style)),
            area,
        );
    }

    fn draw_chat_area(&self, f: &mut Frame, area: Rect) {
        if let Some(channel_id) = self.current_channel {
            if let Some(server_id) = self.current_server {
                if let Some(server) = self.servers.get(&server_id) {
                    if let Some(messages) = server.channel_messages.get(&channel_id) {
                        let visible_height = area.height as usize;

                        let msg_lines: Vec<Line> = messages
                            .iter()
                            .map(|msg| match msg.message_type {
                                ChatMessageType::User => Line::from(vec![
                                    Span::styled(
                                        format!("[{}] ", msg.timestamp),
                                        Style::default().fg(Color::DarkGray),
                                    ),
                                    Span::styled(
                                        &msg.sender,
                                        Style::default()
                                            .fg(Color::Cyan)
                                            .add_modifier(Modifier::BOLD),
                                    ),
                                    Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
                                    Span::styled(&msg.content, Style::default().fg(Color::White)),
                                ]),
                                ChatMessageType::System => Line::from(vec![
                                    Span::styled(
                                        format!("[{}] ", msg.timestamp),
                                        Style::default().fg(Color::DarkGray),
                                    ),
                                    Span::styled(
                                        format!("* {} │ {}", msg.sender, msg.content),
                                        Style::default().fg(Color::Green),
                                    ),
                                ]),
                                ChatMessageType::Private => Line::from(vec![
                                    Span::styled(
                                        format!("[{}] ", msg.timestamp),
                                        Style::default().fg(Color::DarkGray),
                                    ),
                                    Span::styled(
                                        &msg.sender,
                                        Style::default().fg(Color::Magenta),
                                    ),
                                    Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
                                    Span::styled(
                                        &msg.content,
                                        Style::default().fg(Color::Magenta),
                                    ),
                                ]),
                                ChatMessageType::Error => Line::from(vec![
                                    Span::styled(
                                        format!("[{}] ", msg.timestamp),
                                        Style::default().fg(Color::DarkGray),
                                    ),
                                    Span::styled(
                                        format!("✗ {}", msg.content),
                                        Style::default().fg(Color::Red),
                                    ),
                                ]),
                            })
                            .collect();

                        // Bottom-align: skip leading messages if more than visible
                        let total = msg_lines.len();
                        let skip = if total > visible_height + self.message_scroll_offset {
                            total - visible_height - self.message_scroll_offset
                        } else {
                            0
                        };
                        let visible: Vec<Line> = msg_lines
                            .into_iter()
                            .skip(skip)
                            .take(visible_height)
                            .collect();

                        // Pad top if fewer messages than area height
                        let mut padded = Vec::new();
                        if visible.len() < visible_height {
                            for _ in 0..(visible_height - visible.len()) {
                                padded.push(Line::from(""));
                            }
                        }
                        padded.extend(visible);

                        f.render_widget(Paragraph::new(padded), area);
                        return;
                    }
                }
            }
        }

        // Empty state - centered message
        let empty_y = area.y + area.height / 2;
        let empty_area = Rect {
            x: area.x,
            y: empty_y,
            width: area.width,
            height: 1,
        };
        f.render_widget(
            Paragraph::new("Join a server and select a channel to start chatting")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center),
            empty_area,
        );
    }

    fn draw_input_area(&self, f: &mut Frame, area: Rect) {
        let channel_name = if let (Some(server_id), Some(channel_id)) =
            (self.current_server, self.current_channel)
        {
            self.servers
                .get(&server_id)
                .and_then(|s| s.channels.iter().find(|c| c.id == channel_id))
                .map(|c| format!("[#{}] > ", c.name))
                .unwrap_or_else(|| "> ".to_string())
        } else {
            "> ".to_string()
        };

        let input_text = if self.cursor_visible {
            let mut text = self.input.clone();
            if self.cursor_position <= text.len() {
                text.insert(self.cursor_position, '│');
            }
            text
        } else {
            self.input.clone()
        };

        let spans = if input_text.is_empty() && !self.cursor_visible {
            vec![
                Span::styled(&channel_name, Style::default().fg(Color::Cyan)),
                Span::styled(
                    "Type a message...",
                    Style::default().fg(Color::DarkGray),
                ),
            ]
        } else {
            vec![
                Span::styled(&channel_name, Style::default().fg(Color::Cyan)),
                Span::styled(input_text, Style::default().fg(Color::White)),
            ]
        };

        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_status_bar(&self, f: &mut Frame, area: Rect) {
        let online_count = self.online_users.iter().filter(|u| u.is_online).count();
        let status_left = format!(
            " {} — Online  {} user(s)",
            self.username, online_count
        );
        let status_right = "F1:Help F2:Browse F3:PM F4:Settings F5:Voice";
        let gap = (area.width as usize)
            .saturating_sub(status_left.len())
            .saturating_sub(status_right.len());
        let padded = format!("{}{}{}", status_left, " ".repeat(gap), status_right);
        let bar_style = Style::default().fg(Color::Black).bg(Color::White);
        f.render_widget(
            Paragraph::new(Span::styled(padded, bar_style)),
            area,
        );
    }

    fn draw_notifications(&self, f: &mut Frame) {
        if self.notifications.is_empty() {
            return;
        }
        let area = f.area();
        let h = self.notifications.len().min(5) as u16;
        let notification_area = Rect {
            x: area.width.saturating_sub(42),
            y: 1,
            width: 40,
            height: h * 2 + 1,
        };

        let items: Vec<ListItem> = self
            .notifications
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
            f.render_widget(Clear, notification_area);
            f.render_widget(
                List::new(items).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .style(Style::default().fg(Color::DarkGray)),
                ),
                notification_area,
            );
        }
    }

    fn draw_help_popup(&self, f: &mut Frame) {
        let popup_area = Self::centered_rect_fixed(68, 26, f.area());
        f.render_widget(Clear, popup_area);

        let help_text = vec![
            Line::from(Span::styled(
                "\u{25ec} Help",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Navigation:",
                Style::default().fg(Color::Yellow),
            )),
            Line::from("  Tab          Cycle between servers"),
            Line::from("  \u{2191}/\u{2193}          Select channels in current server"),
            Line::from("  PgUp/PgDn    Scroll messages faster"),
            Line::from("  Home/End     Jump to start/end of input"),
            Line::from(""),
            Line::from(Span::styled(
                "Function Keys:",
                Style::default().fg(Color::Yellow),
            )),
            Line::from("  F1           Toggle this help"),
            Line::from("  F2           Server browser"),
            Line::from("  F3           Private messages"),
            Line::from("  F4           Settings (audio devices, voice)"),
            Line::from("  F5           Toggle voice chat"),
            Line::from("  F6           Toggle mute (when in voice)"),
            Line::from("  F7           Toggle deafen (when in voice)"),
            Line::from("  Esc          Close dialogs or quit"),
            Line::from(""),
            Line::from(Span::styled(
                "Chat Commands:",
                Style::default().fg(Color::Yellow),
            )),
            Line::from("  /server <name> <desc>                Create a server"),
            Line::from("  /channel <name> <desc> [text|voice]  Create channel"),
            Line::from("  /join <server_id>                    Join a server"),
            Line::from("  /leave                               Leave current server"),
            Line::from("  /msg <user_id> <message>             Private message"),
            Line::from("  /members                             Server member list"),
        ];

        f.render_widget(
            Paragraph::new(help_text)
                .block(
                    Block::default()
                        .title("Help")
                        .borders(Borders::ALL)
                        .style(Style::default().fg(Color::Cyan)),
                )
                .wrap(Wrap { trim: true }),
            popup_area,
        );
    }

    fn draw_server_browser(&self, f: &mut Frame) {
        let popup_area = Self::centered_rect_fixed(70, 20, f.area());
        f.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(" Server Browser ")
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::Cyan));

        if self.available_servers.is_empty() {
            f.render_widget(
                Paragraph::new("Loading servers...\n\nPress Esc to close")
                    .block(block)
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: true }),
                popup_area,
            );
        } else {
            f.render_widget(block, popup_area);
            let inner = popup_area.inner(ratatui::layout::Margin {
                horizontal: 2,
                vertical: 2,
            });
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(3)])
                .split(inner);

            let items: Vec<ListItem> = self
                .available_servers
                .iter()
                .enumerate()
                .map(|(i, server)| {
                    let style = if i == self.selected_server_index {
                        Style::default()
                            .bg(Color::White)
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    ListItem::new(format!(
                        "  [{}] {} - {} ({} members)",
                        server.id, server.name, server.description, server.member_count
                    ))
                    .style(style)
                })
                .collect();

            f.render_widget(
                List::new(items).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Available Servers"),
                ),
                chunks[0],
            );

            f.render_widget(
                Paragraph::new("Up/Down: Navigate | Enter: Join | Esc: Close")
                    .block(Block::default().borders(Borders::ALL).title("Controls"))
                    .alignment(Alignment::Center),
                chunks[1],
            );
        }
    }

    fn draw_private_messages(&self, f: &mut Frame) {
        let popup_area = Self::centered_rect_fixed(80, 25, f.area());
        f.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(" Private Messages ")
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::Magenta));
        f.render_widget(block, popup_area);

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
            .filter(|u| u.user_id != self.user_id)
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
                let icon = if user.is_online { "\u{25cf}" } else { "\u{25cb}" };
                ListItem::new(format!("{} {}", icon, user.username)).style(style)
            })
            .collect();

        f.render_widget(
            List::new(user_items).block(Block::default().borders(Borders::ALL).title("Users")),
            chunks[0],
        );

        if let Some(selected) = self.selected_private_user {
            if let Some(messages) = self.private_conversations.get(&selected) {
                let items: Vec<ListItem> = messages
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
                    List::new(items)
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
                Paragraph::new("Select a user to message")
                    .block(Block::default().borders(Borders::ALL).title("Messages"))
                    .alignment(Alignment::Center),
                chunks[1],
            );
        }
    }

    fn draw_settings(&self, f: &mut Frame) {
        let popup_area = Self::centered_rect_fixed(70, 22, f.area());
        f.render_widget(Clear, popup_area);

        f.render_widget(
            Block::default()
                .title(" Settings ")
                .borders(Borders::ALL)
                .style(Style::default().fg(Color::Cyan)),
            popup_area,
        );

        let inner = popup_area.inner(ratatui::layout::Margin {
            horizontal: 2,
            vertical: 1,
        });
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(10),
                Constraint::Length(3),
            ])
            .split(inner);

        // Tabs
        let selected_tab = match self.settings_tab {
            SettingsTab::Audio => 0,
            SettingsTab::Voice => 1,
            SettingsTab::Interface => 2,
        };
        f.render_widget(
            Tabs::new(vec!["Audio", "Voice", "Interface"])
                .block(Block::default().borders(Borders::BOTTOM))
                .style(Style::default().fg(Color::White))
                .highlight_style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
                .select(selected_tab),
            chunks[0],
        );

        match self.settings_tab {
            SettingsTab::Audio => self.draw_audio_settings(f, chunks[1]),
            SettingsTab::Voice => self.draw_voice_settings(f, chunks[1]),
            SettingsTab::Interface => self.draw_interface_settings(f, chunks[1]),
        }

        f.render_widget(
            Paragraph::new(
                "Tab: Switch tabs | \u{2191}\u{2193}: Navigate | \u{2190}\u{2192}: Adjust | Esc: Close",
            )
            .style(Style::default().fg(Color::Gray))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::TOP)),
            chunks[2],
        );
    }

    fn draw_audio_settings(&self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(0),
            ])
            .split(area);

        let input_name = self.selected_input_device().unwrap_or("No device");
        let input_style = if self.settings_field == 0 {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        f.render_widget(
            Paragraph::new(format!("\u{25c4} {} \u{25ba}", input_name))
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .title("Input Device")
                        .borders(Borders::ALL)
                        .style(input_style),
                ),
            chunks[0],
        );

        let output_name = self.selected_output_device().unwrap_or("No device");
        let output_style = if self.settings_field == 1 {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        f.render_widget(
            Paragraph::new(format!("\u{25c4} {} \u{25ba}", output_name))
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .title("Output Device")
                        .borders(Borders::ALL)
                        .style(output_style),
                ),
            chunks[1],
        );

        f.render_widget(
            Gauge::default()
                .block(
                    Block::default()
                        .title("Input Volume")
                        .borders(Borders::ALL)
                        .style(if self.settings_field == 2 {
                            Style::default().fg(Color::Yellow)
                        } else {
                            Style::default()
                        }),
                )
                .gauge_style(if self.settings_field == 2 {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::Green)
                })
                .percent(self.audio_settings.input_volume as u16)
                .label(format!("{}%", self.audio_settings.input_volume)),
            chunks[2],
        );

        f.render_widget(
            Gauge::default()
                .block(
                    Block::default()
                        .title("Output Volume")
                        .borders(Borders::ALL)
                        .style(if self.settings_field == 3 {
                            Style::default().fg(Color::Yellow)
                        } else {
                            Style::default()
                        }),
                )
                .gauge_style(if self.settings_field == 3 {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::Blue)
                })
                .percent(self.audio_settings.output_volume as u16)
                .label(format!("{}%", self.audio_settings.output_volume)),
            chunks[3],
        );
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
        f.render_widget(
            Paragraph::new(format!("[ {} ] (F6 to toggle)", mute_status))
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .title("Mute Microphone")
                        .borders(Borders::ALL)
                        .style(if self.settings_field == 0 {
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        }),
                ),
            chunks[0],
        );

        let deafen_status = if self.voice_deafened { "ON " } else { "OFF" };
        f.render_widget(
            Paragraph::new(format!("[ {} ] (F7 to toggle)", deafen_status))
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .title("Deafen (Mute Speakers)")
                        .borders(Borders::ALL)
                        .style(if self.settings_field == 1 {
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        }),
                ),
            chunks[1],
        );
    }

    fn draw_interface_settings(&self, f: &mut Frame, area: Rect) {
        f.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from("Interface customization coming soon!"),
                Line::from(""),
                Line::from("Planned features:"),
                Line::from("  \u{2022} Color themes"),
                Line::from("  \u{2022} Timestamp format"),
                Line::from("  \u{2022} Message display options"),
            ])
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::Gray))
            .block(Block::default().borders(Borders::ALL)),
            area,
        );
    }

    // ========================================================================
    // UTILITY
    // ========================================================================

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

    #[allow(dead_code)]
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
