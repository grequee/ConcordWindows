<script lang="ts">
  import Icon from './Icon.svelte';
  let { id, name, bytes }: { id: number; name: string; bytes: number } = $props();
  import { startDownload, downloads } from './downloads.svelte';
  const downloading = $derived(downloads.items.some(item => item.state === 'downloading' && item.message_id === id));
  const size = $derived(bytes < 1024 ? `${bytes} B` : bytes < 1024 * 1024 ? `${(bytes / 1024).toFixed(1)} KiB` : `${(bytes / 1024 / 1024).toFixed(1)} MiB`);
  function download() { void startDownload(id, name, bytes); }
</script>

<div class="attachment">
  <Icon name="file" size={24} />
  <div class="file-copy"><strong>{name}</strong><span>{size}</span></div>
  <button onclick={download} disabled={downloading} title="Salvar na pasta Downloads" aria-label={`Baixar ${name}`}>
    {#if downloading}<span>Baixando…</span>{:else}<Icon name="download" size={18} />{/if}
  </button>
</div>


<style>
  .attachment { display: flex; align-items: center; gap: 12px; max-width: 360px; margin-top: 8px; padding: 12px; border: 1px solid var(--line-strong); border-radius: 10px; background: var(--surface); color: var(--muted); }
  .file-copy { display: flex; flex: 1; flex-direction: column; gap: 4px; min-width: 0; }
  strong { color: var(--ink-soft); font-size: 12px; font-weight: 600; overflow-wrap: anywhere; }
  span { font-size: 11px; }
  button { flex: none; display: flex; align-items: center; justify-content: center; min-width: 32px; min-height: 32px; border-radius: 7px; color: var(--lime); }
  button:hover { background: var(--lime-soft); }
  button:disabled { opacity: 0.6; }

</style>
