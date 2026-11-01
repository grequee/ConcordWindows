import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { MicrophoneSettings, ScreenTier, Snapshot, AudioDeviceInfo, AudioDeviceSettings, AudioTestEvent, AudioActivityEvent, ChatMessage, ChatPage } from './types';
import { readFilePart } from './fileParts';

export const STATE_EVENT = 'concord:state';

export function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

export function describeError(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === 'string') return error;
  if (error && typeof error === 'object' && 'message' in error) {
    return String((error as { message: unknown }).message);
  }
  return 'Algo deu errado. Tente novamente.';
}

export function getSnapshot(): Promise<Snapshot> {
  return invoke<Snapshot>('get_snapshot');
}

export function setMicrophoneSettings(settings: MicrophoneSettings): Promise<MicrophoneSettings> {
  return invoke<MicrophoneSettings>('set_microphone_settings', { settings });
}

export function listenToState(handler: (snapshot: Snapshot) => void): Promise<UnlistenFn> {
  return listen<Snapshot>(STATE_EVENT, (event) => handler(event.payload));
}

export function connectToServer(
  serverUrl: string,
  displayName: string,
  inviteToken: string,
): Promise<void> {
  return invoke('connect', { serverUrl, displayName, inviteToken });
}

export function disconnectFromServer(): Promise<void> {
  return invoke('disconnect');
}

export function refreshRooms(): Promise<void> {
  return invoke('refresh_rooms');
}

export function joinRoom(roomId: string): Promise<void> {
  return invoke('join_room', { roomId });
}

export function leaveRoom(): Promise<void> {
  return invoke('leave_room');
}

export function joinCall(): Promise<void> {
  return invoke('join_call');
}

export function leaveCall(): Promise<void> {
  return invoke('leave_call');
}

export interface ImageUpload { data: string }

export interface NativeChatAttachment {
  kind: 'native-path';
  path: string;
  name: string;
  size: number;
}

export async function inspectChatAttachment(path: string): Promise<NativeChatAttachment> {
  const info = await invoke<{ name: string; bytes: number }>('inspect_chat_attachment_file', { path });
  return { kind: 'native-path', path, name: info.name, size: info.bytes };
}

export function sendChat(text: string, image?: ImageUpload | null, clientId?: string, replyTo?: number | null): Promise<void> {
  return invoke('send_chat', { text, image: image ?? null, clientId: clientId ?? null, replyTo: replyTo ?? null });
}

export function getChatImage(id: number): Promise<string> { return invoke('get_chat_image', { id }); }

export async function sendChatFile(text: string, file: File, clientId: string, progress: (percent: number) => void, replyTo?: number | null): Promise<void> {
  const ready = await invoke<{upload_id: string; next_part: number; part_bytes: number; receipt: unknown}>('start_chat_attachment', {
    input: { request_id: clientId, text, name: file.name, bytes: file.size, reply_to: replyTo ?? null },
  });
  if (ready.receipt) { progress(100); return; }
  // This bounds a transfer request, never the size of the file.
  if (ready.part_bytes !== 1024 * 1024) throw new Error('Versão de transferência incompatível. Atualize o app e o servidor.');
  const parts = Math.max(1, Math.ceil(file.size / ready.part_bytes));
  for (let part = ready.next_part; part < parts; part++) {
    const data = await readFilePart(file.slice(part * ready.part_bytes, (part + 1) * ready.part_bytes));
    await invoke('upload_chat_part', { id: ready.upload_id, part, data });
    progress(Math.floor((part + 1) / parts * 100));
  }
  await invoke('finish_chat_attachment', { id: ready.upload_id });
}

export async function sendChatAttachmentPath(text: string, file: NativeChatAttachment, clientId: string, progress: (percent: number) => void, replyTo?: number | null): Promise<void> {
  const unlisten = await listen<{ id: string; percent: number }>('concord:upload-progress', event => {
    if (event.payload.id === clientId) progress(event.payload.percent);
  });
  try {
    await invoke('send_chat_attachment_path', {
      path: file.path,
      requestId: clientId,
      text,
      replyTo: replyTo ?? null,
    });
  } finally {
    unlisten();
  }
}

export function downloadChatAttachment(id: number): Promise<string> { return invoke('download_chat_attachment', { id }); }
export function loadChatHistory(): Promise<void> { return invoke('load_chat_history'); }

export function setPresence(presence: {
  muted?: boolean;
  deafened?: boolean;
}): Promise<void> {
  return invoke('set_presence', {
    muted: presence.muted,
    deafened: presence.deafened,
  });
}

export function setScreenSharing(enabled: boolean): Promise<void> {
  return invoke('set_screen_sharing', { enabled });
}

export function setScreenTier(tier: ScreenTier): Promise<void> {
  return invoke('set_screen_tier', { tier });
}

export function clearError(): Promise<void> {
  return invoke('clear_error');
}

export function listAudioDevices(): Promise<AudioDeviceInfo[]> { return invoke('list_audio_devices'); }
export function setAudioDevices(settings: AudioDeviceSettings): Promise<AudioDeviceSettings> { return invoke('set_audio_devices', { settings }); }
export function startAudioTest(id: string, devices: AudioDeviceSettings, microphone: MicrophoneSettings): Promise<void> { return invoke('start_audio_test', { request: { id, devices, microphone } }); }
export function stopAudioTest(): Promise<void> { return invoke('stop_audio_test'); }
export function listenAudioTest(handler: (event: AudioTestEvent) => void): Promise<UnlistenFn> { return listen<AudioTestEvent>('concord:audio-test', event => handler(event.payload)); }
export function listenAudioActivity(handler: (event: AudioActivityEvent) => void): Promise<UnlistenFn> { return listen<AudioActivityEvent>('concord:audio-activity', event => handler(event.payload)); }
export function setParticipantVolume(userId: string, volume: number): Promise<void> { return invoke('set_participant_volume', { userId, volume }); }

export function editChatMessage(id: number, text: string): Promise<void> { return invoke('edit_chat_message', { id, text }); }
export function deleteChatMessage(id: number): Promise<void> { return invoke('delete_chat_message', { id }); }
export function searchChat(query: string, before?: number): Promise<ChatPage> { return invoke('search_chat', { query, before: before ?? null }); }
export function getChatMessage(id: number): Promise<ChatMessage> { return invoke('get_chat_message', { id }); }
export function openExternal(url: string): Promise<void> { return invoke('open_external', { url }); }

export interface DesktopSettings { notifications: boolean; close_to_tray: boolean; mute_shortcut: string }
export interface DesktopStatus { settings: DesktopSettings; tray_available: boolean; global_shortcut_active: boolean; shortcut_message: string }
export const getDesktopSettings = (): Promise<DesktopStatus> => invoke('get_desktop_settings');
export const setDesktopSettings = (settings: DesktopSettings): Promise<DesktopStatus> => invoke('set_desktop_settings', { settings });
export const listenDesktopSettings = (handler: (status: DesktopStatus) => void): Promise<UnlistenFn> => listen<DesktopStatus>('concord:desktop-status', event => handler(event.payload));
export const toggleMicrophoneShortcut = (): Promise<void> => invoke('toggle_microphone_shortcut');
export const minimizeToTray = (): Promise<void> => invoke('minimize_to_tray');

export interface AccountRequest { server_url: string; action: 'register' | 'login' | 'resume'; username?: string; password?: string; invite_token?: string }
export interface SavedAccountInfo { account: import('./types').AccountProfile; expires_at_ms: number }
export const getSavedAccount = (serverUrl: string): Promise<SavedAccountInfo | null> => invoke('get_saved_account', { serverUrl });
export const authenticateAccount = (request: AccountRequest): Promise<import('./types').AccountProfile | null> => invoke('authenticate_account', { request });
export const logoutAccount = (): Promise<void> => invoke('logout_account');
