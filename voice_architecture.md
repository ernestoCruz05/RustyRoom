# Voice Transmission Architecture for RustyRoom

## Overview
This document outlines the architecture design for implementing voice transmission capabilities in the RustyRoom chat application. The voice feature will enable real-time audio communication between users in the same chat rooms.

## Current Architecture Analysis

The existing RustyRoom application has:
- **Async TCP Server** with SQLite persistence
- **Message-based Protocol** using JSON over TCP
- **Room-based Chat System** with user authentication
- **TUI and CLI Clients** with real-time messaging
- **Database Schema** for users, rooms, messages, and sessions

## Proposed Voice Architecture

### High-Level Architecture Diagram

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                           RUSTYROOM VOICE ARCHITECTURE                          │
└─────────────────────────────────────────────────────────────────────────────────┘

┌──────────────────┐    ┌──────────────────┐    ┌──────────────────┐
│   CLIENT A       │    │   CLIENT B       │    │   CLIENT C       │
│                  │    │                  │    │                  │
│ ┌──────────────┐ │    │ ┌──────────────┐ │    │ ┌──────────────┐ │
│ │ Audio Input  │ │    │ │ Audio Input  │ │    │ │ Audio Input  │ │
│ │ (Microphone) │ │    │ │ (Microphone) │ │    │ │ (Microphone) │ │
│ └──────────────┘ │    │ └──────────────┘ │    │ └──────────────┘ │
│        │         │    │        │         │    │        │         │
│        ▼         │    │        ▼         │    │        ▼         │
│ ┌──────────────┐ │    │ ┌──────────────┐ │    │ ┌──────────────┐ │
│ │   Encoder    │ │    │ │   Encoder    │ │    │ │   Encoder    │ │
│ │    (Opus)    │ │    │ │    (Opus)    │ │    │ │    (Opus)    │ │
│ └──────────────┘ │    │ └──────────────┘ │    │ └──────────────┘ │
│        │         │    │        │         │    │        │         │
│        ▼         │    │        ▼         │    │        ▼         │
│ ┌──────────────┐ │    │ ┌──────────────┐ │    │ ┌──────────────┐ │
│ │   Voice      │ │    │ │   Voice      │ │    │ │   Voice      │ │
│ │   Client     │ │    │ │   Client     │ │    │ │   Client     │ │
│ │   Manager    │ │    │ │   Manager    │ │    │ │   Manager    │ │
│ └──────────────┘ │    │ └──────────────┘ │    │ └──────────────┘ │
│        │         │    │        │         │    │        │         │
└────────┼─────────┘    └────────┼─────────┘    └────────┼─────────┘
         │                       │                       │
         │ UDP Audio Packets     │ UDP Audio Packets     │ UDP Audio Packets
         │                       │                       │
         ▼                       ▼                       ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│                              SERVER CLUSTER                                     │
│                                                                                 │
│  ┌─────────────────┐              ┌─────────────────┐                          │
│  │  CHAT SERVER    │              │  VOICE SERVER   │                          │
│  │                 │◄────────────►│                 │                          │
│  │ ┌─────────────┐ │   Control    │ ┌─────────────┐ │                          │
│  │ │   User      │ │   Channel    │ │   Voice     │ │                          │
│  │ │   Auth      │ │   (TCP)      │ │   Session   │ │                          │
│  │ │   Manager   │ │              │ │   Manager   │ │                          │
│  │ └─────────────┘ │              │ └─────────────┘ │                          │
│  │                 │              │        │        │                          │
│  │ ┌─────────────┐ │              │        ▼        │                          │
│  │ │   Room      │ │              │ ┌─────────────┐ │                          │
│  │ │   Manager   │ │              │ │   Audio     │ │                          │
│  │ └─────────────┘ │              │ │   Mixer/    │ │                          │
│  │                 │              │ │   Router    │ │                          │
│  │ ┌─────────────┐ │              │ └─────────────┘ │                          │
│  │ │  Message    │ │              │        │        │                          │
│  │ │  Broker     │ │              │        ▼        │                          │
│  │ └─────────────┘ │              │ ┌─────────────┐ │                          │
│  │                 │              │ │   Packet    │ │                          │
│  │ ┌─────────────┐ │              │ │   Buffer    │ │                          │
│  │ │  Database   │ │              │ │   Manager   │ │                          │
│  │ │  Interface  │ │              │ └─────────────┘ │                          │
│  │ └─────────────┘ │              │                 │                          │
│  └─────────────────┘              └─────────────────┘                          │
│           │                                 │                                  │
│           ▼                                 ▼                                  │
│  ┌─────────────────┐              ┌─────────────────┐                          │
│  │    SQLITE       │              │   VOICE STATE   │                          │
│  │   DATABASE      │              │     CACHE       │                          │
│  │                 │              │    (Redis)      │                          │
│  │ • Users         │              │                 │                          │
│  │ • Rooms         │              │ • Active Voice  │                          │
│  │ • Messages      │              │   Sessions      │                          │
│  │ • Sessions      │              │ • Room Audio    │                          │
│  │ • Voice Logs    │              │   State         │                          │
│  └─────────────────┘              │ • Quality       │                          │
│                                   │   Metrics       │                          │
│                                   └─────────────────┘                          │
└─────────────────────────────────────────────────────────────────────────────────┘
```

## Component Architecture

### 1. Client-Side Components

#### Voice Client Manager
```rust
pub struct VoiceClientManager {
    pub audio_input: AudioInputHandler,
    pub audio_output: AudioOutputHandler,
    pub encoder: OpusEncoder,
    pub decoder: OpusDecoder,
    pub network_manager: VoiceNetworkManager,
    pub room_id: Option<u16>,
    pub is_muted: bool,
    pub is_deafened: bool,
}
```

#### Audio Pipeline
```
Microphone → Audio Input → Noise Suppression → Encoding (Opus) → Network Layer
                ↓
Network Layer → Decoding (Opus) → Audio Mixing → Audio Output → Speakers
```

### 2. Server-Side Components

#### Voice Server Architecture
```rust
pub struct VoiceServer {
    pub session_manager: VoiceSessionManager,
    pub audio_router: AudioRouter,
    pub room_voice_manager: RoomVoiceManager,
    pub quality_manager: QualityManager,
    pub state_cache: VoiceStateCache,
}
```

#### Audio Flow Control
```
UDP Packets → Packet Validator → Buffer Manager → Audio Mixer → Distribution Engine
                     ↓
Quality Monitor → Adaptive Bitrate → Jitter Buffer → Echo Cancellation
```

## Protocol Design

### 3. Voice Control Messages (TCP)

```rust
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum VoiceMessageType {
    // Connection Management
    VoiceConnect { room_id: u16, user_id: u16 },
    VoiceDisconnect { room_id: u16, user_id: u16 },
    VoiceConnectionEstablished { udp_port: u16, session_token: String },
    
    // Audio Control
    SetMuted { is_muted: bool },
    SetDeafened { is_deafened: bool },
    SetVolume { volume: f32 },
    
    // Room Voice Management
    RoomVoiceState { room_id: u16, users: Vec<VoiceUserState> },
    UserVoiceUpdate { user_id: u16, is_speaking: bool, is_muted: bool },
    
    // Quality Control
    RequestBitrateChange { target_bitrate: u32 },
    AudioQualityReport { packet_loss: f32, latency: u32, jitter: u32 },
    
    // Errors
    VoiceError { code: VoiceErrorCode, message: String },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VoiceUserState {
    pub user_id: u16,
    pub username: String,
    pub is_speaking: bool,
    pub is_muted: bool,
    pub is_deafened: bool,
    pub volume: f32,
}
```

### 4. Audio Packet Format (UDP)

```
┌─────────────────────────────────────────────────────────────┐
│                    AUDIO PACKET HEADER                      │
├─────────┬─────────┬─────────┬─────────┬─────────┬───────────┤
│ Version │ Type    │ User ID │ Room ID │ Seq Num │ Timestamp │
│ (1 bit) │ (3 bit) │ (16 bit)│ (16 bit)│ (16 bit)│ (32 bit)  │
├─────────┼─────────┼─────────┼─────────┼─────────┼───────────┤
│ Payload Length   │ Opus Data Length  │    Checksum        │
│    (16 bit)      │     (16 bit)      │    (32 bit)        │
└─────────┴─────────┴─────────┴─────────┴─────────┴───────────┘
│                    OPUS ENCODED AUDIO DATA                  │
│                      (Variable Length)                      │
└─────────────────────────────────────────────────────────────┘
```

## Network Architecture

### 5. Dual Protocol Design

```
┌────────────────────────────────────────────────────────────┐
│                    NETWORK PROTOCOLS                       │
│                                                            │
│  ┌─────────────────┐           ┌─────────────────┐         │
│  │   TCP Channel   │           │   UDP Channel   │         │
│  │   (Port 8080)   │           │  (Port 8081)    │         │
│  │                 │           │                 │         │
│  │ • Authentication│           │ • Audio Packets │         │
│  │ • Room Control  │           │ • Low Latency   │         │
│  │ • Voice Setup   │           │ • High Volume   │         │
│  │ • Text Messages │           │ • Real-time     │         │
│  │ • Status Updates│           │ • Unreliable    │         │
│  │ • Error Handling│           │ • Fast Delivery │         │
│  └─────────────────┘           └─────────────────┘         │
│          │                             │                  │
│          └─────────────┬───────────────┘                  │
│                        │                                  │
│                 ┌─────────────┐                           │
│                 │  Coordination │                         │
│                 │   Service     │                         │
│                 └─────────────┘                           │
└────────────────────────────────────────────────────────────┘
```

## Data Flow Diagrams

### 6. Voice Session Establishment

```
Client A                    Chat Server                 Voice Server
   │                           │                           │
   │ 1. VoiceConnect           │                           │
   ├──────────────────────────►│                           │
   │                           │ 2. Validate User/Room    │
   │                           ├──────────────────────────►│
   │                           │                           │
   │                           │ 3. Create Voice Session  │
   │                           │◄──────────────────────────┤
   │                           │                           │
   │ 4. VoiceConnectionEst.    │                           │
   │◄──────────────────────────┤                           │
   │                           │                           │
   │ 5. UDP Handshake          │                           │
   ├───────────────────────────┼──────────────────────────►│
   │                           │                           │
   │ 6. Start Audio Stream     │                           │
   ├───────────────────────────┼──────────────────────────►│
```

### 7. Audio Mixing and Distribution

```
       User A Audio              Voice Server             User B & C Output
           │                         │                         │
           │ 1. Opus Packets         │                         │
           ├────────────────────────►│                         │
           │                         │ 2. Decode               │
           │                         ├──────┐                  │
           │                         │      │                  │
           │                         │◄─────┘                  │
           │                         │ 3. Mix with Room Audio  │
           │                         ├──────┐                  │
           │                         │      │                  │
           │                         │◄─────┘                  │
           │                         │ 4. Encode for Each User │
           │                         ├──────┐                  │
           │                         │      │                  │
           │                         │◄─────┘                  │
           │                         │ 5. Distribute           │
           │                         ├─────────────────────────►│
```

## Implementation Phases

### Phase 1: Foundation (Long)
-  Voice message types and protocol design
-  UDP server implementation for audio packets
-  Basic audio capture and playback using `cpal` crate
-  Opus encoding/decoding integration using `opus` crate
-  Voice session management in existing server

### Phase 2: Core Voice Features (Extra Long)
- Real-time audio transmission over UDP
- Basic audio mixing for multiple users in a room
- Voice state management (mute, deafen, speaking indicators)
- Integration with existing TUI client for voice controls
- Quality monitoring and basic adaptive bitrate

### Phase 3: Advanced Features (Medium)
- Noise suppression and echo cancellation
- Jitter buffer implementation for smooth playback
- Voice activity detection (VAD)
- Push-to-talk and voice-activated recording
- Room-based audio permissions and moderation

### Phase 4: Optimization (Freely long)
- Performance optimization for low-latency audio
- Bandwidth optimization and compression improvements
- Stress testing with multiple concurrent users
- Voice quality metrics and monitoring
- Documentation and deployment guides

## Technical Considerations

### Rust Crates Required
```toml
[dependencies]
# Existing dependencies...
cpal = "0.15"           # Cross-platform audio I/O
opus = "0.3"            # Opus audio codec
rubato = "0.14"         # Audio resampling
hound = "3.5"           # WAV file I/O (for testing)
tokio-util = "0.7"      # UDP codec utilities
bytes = "1.5"           # Efficient byte manipulation
ringbuffer = "0.15"     # Circular buffers for audio
```

### Performance Requirements
- **Latency**: < 50ms end-to-end for local networks
- **Bandwidth**: 32-64 kbps per voice stream (Opus compressed)
- **CPU Usage**: < 5% per active voice connection
- **Memory**: < 10MB per active voice session
- **Concurrent Users**: Support 50+ simultaneous voice users per room
- **Chat Server Delayage**: Can't lag the current Chat Server during Query searches and query repopulation

### Quality Settings
```rust
pub struct AudioQualityConfig {
    pub sample_rate: u32,        // 48000 Hz (standard)
    pub channels: u16,           // 1 (mono) or 2 (stereo)
    pub frame_size: u16,         // 960 samples (20ms at 48kHz)
    pub bitrate: u32,            // 32-128 kbps
    pub complexity: u8,          // 0-10 (Opus complexity)
    pub use_vbr: bool,           // Variable bitrate
    pub use_fec: bool,           // Forward error correction
}
```

## Security Considerations

### Voice Security Features
- **Authentication**: Voice sessions tied to authenticated chat users
- **Room Permissions**: Voice access controlled by room membership
- **Encryption**: Optional TLS/DTLS for UDP audio streams
- **Rate Limiting**: Prevent audio spam and DoS attacks
- **Quality Controls**: Bandwidth limiting per user/room

### Privacy Features
- **Push-to-Talk**: Default mode to prevent accidental transmission
- **Local Recording Control**: User consent for any audio recording
- **Voice Data Retention**: Configurable audio log retention policies
- **User Permissions**: Granular control over voice features per user

## Monitoring and Metrics

### Voice Analytics
```rust
pub struct VoiceMetrics {
    pub active_sessions: u32,
    pub total_bandwidth_usage: u64,
    pub average_latency: u32,
    pub packet_loss_rate: f32,
    pub audio_quality_score: f32,
    pub concurrent_speakers: u32,
}
```
