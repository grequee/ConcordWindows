<script lang="ts">
  import { onMount } from 'svelte';
  import { describeError, getChatImage } from './api';
  let { id, width, height }: { id: number; width: number; height: number } = $props();
  let element = $state<HTMLDivElement>();
  let src = $state<string | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(false);
  let dialog = $state<HTMLDialogElement>();
  let zoomSrc = $state<string | null>(null);
  function zoom() { zoomSrc = src; dialog?.showModal(); }
  let visible = false;
  let generation = 0;

  async function load() {
    const current = ++generation;
    loading = true;
    error = null;
    try {
      const image = await getChatImage(id);
      if (current === generation && visible) src = image;
    } catch (cause) {
      if (current === generation) error = describeError(cause);
    } finally {
      if (current === generation) loading = false;
    }
  }
  onMount(() => {
    const observer = new IntersectionObserver(([entry]) => {
      visible = entry.isIntersecting;
      if (visible && !src && !loading) void load();
      if (!visible) { generation++; src = null; loading = false; }
    }, { rootMargin: '300px' });
    if (element) observer.observe(element);
    return () => { observer.disconnect(); generation++; };
  });
</script>

<div class="chat-image" bind:this={element} style:aspect-ratio={`${width} / ${height}`}>
  {#if src}
    <button class="zoom-button" onclick={zoom} aria-label="Ampliar imagem"><img {src} alt="Imagem enviada na conversa" {width} {height} decoding="async"
      onerror={() => { src = null; error = 'Não foi possível exibir esta imagem.'; }} /></button>
  {:else if error}
    <div class="image-state" role="status"><span>{error}</span><button onclick={load}>Tentar novamente</button></div>
  {:else}
    <div class="image-state" role="status">{loading ? 'Carregando imagem…' : 'Imagem da conversa'}</div>
  {/if}
</div>

<dialog bind:this={dialog} onclose={() => zoomSrc = null} aria-label="Imagem ampliada">
  <button class="close-zoom" onclick={() => dialog?.close()}>Fechar imagem · Esc</button>
  {#if zoomSrc}<img class="zoom-image" src={zoomSrc} alt="Imagem da conversa ampliada" />{/if}
</dialog>
<style>
  dialog { max-width:95vw; max-height:95vh; padding:16px; border:1px solid var(--line-strong); border-radius:12px; background:var(--surface-raised); }
  dialog::backdrop { background:rgba(0,0,0,.85); }
  .zoom-button { display:block; width:100%; height:100%; cursor:zoom-in; }
  .zoom-image { max-width:88vw; width:auto; height:auto; max-height:80vh; }
  .close-zoom { display:block; margin:0 0 12px auto; font-size:12px; }
  .chat-image { width: min(360px, 100%); max-height: 360px; min-height: 100px; overflow: hidden; margin-top: 8px; border: 1px solid var(--line-strong); border-radius: 10px; background: var(--surface); }
  img { display: block; width: 100%; height: 100%; max-height: 360px; object-fit: contain; }
  .image-state { display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 10px; height: 100%; min-height: 100px; padding: 16px; color: var(--muted); font-size: 12px; text-align: center; }
  button { color: var(--lime); text-decoration: underline; }
</style>
