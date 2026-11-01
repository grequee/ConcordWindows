import { invoke } from '@tauri-apps/api/core';
export interface Download { id: string; message_id: number; name: string; received: number; total: number; state: 'downloading' | 'completed' | 'failed'; path?: string; error?: string }
export const downloads = $state<{ items: Download[] }>({ items: [] });
export function updateDownload(event: Partial<Download> & { id: string }) {
  const item = downloads.items.find(item => item.id === event.id);
  if (item) Object.assign(item, event);
}
export async function startDownload(messageId: number, name: string, total: number): Promise<void> {
  const id = crypto.randomUUID();
  downloads.items = [...downloads.items.filter(item => item.state === 'downloading'), ...downloads.items.filter(item => item.state !== 'downloading').slice(-4), { id, message_id: messageId, name, total, received: 0, state: 'downloading' }];
  try { const path = await invoke<string>('download_chat_attachment', { id: messageId, transferId: id }); updateDownload({ id, state: 'completed', path, received: total }); }
  catch (cause) { updateDownload({ id, state: 'failed', error: cause instanceof Error ? cause.message : String(cause) }); }
}
export const cancelDownload = (id: string): Promise<void> => invoke('cancel_chat_download', { transferId: id });
