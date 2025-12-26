//! Voice Communication Module for RustyRoom
//!
//! This module implements real-time voice communication with:
//! - Opus codec for efficient audio compression
//! - UDP-based audio packet transmission
//! - Voice Activity Detection (VAD)
//! - Jitter buffer for smooth playback
//! - Room-based audio routing

use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use bytes::{Buf, BufMut, BytesMut};
use std::collections::{HashMap, VecDeque};
use std::io::Cursor;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::RwLock;

// ============================================================================
// CONSTANTS
// ============================================================================

/// Audio sample rate in Hz (standard for Opus)
pub const SAMPLE_RATE: u32 = 48000;

/// Frame size in samples (20ms at 48kHz)
pub const FRAME_SIZE: usize = 960;

/// Number of audio channels (mono)
pub const CHANNELS: usize = 1;

/// Maximum packet size for UDP transmission
pub const MAX_PACKET_SIZE: usize = 4096;

/// Opus bitrate in bits per second
pub const OPUS_BITRATE: i32 = 48000;

/// Jitter buffer target latency in milliseconds
pub const JITTER_BUFFER_TARGET_MS: u64 = 60;

/// Voice Activity Detection threshold (RMS energy)
pub const VAD_THRESHOLD: f32 = 0.01;

/// Silence frames before stopping transmission
pub const SILENCE_FRAMES_THRESHOLD: u32 = 25; // ~500ms at 20ms frames

// ============================================================================
// AUDIO PACKET PROTOCOL
// ============================================================================

/// Packet types for voice protocol
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum PacketType {
    /// Audio data packet
    Audio = 0,
    /// Heartbeat/keepalive packet
    Heartbeat = 1,
    /// Voice state update (mute/deafen)
    StateUpdate = 2,
    /// Speaking indicator
    Speaking = 3,
    /// Authentication token
    Auth = 4,
    /// Connection acknowledgment
    Ack = 5,
}

impl From<u8> for PacketType {
    fn from(v: u8) -> Self {
        match v {
            0 => PacketType::Audio,
            1 => PacketType::Heartbeat,
            2 => PacketType::StateUpdate,
            3 => PacketType::Speaking,
            4 => PacketType::Auth,
            5 => PacketType::Ack,
            _ => PacketType::Audio,
        }
    }
}

/// Voice packet header structure
///
/// Layout (14 bytes total):
/// - Version (1 byte): Protocol version
/// - Type (1 byte): Packet type
/// - User ID (2 bytes): Sender user ID
/// - Room ID (2 bytes): Target room ID
/// - Sequence (2 bytes): Packet sequence number
/// - Timestamp (4 bytes): Audio timestamp
/// - Payload Length (2 bytes): Length of audio data
#[derive(Debug, Clone)]
pub struct VoicePacketHeader {
    pub version: u8,
    pub packet_type: PacketType,
    pub user_id: u16,
    pub room_id: u16,
    pub sequence: u16,
    pub timestamp: u32,
    pub payload_length: u16,
}

impl VoicePacketHeader {
    /// Header size in bytes
    pub const SIZE: usize = 14;

    /// Create a new audio packet header
    pub fn new_audio(user_id: u16, room_id: u16, sequence: u16, timestamp: u32, payload_len: u16) -> Self {
        Self {
            version: 1,
            packet_type: PacketType::Audio,
            user_id,
            room_id,
            sequence,
            timestamp,
            payload_length: payload_len,
        }
    }

    /// Create a heartbeat packet header
    pub fn new_heartbeat(user_id: u16, room_id: u16) -> Self {
        Self {
            version: 1,
            packet_type: PacketType::Heartbeat,
            user_id,
            room_id,
            sequence: 0,
            timestamp: 0,
            payload_length: 0,
        }
    }

    /// Serialize header to bytes
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(Self::SIZE);
        buf.push(self.version);
        buf.push(self.packet_type as u8);
        buf.write_u16::<BigEndian>(self.user_id).unwrap();
        buf.write_u16::<BigEndian>(self.room_id).unwrap();
        buf.write_u16::<BigEndian>(self.sequence).unwrap();
        buf.write_u32::<BigEndian>(self.timestamp).unwrap();
        buf.write_u16::<BigEndian>(self.payload_length).unwrap();
        buf
    }

    /// Deserialize header from bytes
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < Self::SIZE {
            return None;
        }

        let mut cursor = Cursor::new(data);
        Some(Self {
            version: cursor.get_u8(),
            packet_type: PacketType::from(cursor.get_u8()),
            user_id: cursor.read_u16::<BigEndian>().ok()?,
            room_id: cursor.read_u16::<BigEndian>().ok()?,
            sequence: cursor.read_u16::<BigEndian>().ok()?,
            timestamp: cursor.read_u32::<BigEndian>().ok()?,
            payload_length: cursor.read_u16::<BigEndian>().ok()?,
        })
    }
}

/// Complete voice packet with header and audio payload
#[derive(Debug, Clone)]
pub struct VoicePacket {
    pub header: VoicePacketHeader,
    pub payload: Vec<u8>,
}

impl VoicePacket {
    /// Create a new voice packet
    pub fn new(header: VoicePacketHeader, payload: Vec<u8>) -> Self {
        Self { header, payload }
    }

    /// Encode packet to bytes for transmission
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = BytesMut::with_capacity(VoicePacketHeader::SIZE + self.payload.len());
        buf.put_slice(&self.header.encode());
        buf.put_slice(&self.payload);
        buf.to_vec()
    }

    /// Decode packet from received bytes
    pub fn decode(data: &[u8]) -> Option<Self> {
        let header = VoicePacketHeader::decode(data)?;
        
        if data.len() < VoicePacketHeader::SIZE + header.payload_length as usize {
            return None;
        }

        let payload = data[VoicePacketHeader::SIZE..VoicePacketHeader::SIZE + header.payload_length as usize].to_vec();
        
        Some(Self { header, payload })
    }
}

// ============================================================================
// VOICE ACTIVITY DETECTION
// ============================================================================

/// Voice Activity Detection using RMS energy analysis
#[derive(Debug)]
pub struct VoiceActivityDetector {
    /// Energy threshold for voice detection
    threshold: f32,
    /// Consecutive silent frame counter
    silence_frames: u32,
    /// Whether voice is currently detected
    is_speaking: bool,
    /// Smoothed energy level for hysteresis
    smoothed_energy: f32,
    /// Smoothing factor (0.0-1.0)
    smoothing_factor: f32,
}

impl VoiceActivityDetector {
    /// Create a new VAD instance
    pub fn new(threshold: f32) -> Self {
        Self {
            threshold,
            silence_frames: 0,
            is_speaking: false,
            smoothed_energy: 0.0,
            smoothing_factor: 0.1,
        }
    }

    /// Calculate RMS energy of audio samples
    fn calculate_rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let sum_squares: f32 = samples.iter().map(|s| s * s).sum();
        (sum_squares / samples.len() as f32).sqrt()
    }

    /// Process audio frame and detect voice activity
    ///
    /// Returns true if voice activity is detected
    pub fn process(&mut self, samples: &[f32]) -> bool {
        let current_energy = Self::calculate_rms(samples);
        
        // Apply exponential smoothing
        self.smoothed_energy = self.smoothing_factor * current_energy 
            + (1.0 - self.smoothing_factor) * self.smoothed_energy;

        if self.smoothed_energy > self.threshold {
            self.silence_frames = 0;
            self.is_speaking = true;
        } else {
            self.silence_frames += 1;
            if self.silence_frames > SILENCE_FRAMES_THRESHOLD {
                self.is_speaking = false;
            }
        }

        self.is_speaking
    }

    /// Check if currently speaking
    pub fn is_speaking(&self) -> bool {
        self.is_speaking
    }

    /// Get current smoothed energy level
    pub fn get_energy(&self) -> f32 {
        self.smoothed_energy
    }
}

// ============================================================================
// JITTER BUFFER
// ============================================================================

/// Jitter buffer entry with timing information
#[derive(Debug, Clone)]
struct JitterBufferEntry {
    packet: VoicePacket,
    arrival_time: Instant,
}

/// Adaptive jitter buffer for smooth audio playback
///
/// Handles network jitter by buffering packets and
/// reordering them based on sequence numbers
#[derive(Debug)]
pub struct JitterBuffer {
    /// Buffer storage ordered by sequence number
    buffer: VecDeque<JitterBufferEntry>,
    /// Target latency in milliseconds
    target_latency_ms: u64,
    /// Current estimated jitter
    estimated_jitter: f32,
    /// Last played sequence number
    last_sequence: Option<u16>,
    /// Maximum buffer size
    max_size: usize,
    /// Statistics: total packets received
    packets_received: u64,
    /// Statistics: packets dropped due to late arrival
    packets_dropped: u64,
}

impl JitterBuffer {
    /// Create a new jitter buffer
    pub fn new(target_latency_ms: u64, max_size: usize) -> Self {
        Self {
            buffer: VecDeque::with_capacity(max_size),
            target_latency_ms,
            estimated_jitter: 0.0,
            last_sequence: None,
            max_size,
            packets_received: 0,
            packets_dropped: 0,
        }
    }

    /// Insert a packet into the jitter buffer
    pub fn push(&mut self, packet: VoicePacket) {
        self.packets_received += 1;
        
        let sequence = packet.header.sequence;
        
        // Check if packet is too old (already played)
        if let Some(last_seq) = self.last_sequence {
            let seq_diff = sequence.wrapping_sub(last_seq);
            if seq_diff > 32768 {
                // Packet is older than last played, drop it
                self.packets_dropped += 1;
                return;
            }
        }

        let entry = JitterBufferEntry {
            packet,
            arrival_time: Instant::now(),
        };

        // Insert in sequence order
        let insert_pos = self.buffer
            .iter()
            .position(|e| e.packet.header.sequence > sequence)
            .unwrap_or(self.buffer.len());

        self.buffer.insert(insert_pos, entry);

        // Trim if buffer exceeds max size
        while self.buffer.len() > self.max_size {
            self.buffer.pop_front();
            self.packets_dropped += 1;
        }
    }

    /// Get the next packet to play, if available
    ///
    /// Returns None if buffer is empty or hasn't reached target latency
    pub fn pop(&mut self) -> Option<VoicePacket> {
        if self.buffer.is_empty() {
            return None;
        }

        let entry = self.buffer.front()?;
        let elapsed = entry.arrival_time.elapsed();

        // Wait until target latency is reached
        if elapsed.as_millis() < self.target_latency_ms as u128 {
            return None;
        }

        let entry = self.buffer.pop_front()?;
        self.last_sequence = Some(entry.packet.header.sequence);
        Some(entry.packet)
    }

    /// Get current buffer depth in packets
    pub fn depth(&self) -> usize {
        self.buffer.len()
    }

    /// Get packet loss statistics
    pub fn get_stats(&self) -> (u64, u64) {
        (self.packets_received, self.packets_dropped)
    }

    /// Clear the buffer
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.last_sequence = None;
    }
}

// ============================================================================
// VOICE SESSION STATE
// ============================================================================

/// Voice state for a user in a room
#[derive(Debug, Clone)]
pub struct VoiceUserState {
    pub user_id: u16,
    pub username: String,
    pub room_id: u16,
    pub is_muted: bool,
    pub is_deafened: bool,
    pub is_speaking: bool,
    pub socket_addr: Option<SocketAddr>,
    pub last_heartbeat: Instant,
    pub sequence_number: u16,
    pub audio_timestamp: u32,
}

impl VoiceUserState {
    /// Create a new voice user state
    pub fn new(user_id: u16, username: String, room_id: u16, socket_addr: SocketAddr) -> Self {
        Self {
            user_id,
            username,
            room_id,
            is_muted: false,
            is_deafened: false,
            is_speaking: false,
            socket_addr: Some(socket_addr),
            last_heartbeat: Instant::now(),
            sequence_number: 0,
            audio_timestamp: 0,
        }
    }

    /// Update heartbeat timestamp
    pub fn heartbeat(&mut self) {
        self.last_heartbeat = Instant::now();
    }

    /// Check if session is expired
    pub fn is_expired(&self, timeout: Duration) -> bool {
        self.last_heartbeat.elapsed() > timeout
    }

    /// Get next sequence number
    pub fn next_sequence(&mut self) -> u16 {
        let seq = self.sequence_number;
        self.sequence_number = self.sequence_number.wrapping_add(1);
        seq
    }

    /// Update audio timestamp
    pub fn advance_timestamp(&mut self, samples: u32) {
        self.audio_timestamp = self.audio_timestamp.wrapping_add(samples);
    }
}

// ============================================================================
// OPUS CODEC WRAPPER
// ============================================================================

/// Opus encoder wrapper with error handling
pub struct OpusEncoder {
    encoder: opus::Encoder,
}

impl OpusEncoder {
    /// Create a new Opus encoder
    pub fn new() -> Result<Self, opus::Error> {
        let mut encoder = opus::Encoder::new(
            SAMPLE_RATE,
            opus::Channels::Mono,
            opus::Application::Voip,
        )?;
        
        // Configure encoder for voice chat
        encoder.set_bitrate(opus::Bitrate::Bits(OPUS_BITRATE))?;
        encoder.set_vbr(true)?;
        encoder.set_inband_fec(true)?;
        encoder.set_packet_loss_perc(5)?;
        
        Ok(Self { encoder })
    }

    /// Encode audio samples to Opus
    ///
    /// # Arguments
    /// * `samples` - PCM audio samples (mono, 48kHz)
    ///
    /// # Returns
    /// Encoded Opus data or error
    pub fn encode(&mut self, samples: &[f32]) -> Result<Vec<u8>, opus::Error> {
        let mut output = vec![0u8; MAX_PACKET_SIZE];
        let len = self.encoder.encode_float(samples, &mut output)?;
        output.truncate(len);
        Ok(output)
    }
}

/// Opus decoder wrapper with error handling
pub struct OpusDecoder {
    decoder: opus::Decoder,
}

impl OpusDecoder {
    /// Create a new Opus decoder
    pub fn new() -> Result<Self, opus::Error> {
        let decoder = opus::Decoder::new(SAMPLE_RATE, opus::Channels::Mono)?;
        Ok(Self { decoder })
    }

    /// Decode Opus data to audio samples
    ///
    /// # Arguments
    /// * `data` - Encoded Opus data
    ///
    /// # Returns
    /// Decoded PCM samples or error
    pub fn decode(&mut self, data: &[u8]) -> Result<Vec<f32>, opus::Error> {
        let mut output = vec![0.0f32; FRAME_SIZE];
        let len = self.decoder.decode_float(data, &mut output, false)?;
        output.truncate(len);
        Ok(output)
    }

    /// Decode with packet loss concealment (for lost packets)
    pub fn decode_loss(&mut self) -> Result<Vec<f32>, opus::Error> {
        let mut output = vec![0.0f32; FRAME_SIZE];
        let len = self.decoder.decode_float(&[], &mut output, true)?;
        output.truncate(len);
        Ok(output)
    }
}

// ============================================================================
// VOICE ROOM MANAGER
// ============================================================================

/// Manages voice sessions for a single room
#[derive(Debug)]
pub struct VoiceRoom {
    pub room_id: u16,
    pub users: HashMap<u16, VoiceUserState>,
}

impl VoiceRoom {
    /// Create a new voice room
    pub fn new(room_id: u16) -> Self {
        Self {
            room_id,
            users: HashMap::new(),
        }
    }

    /// Add a user to the voice room
    pub fn add_user(&mut self, state: VoiceUserState) {
        self.users.insert(state.user_id, state);
    }

    /// Remove a user from the voice room
    pub fn remove_user(&mut self, user_id: u16) -> Option<VoiceUserState> {
        self.users.remove(&user_id)
    }

    /// Get user by ID
    pub fn get_user(&self, user_id: u16) -> Option<&VoiceUserState> {
        self.users.get(&user_id)
    }

    /// Get mutable user by ID
    pub fn get_user_mut(&mut self, user_id: u16) -> Option<&mut VoiceUserState> {
        self.users.get_mut(&user_id)
    }

    /// Get all socket addresses for users (excluding sender)
    pub fn get_recipient_addrs(&self, exclude_user_id: u16) -> Vec<SocketAddr> {
        self.users
            .iter()
            .filter(|(id, state)| {
                **id != exclude_user_id && !state.is_deafened && state.socket_addr.is_some()
            })
            .filter_map(|(_, state)| state.socket_addr)
            .collect()
    }

    /// Check if room is empty
    pub fn is_empty(&self) -> bool {
        self.users.is_empty()
    }

    /// Get number of users
    pub fn user_count(&self) -> usize {
        self.users.len()
    }
}

// ============================================================================
// VOICE SERVER
// ============================================================================

/// Voice server state shared across handlers
pub struct VoiceServerState {
    /// Active voice rooms
    pub rooms: HashMap<u16, VoiceRoom>,
    /// Token to user ID mapping
    pub pending_tokens: HashMap<String, (u16, u16)>, // token -> (user_id, room_id)
    /// Socket address to user ID mapping
    pub addr_to_user: HashMap<SocketAddr, (u16, u16)>, // addr -> (user_id, room_id)
}

impl VoiceServerState {
    /// Create new voice server state
    pub fn new() -> Self {
        Self {
            rooms: HashMap::new(),
            pending_tokens: HashMap::new(),
            addr_to_user: HashMap::new(),
        }
    }

    /// Register a pending voice token
    pub fn register_token(&mut self, token: String, user_id: u16, room_id: u16) {
        self.pending_tokens.insert(token, (user_id, room_id));
    }

    /// Validate and consume a token
    pub fn validate_token(&mut self, token: &str) -> Option<(u16, u16)> {
        self.pending_tokens.remove(token)
    }
}

impl Default for VoiceServerState {
    fn default() -> Self {
        Self::new()
    }
}

/// Enhanced voice server with room-based routing
pub struct VoiceServer {
    state: Arc<RwLock<VoiceServerState>>,
}

impl VoiceServer {
    /// Create a new voice server
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(VoiceServerState::new())),
        }
    }

    /// Get shared state reference
    pub fn state(&self) -> Arc<RwLock<VoiceServerState>> {
        Arc::clone(&self.state)
    }

    /// Start the voice server
    pub async fn start(
        &self,
        bind_address: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let socket = UdpSocket::bind(bind_address).await?;
        println!("[Voice] Server listening on {}", bind_address);

        let socket = Arc::new(socket);
        let mut buf = [0u8; MAX_PACKET_SIZE];

        loop {
            match socket.recv_from(&mut buf).await {
                Ok((size, peer_addr)) => {
                    let data = buf[..size].to_vec();
                    let socket_clone = Arc::clone(&socket);
                    let state_clone = Arc::clone(&self.state);

                    // Handle packet asynchronously
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_packet(
                            state_clone,
                            socket_clone,
                            data,
                            peer_addr,
                        ).await {
                            eprintln!("[Voice] Packet handling error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    eprintln!("[Voice] Receive error: {}", e);
                }
            }
        }
    }

    /// Handle incoming voice packet
    async fn handle_packet(
        state: Arc<RwLock<VoiceServerState>>,
        socket: Arc<UdpSocket>,
        data: Vec<u8>,
        peer_addr: SocketAddr,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Check if this is a token authentication
        if let Ok(token_str) = std::str::from_utf8(&data) {
            let token = token_str.trim();
            
            let mut state_guard = state.write().await;
            
            if let Some((user_id, room_id)) = state_guard.validate_token(token) {
                // Create voice room if it doesn't exist
                let room = state_guard.rooms
                    .entry(room_id)
                    .or_insert_with(|| VoiceRoom::new(room_id));

                // Add user to room
                let user_state = VoiceUserState::new(
                    user_id,
                    format!("User_{}", user_id),
                    room_id,
                    peer_addr,
                );
                room.add_user(user_state);

                // Register address mapping
                state_guard.addr_to_user.insert(peer_addr, (user_id, room_id));

                drop(state_guard);

                // Send acknowledgment
                socket.send_to(b"VOICE_CONNECTED", peer_addr).await?;
                println!("[Voice] User {} connected to room {}", user_id, room_id);
                
                return Ok(());
            }
        }

        // Try to decode as voice packet
        if let Some(packet) = VoicePacket::decode(&data) {
            let state_guard = state.read().await;

            // Verify sender is registered
            if let Some((user_id, room_id)) = state_guard.addr_to_user.get(&peer_addr) {
                // Get recipients
                if let Some(room) = state_guard.rooms.get(room_id) {
                    let recipients = room.get_recipient_addrs(*user_id);
                    
                    drop(state_guard);

                    // Forward packet to all recipients
                    let encoded = packet.encode();
                    for addr in recipients {
                        let _ = socket.send_to(&encoded, addr).await;
                    }
                }
            }
        } else {
            // Legacy: raw audio forwarding for backwards compatibility
            let state_guard = state.read().await;
            
            if let Some((user_id, room_id)) = state_guard.addr_to_user.get(&peer_addr) {
                if let Some(room) = state_guard.rooms.get(room_id) {
                    let recipients = room.get_recipient_addrs(*user_id);
                    
                    drop(state_guard);

                    // Forward raw data
                    for addr in recipients {
                        let _ = socket.send_to(&data, addr).await;
                    }
                }
            }
        }

        Ok(())
    }

    /// Cleanup expired sessions
    pub async fn cleanup_expired_sessions(&self, timeout: Duration) {
        let mut state = self.state.write().await;
        let mut to_remove: Vec<(u16, u16, SocketAddr)> = Vec::new();

        for (room_id, room) in state.rooms.iter_mut() {
            for (user_id, user_state) in room.users.iter() {
                if user_state.is_expired(timeout) {
                    if let Some(addr) = user_state.socket_addr {
                        to_remove.push((*room_id, *user_id, addr));
                    }
                }
            }
        }

        for (room_id, user_id, addr) in to_remove {
            if let Some(room) = state.rooms.get_mut(&room_id) {
                room.remove_user(user_id);
                state.addr_to_user.remove(&addr);
                println!("[Voice] User {} expired from room {}", user_id, room_id);
            }
        }

        // Remove empty rooms
        state.rooms.retain(|_, room| !room.is_empty());
    }
}

impl Default for VoiceServer {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// VOICE CLIENT
// ============================================================================

/// Voice client for sending and receiving audio
pub struct VoiceClient {
    /// Opus encoder for outgoing audio
    encoder: OpusEncoder,
    /// Opus decoder for incoming audio
    decoder: OpusDecoder,
    /// Voice activity detector
    vad: VoiceActivityDetector,
    /// Jitter buffer for playback
    jitter_buffer: JitterBuffer,
    /// Current voice state
    pub is_muted: bool,
    pub is_deafened: bool,
    /// Push-to-talk mode
    pub push_to_talk: bool,
    pub ptt_active: bool,
    /// User and room IDs
    pub user_id: u16,
    pub room_id: u16,
    /// Sequence number counter
    sequence: u16,
    /// Audio timestamp counter
    timestamp: u32,
}

impl VoiceClient {
    /// Create a new voice client
    pub fn new(user_id: u16, room_id: u16) -> Result<Self, opus::Error> {
        Ok(Self {
            encoder: OpusEncoder::new()?,
            decoder: OpusDecoder::new()?,
            vad: VoiceActivityDetector::new(VAD_THRESHOLD),
            jitter_buffer: JitterBuffer::new(JITTER_BUFFER_TARGET_MS, 50),
            is_muted: false,
            is_deafened: false,
            push_to_talk: false,
            ptt_active: false,
            user_id,
            room_id,
            sequence: 0,
            timestamp: 0,
        })
    }

    /// Process outgoing audio and create voice packet
    ///
    /// Returns None if audio should not be transmitted (muted, no voice activity)
    pub fn process_outgoing(&mut self, samples: &[f32]) -> Option<VoicePacket> {
        // Check mute state
        if self.is_muted {
            return None;
        }

        // Check push-to-talk
        if self.push_to_talk && !self.ptt_active {
            return None;
        }

        // Voice activity detection
        if !self.vad.process(samples) {
            return None;
        }

        // Encode audio
        let encoded = self.encoder.encode(samples).ok()?;

        // Create packet header
        let header = VoicePacketHeader::new_audio(
            self.user_id,
            self.room_id,
            self.next_sequence(),
            self.timestamp,
            encoded.len() as u16,
        );

        // Update timestamp
        self.timestamp = self.timestamp.wrapping_add(samples.len() as u32);

        Some(VoicePacket::new(header, encoded))
    }

    /// Process incoming voice packet
    ///
    /// Returns decoded audio samples
    pub fn process_incoming(&mut self, packet: VoicePacket) -> Option<Vec<f32>> {
        // Check deafen state
        if self.is_deafened {
            return None;
        }

        // Add to jitter buffer
        self.jitter_buffer.push(packet);

        // Try to get packet from buffer
        let ready_packet = self.jitter_buffer.pop()?;

        // Decode audio
        self.decoder.decode(&ready_packet.payload).ok()
    }

    /// Check if currently speaking
    pub fn is_speaking(&self) -> bool {
        self.vad.is_speaking()
    }

    /// Get next sequence number
    fn next_sequence(&mut self) -> u16 {
        let seq = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);
        seq
    }

    /// Get jitter buffer statistics
    pub fn get_stats(&self) -> (u64, u64) {
        self.jitter_buffer.get_stats()
    }

    /// Clear jitter buffer (for disconnect/reconnect)
    pub fn reset(&mut self) {
        self.jitter_buffer.clear();
        self.sequence = 0;
        self.timestamp = 0;
    }
}

// ============================================================================
// AUDIO MIXER
// ============================================================================

/// Simple audio mixer for combining multiple audio streams
pub struct AudioMixer {
    /// Maximum number of streams to mix
    max_streams: usize,
    /// Master volume (0.0-1.0)
    master_volume: f32,
}

impl AudioMixer {
    /// Create a new audio mixer
    pub fn new(max_streams: usize) -> Self {
        Self {
            max_streams,
            master_volume: 1.0,
        }
    }

    /// Set master volume
    pub fn set_volume(&mut self, volume: f32) {
        self.master_volume = volume.clamp(0.0, 1.0);
    }

    /// Mix multiple audio streams together
    ///
    /// Uses simple averaging with limiting to prevent clipping
    pub fn mix(&self, streams: &[Vec<f32>]) -> Vec<f32> {
        if streams.is_empty() {
            return Vec::new();
        }

        let max_len = streams.iter().map(|s| s.len()).max().unwrap_or(0);
        let mut output = vec![0.0f32; max_len];
        let num_streams = streams.len().min(self.max_streams) as f32;

        for stream in streams.iter().take(self.max_streams) {
            for (i, &sample) in stream.iter().enumerate() {
                if i < output.len() {
                    output[i] += sample / num_streams;
                }
            }
        }

        // Apply master volume and soft limiting
        for sample in output.iter_mut() {
            *sample *= self.master_volume;
            // Soft clipping to prevent harsh distortion
            *sample = (*sample).tanh();
        }

        output
    }
}

impl Default for AudioMixer {
    fn default() -> Self {
        Self::new(10)
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packet_encode_decode() {
        let header = VoicePacketHeader::new_audio(1, 2, 42, 1234, 100);
        let payload = vec![1u8; 100];
        let packet = VoicePacket::new(header, payload.clone());

        let encoded = packet.encode();
        let decoded = VoicePacket::decode(&encoded).unwrap();

        assert_eq!(decoded.header.user_id, 1);
        assert_eq!(decoded.header.room_id, 2);
        assert_eq!(decoded.header.sequence, 42);
        assert_eq!(decoded.header.timestamp, 1234);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn test_vad() {
        let mut vad = VoiceActivityDetector::new(0.01);
        
        // Silent audio
        let silence = vec![0.0f32; 960];
        for _ in 0..30 {
            vad.process(&silence);
        }
        assert!(!vad.is_speaking());

        // Loud audio
        let loud = vec![0.5f32; 960];
        vad.process(&loud);
        assert!(vad.is_speaking());
    }

    #[test]
    fn test_jitter_buffer() {
        let mut buffer = JitterBuffer::new(0, 10);

        let header1 = VoicePacketHeader::new_audio(1, 1, 0, 0, 10);
        let header2 = VoicePacketHeader::new_audio(1, 1, 1, 960, 10);
        
        // Insert out of order
        buffer.push(VoicePacket::new(header2.clone(), vec![2; 10]));
        buffer.push(VoicePacket::new(header1.clone(), vec![1; 10]));

        // Should come out in order
        let p1 = buffer.pop().unwrap();
        assert_eq!(p1.header.sequence, 0);
        
        let p2 = buffer.pop().unwrap();
        assert_eq!(p2.header.sequence, 1);
    }

    #[test]
    fn test_audio_mixer() {
        let mixer = AudioMixer::new(10);
        
        let stream1 = vec![0.5f32; 960];
        let stream2 = vec![0.3f32; 960];
        
        let mixed = mixer.mix(&[stream1, stream2]);
        
        assert_eq!(mixed.len(), 960);
        // Average of 0.5 and 0.3 = 0.4, tanh(0.4) ≈ 0.38
        assert!((mixed[0] - 0.38).abs() < 0.02);
    }
}
