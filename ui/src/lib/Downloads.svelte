<script lang="ts">
  import { downloads, cancelDownload } from './downloads.svelte';
  let error = $state('');
</script>
{#if downloads.items.length}
  <aside class="downloads" aria-label="Downloads">
    <header>Downloads</header>
    {#each downloads.items as item (item.id)}
      <div class="download"><strong>{item.name}</strong>
        {#if item.state === 'downloading'}
          <progress value={item.received} max={Math.max(1,item.total)} aria-label={`Download de ${item.name}`}></progress>
          <span>{Math.floor(item.received / Math.max(1,item.total) * 100)}%</span>
          <button onclick={() => cancelDownload(item.id).catch(cause => error = String(cause))}>Cancelar</button>
        {:else}
          <p role="status">{item.state === 'completed' ? `Salvo em ${item.path}` : item.error}</p>
          <button aria-label={`Fechar download de ${item.name}`} onclick={() => downloads.items = downloads.items.filter(entry => entry.id !== item.id)}>Fechar</button>
        {/if}
      </div>
    {/each}
    {#if error}<p role="status">{error}</p>{/if}
  </aside>
{/if}
<style>
  .downloads { position:fixed; bottom:18px; right:20px; z-index:90; width:310px; max-height:50vh; overflow:auto; padding:16px; border:1px solid var(--line-strong); border-radius:12px; background:var(--surface-raised); box-shadow:var(--shadow-md); }
  header { font-family:var(--font-display); font-size:20px; margin-bottom:8px; }
  .download { display:flex; gap:8px; align-items:center; flex-wrap:wrap; padding:10px 0; border-top:1px solid var(--line); font-size:11px; }
  strong { width:100%; overflow-wrap:anywhere; color:var(--ink-soft); } progress { flex:1; min-width:0; accent-color:var(--lime); } p { margin:0; color:var(--muted); overflow-wrap:anywhere; width:100%; } button { color:var(--lime); padding:4px; }
</style>
