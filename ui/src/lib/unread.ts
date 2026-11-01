import type { ChatMessage } from './types';
export function readKey(server: string, room: string): string { return `concord.read.${encodeURIComponent(server.trim().replace(/\/$/, ''))}.${encodeURIComponent(room)}`; }
export function unreadMessages(messages: ChatMessage[], lastRead: number, self: string, author: string): ChatMessage[] {
  return messages.filter(message => message.id > lastRead && message.user_id !== self && (!message.author_id || message.author_id !== author));
}
export function readPosition(storage: Pick<Storage, 'getItem'>, key: string): number | null {
  try { const raw = storage.getItem(key); if (raw === null) return null; const value = Number(raw); return Number.isSafeInteger(value) && value >= 0 ? value : null; } catch { return null; }
}
