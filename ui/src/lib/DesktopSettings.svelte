<script lang="ts">
  import * as api from './api';
  import Icon from './Icon.svelte';
  let { status }: { status: api.DesktopStatus | null } = $props();
  let open = $state(false);
  let busy = $state(false);
  let error = $state('');
  async function save(changes: Partial<api.DesktopSettings>) {
    if (!status || busy) return;
    busy = true; error = '';
    try { await api.setDesktopSettings({ ...status.settings, ...changes }); }
    catch (cause) { error = api.describeError(cause); }
    finally { busy = false; }
  }
  function closeOnEscape(event: KeyboardEvent) {
    if (open && event.key === 'Escape') {
      open = false;
      event.stopPropagation();
    }
  }
</script>
<svelte:window onkeydown={closeOnEscape} />
<div class="preferences">
  <button class="preferences-trigger" onclick={() => open = !open} aria-expanded={open} aria-controls="preferences-panel"><Icon name="settings" size={16} /> Preferências</button>
  {#if open}
    <section id="preferences-panel" aria-label="Preferências do app">
      <div class="preferences-heading"><h2>Do seu jeito</h2><button type="button" class="close-button" onclick={() => open = false} aria-label="Fechar preferências"><Icon name="close" size={17} /></button></div>
      {#if status}
        <label><input type="checkbox" checked={status.settings.notifications} disabled={busy} onchange={event => save({ notifications: event.currentTarget.checked })} /> Notificações em segundo plano</label>
        <label><input type="checkbox" checked={status.settings.close_to_tray} disabled={busy || !status.tray_available} onchange={event => save({ close_to_tray: event.currentTarget.checked })} /> Fechar mantém o app na bandeja</label>
        {#if status.tray_available}<button class="tray" onclick={() => api.minimizeToTray().catch(cause => error = api.describeError(cause))}>Minimizar para a bandeja</button>{:else}<p>A bandeja não está disponível nesta sessão do desktop.</p>{/if}
        <details><summary>Sobre e diagnóstico</summary><p>Concord 0.1.0 · Mídia nativa: Opus e VP8. Os dados de rede ficam em Diagnóstico, na chamada.</p></details>
      {:else}<p>Carregando preferências…</p>{/if}
      {#if error}<p role="status" class="error">{error}</p>{/if}
    </section>
  {/if}
</div>
<style>
  .preferences { position: relative; margin-top: 10px; }
  .preferences-trigger { display:flex; align-items:center; gap:9px; color:var(--muted); padding:10px; width:100%; font-size:12px; }
  section { position:fixed; bottom:55px; left:24px; width:min(390px,calc(100vw - 48px)); max-height:80vh; overflow:auto; z-index:80; padding:22px; border:1px solid var(--line-strong); border-radius:12px; background:var(--surface-raised); box-shadow:var(--shadow-md); }
  .preferences-heading { display:flex; align-items:center; justify-content:space-between; gap:12px; margin-bottom:20px; }
  h2 { font-family:var(--font-display); font-size:25px; margin:0; }
  .close-button { display:grid; place-items:center; flex:none; padding:6px; color:var(--muted); border-radius:6px; }
  .close-button:hover { color:var(--ink); background:var(--surface-hover); }
  label { display:flex; gap:9px; align-items:center; font-size:12px; color:var(--ink-soft); margin:16px 0; }
  p,summary { font-size:11px; line-height:1.6; color:var(--muted); }
  .tray { color:var(--lime); font-size:11px; padding:8px 0; } .error { color:var(--coral); } details { margin-top:15px; }
</style>
