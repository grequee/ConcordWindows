export type LinkStatus = 'disconnected' | 'connecting' | 'waiting_for_server' | 'reconnecting' | 'connected';

export interface UiError {
  message: string;
  code: string | null;
  fatal: boolean;
}

export interface RoomSummary {
  id: string;
  name: string;
  description: string | null;
  member_count: number;
  capacity: number;
}

export interface Participant {
  user_id: string;
  display_name: string;
  muted: boolean;
  deafened: boolean;
  screen_sharing: boolean;
  in_call: boolean;
  joined_at_ms: number;
}

export type CallState = 'joining' | 'active' | 'reconnecting';
export type CallPeerState = 'connecting' | 'connected' | 'reconnecting' | 'failed';
export type ScreenTier = '720p30' | '720p60' | '1080p30' | '1080p60';

export interface CallAudioInfo {
  speaking: boolean;
  microphone_level: number;
  microphone_clipped: boolean;
  capture_underruns: number;
  playback_underruns: number;
  playback_dropped: number;
  stream_errors: number;
  tx_frames: number;
  rx_frames: number;
}

export interface CallPeerInfo {
  speaking: boolean;
  audio_level: number;
  volume_percent: number;
  user_id: string;
  display_name: string;
  state: CallPeerState;
  packets_received: number;
  packets_lost: number;
  frames_concealed: number;
  queued_frames: number;
  screen_sharing: boolean;
  video_frames: number;
  video_lost: number;
  video_decode_errors: number;
}

export interface CallVideoInfo {
  tx_frames: number;
  tx_keyframes: number;
  rx_frames: number;
  keyframe_requests: number;
  decode_errors: number;
}

export interface CallInfo {
  id: string;
  room_id: string;
  state: CallState;
  muted: boolean;
  deafened: boolean;
  screen_sharing: boolean;
  screen_error: string | null;
  screen_tier: ScreenTier;
  peers: CallPeerInfo[];
  audio: CallAudioInfo;
  video: CallVideoInfo;
}

export interface ChatMessage {
  author_id?: string | null;
  reply_to?: number | null;
  reply?: { id: number; display_name: string; text: string } | null;
  edited_at_ms?: number | null;
  attachment?: { name: string; bytes: number } | null;
  image?: { content_type: string; bytes: number; width: number; height: number } | null;
  id: number;
  room_id: string;
  user_id: string;
  display_name: string;
  text: string;
  sent_at_ms: number;
}

export interface ActiveRoom {
  has_more: boolean;
  room: RoomSummary;
  participants: Participant[];
  messages: ChatMessage[];
}

export interface AccountProfile { id: string; username: string; display_name: string }

export interface Snapshot {
  account: AccountProfile | null;
  chat_persistent: boolean;
  rich_chat: boolean;
  self_author_id: string;
  image_uploads: boolean;
  attachment_uploads: boolean;
  microphone: MicrophoneSettings;
  audio_devices: AudioDeviceSettings;
  noise_suppression_available: boolean;
  status: LinkStatus;
  connection_message: string | null;
  connection_attempt: number;
  self_id: string | null;
  display_name: string | null;
  server_name: string | null;
  server_version: string | null;
  protocol_version: number | null;
  rooms: RoomSummary[];
  active_room: ActiveRoom | null;
  call: CallInfo | null;
  error: UiError | null;
}

export const emptySnapshot: Snapshot = {
  account: null,
  chat_persistent: false,
  rich_chat: false,
  self_author_id: '',
  image_uploads: false,
  attachment_uploads: false,
  microphone: { volume_percent: 100, noise_suppression: false },
  audio_devices: { input_device: null, output_device: null },
  noise_suppression_available: false,
  status: 'disconnected',
  connection_message: null,
  connection_attempt: 0,
  self_id: null,
  display_name: null,
  server_name: null,
  server_version: null,
  protocol_version: null,
  rooms: [],
  active_room: null,
  call: null,
  error: null,
};

export interface MicrophoneSettings {
  volume_percent: number;
  noise_suppression: boolean;
}

export interface AudioDeviceSettings { input_device: string | null; output_device: string | null }
export interface AudioDeviceInfo {
  id: string; name: string; is_default_input: boolean; is_default_output: boolean;
  input_channels: number | null; output_channels: number | null;
  input_sample_rate: number | null; output_sample_rate: number | null;
}
export interface AudioTestEvent {
  id: string; phase: 'opening' | 'recording' | 'playing' | 'stopped' | 'error';
  remaining_ms: number; level: number; clipped: boolean; error: string | null;
}
export interface AudioActivityEvent { call_id: string; users: { user_id: string; speaking: boolean; level: number }[] }

export interface ChatPage { messages: ChatMessage[]; has_more: boolean }
