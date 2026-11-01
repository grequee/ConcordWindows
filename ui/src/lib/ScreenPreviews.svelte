<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { listen } from '@tauri-apps/api/event';
  import Icon from './Icon.svelte';
  import type { CallInfo } from './types';

  interface Props {
    call: CallInfo;
  }

  interface ScreenPreviewEvent {
    source_id: string;
    display_name: string;
    width: number;
    height: number;
    png_base64: string;
  }

  interface ScreenPreview {
    displayName: string;
    width: number;
    height: number;
    src: string;
  }

  let { call }: Props = $props();
  let previews = $state<Record<string, ScreenPreview>>({});
  let collapsed = $state(false);
  let fullscreenSourceId = $state<string | null>(null);
  let fullscreenStage = $state<HTMLDivElement | undefined>(undefined);
  let fullscreenWasActive = false;

  const shares = $derived([
    ...(call.screen_sharing ? [{ sourceId: 'self', displayName: 'Sua tela' }] : []),
    ...call.peers
      .filter((peer) => peer.screen_sharing)
      .map((peer) => ({ sourceId: peer.user_id, displayName: peer.display_name })),
  ]);
  const fullscreenShare = $derived(
    shares.find((share) => share.sourceId === fullscreenSourceId) ?? null,
  );
  const fullscreenPreview = $derived(
    fullscreenSourceId ? previews[fullscreenSourceId] ?? null : null,
  );

  let previousSources = new Set<string>();

  $effect(() => {
    const activeSources = new Set(shares.map((share) => share.sourceId));
    const endedSources = [...previousSources].filter((sourceId) => !activeSources.has(sourceId));
    previousSources = activeSources;
    if (fullscreenSourceId && !activeSources.has(fullscreenSourceId)) closeFullscreen();
    if (endedSources.length > 0) {
      const next = { ...previews };
      for (const sourceId of endedSources) delete next[sourceId];
      previews = next;
    }
  });

  async function watchFullscreen(sourceId: string) {
    fullscreenSourceId = sourceId;
    await tick();
    try {
      if (fullscreenStage?.requestFullscreen) {
        await fullscreenStage.requestFullscreen();
        fullscreenWasActive = true;
      }
    } catch {
      // The fixed overlay still fills the Concord window when WebView
      // fullscreen mode is unavailable.
      fullscreenWasActive = false;
    }
  }

  function closeFullscreen() {
    const shouldExit = document.fullscreenElement === fullscreenStage;
    fullscreenWasActive = false;
    fullscreenSourceId = null;
    if (shouldExit) void document.exitFullscreen().catch(() => {});
  }

  function handleFullscreenChange() {
    if (fullscreenWasActive && document.fullscreenElement !== fullscreenStage) {
      fullscreenWasActive = false;
      fullscreenSourceId = null;
    }
  }

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape' && fullscreenSourceId) closeFullscreen();
  }

  onMount(() => {
    let alive = true;
    let unlisten: (() => void) | undefined;
    document.addEventListener('fullscreenchange', handleFullscreenChange);
    document.addEventListener('keydown', handleKeydown);

    void listen<ScreenPreviewEvent>('concord:screen-preview', (event) => {
      const preview = event.payload;
      if (!alive) return;
      previews = {
        ...previews,
        [preview.source_id]: {
          displayName: preview.display_name,
          width: preview.width,
          height: preview.height,
          src: `data:image/png;base64,${preview.png_base64}`,
        },
      };
    }).then((stop) => {
      if (alive) unlisten = stop;
      else stop();
    });

    return () => {
      alive = false;
      unlisten?.();
      document.removeEventListener('fullscreenchange', handleFullscreenChange);
      document.removeEventListener('keydown', handleKeydown);
    };
  });
</script>

{#if shares.length > 0}
  <section class="screen-previews" aria-label="Telas compartilhadas">
    <header class="preview-bar">
      <div class="preview-heading">
        <span class="screen-icon"><Icon name="screen" size={14} /></span>
        <div class="preview-heading-copy">
          <strong>Telas compartilhadas</strong>
          <span>{shares.length} {shares.length === 1 ? 'transmissão ativa' : 'transmissões ativas'}</span>
        </div>
      </div>
      {#if collapsed}<span class="collapsed-note">A transmissão continua</span>{/if}
      <button
        class="collapse-button"
        onclick={() => (collapsed = !collapsed)}
        aria-expanded={!collapsed}
        aria-controls="screen-preview-grid"
        title={collapsed ? 'Mostrar telas compartilhadas' : 'Minimizar telas compartilhadas'}
      >
        <span>{collapsed ? 'Mostrar telas' : 'Minimizar'}</span>
        <Icon name="chevron-down" size={14} class={collapsed ? 'turned' : ''} />
      </button>
    </header>

    {#if !collapsed}
      <div id="screen-preview-grid" class="preview-grid" class:single={shares.length === 1}>
        {#each shares as share (share.sourceId)}
          {@const preview = previews[share.sourceId]}
          <article class="preview-card">
            <div class="source-label">
              <span class="source-live" aria-hidden="true"></span>
              <span>{share.displayName}</span>
              {#if share.sourceId === 'self'}<span class="you-label">você</span>{/if}
              <button
                class="fullscreen-button"
                onclick={() => watchFullscreen(share.sourceId)}
                title="Assistir em tela cheia"
                aria-label="Assistir a tela de {share.displayName} em tela cheia"
              >
                <Icon name="maximize" size={12} />
                <span>Tela cheia</span>
              </button>
            </div>
            <div class="preview-frame">
              {#if preview}
                <img
                  src={preview.src}
                  alt="Prévia da tela compartilhada por {share.displayName}"
                  width={preview.width}
                  height={preview.height}
                />
              {:else}
                <div class="waiting-preview">
                  <span class="waiting-pulse"></span>
                  <span>Recebendo tela…</span>
                </div>
              {/if}
            </div>
          </article>
        {/each}
      </div>
    {/if}
  </section>
{/if}

{#if fullscreenSourceId && fullscreenShare}
  <div
    class="fullscreen-stage"
    bind:this={fullscreenStage}
    role="dialog"
    aria-modal="true"
    aria-label="Tela compartilhada por {fullscreenShare.displayName}"
  >
    <header class="fullscreen-toolbar">
      <div>
        <span class="source-live" aria-hidden="true"></span>
        <strong>{fullscreenShare.displayName}</strong>
        {#if fullscreenShare.sourceId === 'self'}<span class="you-label">você</span>{/if}
      </div>
      <button class="fullscreen-close" onclick={closeFullscreen}>
        <Icon name="close" size={14} />
        <span>Fechar</span>
        <kbd>Esc</kbd>
      </button>
    </header>
    <div class="fullscreen-media">
      {#if fullscreenPreview}
        <img
          src={fullscreenPreview.src}
          alt="Tela compartilhada por {fullscreenShare.displayName} em tela cheia"
        />
      {:else}
        <div class="waiting-preview">
          <span class="waiting-pulse"></span>
          <span>Recebendo tela…</span>
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .screen-previews {
    flex: none;
    padding: 0 clamp(22px, 3.2vw, 42px) 10px;
    border-bottom: 1px solid var(--line);
    background: rgba(8, 10, 9, 0.15);
  }

  .preview-bar {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 42px;
  }

  .preview-heading {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  .screen-icon {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    flex: none;
    border: 1px solid rgba(217, 242, 107, 0.18);
    border-radius: 7px;
    background: rgba(217, 242, 107, 0.07);
    color: var(--lime);
  }

  .preview-heading-copy {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }

  .preview-heading-copy strong {
    color: var(--ink-soft);
    font-size: 10px;
    font-weight: 650;
  }

  .preview-heading-copy > span,
  .collapsed-note {
    color: var(--faint);
    font-size: 9px;
  }

  .collapsed-note {
    overflow: hidden;
    margin-left: auto;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .collapse-button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 5px;
    min-height: 28px;
    margin-left: auto;
    padding: 0 8px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: rgba(241, 232, 215, 0.025);
    color: var(--muted);
    font: inherit;
    font-size: 9px;
    cursor: pointer;
    transition: border-color 140ms ease, color 140ms ease, background 140ms ease;
  }

  .collapse-button:hover {
    border-color: rgba(217, 242, 107, 0.24);
    background: rgba(217, 242, 107, 0.06);
    color: var(--ink-soft);
  }

  .collapse-button :global(.turned) {
    transform: rotate(180deg);
  }

  .collapse-button :global(svg) {
    transition: transform 160ms ease;
  }

  .preview-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(280px, 100%), 1fr));
    gap: 9px;
    max-height: min(60vh, 620px);
    overflow: auto;
    padding: 0 1px 2px;
  }

  .preview-grid.single {
    grid-template-columns: minmax(0, 1fr);
  }

  .preview-card {
    min-width: 0;
    overflow: hidden;
    border: 1px solid rgba(241, 232, 215, 0.1);
    border-radius: 8px;
    background: rgba(0, 0, 0, 0.22);
  }

  .source-label {
    display: flex;
    align-items: center;
    gap: 7px;
    min-height: 30px;
    padding: 0 9px;
    color: var(--ink-soft);
    font-size: 9px;
    font-weight: 600;
  }

  .fullscreen-button {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    min-height: 23px;
    margin-left: auto;
    padding: 0 7px;
    border: 1px solid rgba(241, 232, 215, 0.1);
    border-radius: 5px;
    background: rgba(241, 232, 215, 0.035);
    color: var(--muted);
    font: inherit;
    font-size: 8px;
    cursor: pointer;
    transition: border-color 140ms ease, color 140ms ease, background 140ms ease;
  }

  .fullscreen-button:hover {
    border-color: rgba(217, 242, 107, 0.24);
    background: rgba(217, 242, 107, 0.07);
    color: var(--lime);
  }

  .source-live,
  .waiting-pulse {
    width: 6px;
    height: 6px;
    flex: none;
    border-radius: 50%;
    background: var(--lime);
    box-shadow: 0 0 7px rgba(217, 242, 107, 0.55);
  }

  .you-label {
    padding: 2px 5px;
    border-radius: 3px;
    background: rgba(217, 242, 107, 0.08);
    color: var(--lime);
    font-size: 8px;
    font-weight: 500;
  }

  .preview-frame {
    display: grid;
    place-items: center;
    width: 100%;
    min-height: 120px;
    overflow: hidden;
    background:
      linear-gradient(135deg, rgba(255, 255, 255, 0.025), transparent 45%),
      #090b0a;
  }

  .preview-frame img {
    display: block;
    width: auto;
    height: auto;
    max-width: 100%;
    max-height: min(54vh, 560px);
    object-fit: contain;
  }

  .waiting-preview {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    color: var(--faint);
    font-size: 10px;
  }

  .waiting-pulse {
    animation: pulse 1.5s ease-in-out infinite;
  }

  .fullscreen-stage {
    position: fixed;
    z-index: 10000;
    inset: 0;
    display: grid;
    grid-template-rows: 48px minmax(0, 1fr);
    width: 100vw;
    height: 100vh;
    border: 0;
    border-radius: 0;
    background: #050706;
    color: var(--ink-soft);
  }

  .fullscreen-stage:fullscreen {
    width: 100vw;
    height: 100vh;
  }

  .fullscreen-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    min-width: 0;
    padding: 0 18px;
    border-bottom: 1px solid rgba(241, 232, 215, 0.1);
    background: #0b0e0c;
  }

  .fullscreen-toolbar > div,
  .fullscreen-close {
    display: inline-flex;
    align-items: center;
    gap: 9px;
  }

  .fullscreen-toolbar strong {
    overflow: hidden;
    font-size: 12px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .fullscreen-close {
    min-height: 30px;
    padding: 0 9px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: rgba(241, 232, 215, 0.035);
    color: var(--muted);
    font: inherit;
    font-size: 10px;
    cursor: pointer;
  }

  .fullscreen-close:hover {
    color: var(--ink-soft);
    border-color: rgba(217, 242, 107, 0.24);
  }

  .fullscreen-close kbd {
    padding: 2px 4px;
    border: 1px solid rgba(241, 232, 215, 0.12);
    border-radius: 3px;
    color: var(--faint);
    font: inherit;
    font-size: 8px;
  }

  .fullscreen-media {
    display: grid;
    place-items: center;
    min-width: 0;
    min-height: 0;
    padding: 12px;
    overflow: hidden;
  }

  .fullscreen-media img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: contain;
  }

  @keyframes pulse {
    50% { opacity: 0.4; }
  }

  @media (max-width: 680px) {
    .screen-previews { padding-right: 18px; padding-left: 18px; }
    .preview-grid { grid-template-columns: minmax(0, 1fr); }
    .collapsed-note { display: none; }
  }
</style>
