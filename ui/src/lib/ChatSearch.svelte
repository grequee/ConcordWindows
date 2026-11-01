<script lang="ts">
  import { searchChat, describeError } from './api';
  import type { ChatMessage } from './types';
  import Icon from './Icon.svelte';
  let { roomId, onreply, onclose }: { roomId: string; onreply: (message: ChatMessage) => void; onclose: () => void } = $props();
  let query = $state('');
  let messages = $state<ChatMessage[]>([]);
  let busy = $state(false);
  let more = $state(false);
  let error = $state<string | null>(null);
  let searched = $state(false);
  let request = 0;
  async function search(older = false) {
    const term = query.trim();
    if (!term || busy) return;
    const sequence = ++request;
    const room = roomId;
    busy = true; error = null;
    try {
      const page = await searchChat(term, older ? messages.at(-1)?.id : undefined);
      if (room !== roomId || sequence !== request) return;
      messages = older ? [...messages, ...page.messages.reverse()] : page.messages.reverse();
      more = page.has_more; searched = true;
    } catch (cause) { if (room === roomId) error = describeError(cause); }
    finally { busy = false; }
  }
</script>
<section class="chat-search" aria-label="Buscar no histórico">
  <form onsubmit={event => { event.preventDefault(); void search(); }}>
    <Icon name="search" size={15} />
    <input aria-label="Buscar nesta sala" placeholder="Buscar nesta sala…" maxlength="100" bind:value={query} />
    <button disabled={busy || !query.trim()}>{busy ? 'Buscando…' : 'Buscar'}</button>
    <button type="button" onclick={onclose} aria-label="Fechar busca"><Icon name="close" size={16} /></button>
  </form>
  {#if error}<p role="status">{error}</p>{/if}
  {#if searched && !messages.length}<p role="status">Nenhuma mensagem encontrada.</p>{/if}
  {#if messages.length}<div class="search-results">{#each messages as message (message.id)}
    <div class="search-result"><div><strong>{message.display_name}</strong><time>{new Date(message.sent_at_ms).toLocaleString('pt-BR')}</time><p>{message.text || message.attachment?.name || 'Imagem'}</p></div><button title="Responder a esta mensagem" aria-label="Responder a esta mensagem" onclick={() => onreply(message)}><Icon name="reply" size={15} /></button></div>
  {/each}</div>{/if}
  {#if more}<button class="more-results" disabled={busy} onclick={() => search(true)}>Mais resultados</button>{/if}
</section>
<style>
  .chat-search { padding: 12px clamp(22px, 3.2vw, 42px); border-bottom: 1px solid var(--line); background: var(--surface); }
  form { display: flex; align-items: center; gap: 10px; color: var(--muted); }
  input { flex: 1; min-width: 0; padding: 9px; border: 1px solid var(--line-strong); border-radius: 7px; background: var(--bg); color: var(--ink); font-size: 12px; }
  button { padding: 6px; color: var(--lime); font-size: 11px; }
  button:disabled { opacity: .5; }
  p { color: var(--muted); font-size: 11px; }
  .search-results { max-height: 250px; overflow-y: auto; margin-top: 10px; }
  .search-result { display: flex; align-items: center; gap: 10px; padding: 10px 4px; border-top: 1px solid var(--line); }
  .search-result > div { flex: 1; min-width: 0; }
  strong { color: var(--ink-soft); font-size: 11px; }
  time { margin-left: 10px; color: var(--faint); font-size: 9px; }
  .search-result p { margin: 5px 0 0; overflow-wrap: anywhere; white-space: pre-wrap; }
  .more-results { display: block; margin: 8px auto 0; }
</style>
