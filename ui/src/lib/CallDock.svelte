<script lang="ts">
  import { describeError } from './api';
  import * as api from './api';
  import Avatar from './Avatar.svelte';
  import Icon from './Icon.svelte';
  import ScreenPreviews from './ScreenPreviews.svelte';
  import type {
    ActiveRoom,
    CallInfo,
    CallPeerState,
    CallState,
    ScreenTier,
  } from './types';

  interface Props {
    call: CallInfo | null;
    room: ActiveRoom;
    selfId: string;
    onjoin: () => Promise<void>;
    onleave: () => Promise<void>;
  }

  let { call, room, selfId, onjoin, onleave }: Props = $props();

  let joining = $state(false);
  let leaving = $state(false);
  let presencePending = $state<'muted' | 'deafened' | null>(null);
  let screenPending = $state(false);
  let tierPending = $state(false);
  let dockError = $state<string | null>(null);
  let dismissedScreenError = $state<string | null>(null);

  const qualityOptions: ReadonlyArray<{
    value: ScreenTier;
    label: string;
    hint: string;
  }> = [
    { value: '720p30', label: '720p30', hint: 'equilíbrio e menor uso de rede' },
    { value: '720p60', label: '720p60', hint: 'mais fluidez para movimento' },
    { value: '1080p30', label: '1080p30', hint: 'mais detalhe para texto' },
    {
      value: '1080p60',
      label: '1080p60 (exp.)',
      hint: 'experimental: mais detalhe e fluidez, maior uso de CPU e rede',
    },
  ];

  const self = $derived(
    room.participants.find((participant) => participant.user_id === selfId) ?? null,
  );
  const muted = $derived(call?.muted ?? self?.muted ?? false);
  const deafened = $derived(call?.deafened ?? self?.deafened ?? false);
  const screenSharing = $derived(call?.screen_sharing ?? false);
  const screenTier = $derived(call?.screen_tier ?? '720p30');
  const screenTierHint = $derived(
    qualityOptions.find((option) => option.value === screenTier)?.hint ?? 'perfil experimental',
  );
  const remoteScreens = $derived(
    call?.peers.reduce((count, peer) => count + (peer.screen_sharing ? 1 : 0), 0) ?? 0,
  );
  const screenError = $derived(
    call?.screen_error && call.screen_error !== dismissedScreenError
      ? call.screen_error
      : null,
  );
  const busy = $derived(joining || leaving || presencePending !== null || tierPending);
  const totalLost = $derived(call?.peers.reduce((sum, peer) => sum + peer.packets_lost, 0) ?? 0);
  const totalConcealed = $derived(
    call?.peers.reduce((sum, peer) => sum + peer.frames_concealed, 0) ?? 0,
  );
  const totalVideoLost = $derived(
    call?.peers.reduce((sum, peer) => sum + peer.video_lost + peer.video_decode_errors, 0) ?? 0,
  );

  $effect(() => {
    if (!call?.screen_error) dismissedScreenError = null;
  });

  function callStateLabel(state: CallState): string {
    if (state === 'joining') return 'entrando';
    if (state === 'reconnecting') return 'reconectando';
    return 'voz ativa';
  }

  function peerStateLabel(state: CallPeerState): string {
    if (state === 'connected') return 'conectado';
    if (state === 'connecting') return 'conectando';
    if (state === 'reconnecting') return 'reconectando';
    return 'falhou';
  }

  async function join() {
    if (busy) return;
    dockError = null;
    joining = true;
    try {
      await onjoin();
    } catch (error) {
      dockError = describeError(error);
    } finally {
      joining = false;
    }
  }

  async function leave() {
    if (busy) return;
    dockError = null;
    leaving = true;
    try {
      await onleave();
    } catch (error) {
      dockError = describeError(error);
    } finally {
      leaving = false;
    }
  }

  async function toggle(field: 'muted' | 'deafened' | 'screen') {
    if (busy) return;
    dockError = null;
    if (field === 'screen') {
      if (screenPending) return;
      screenPending = true;
      try {
        await api.setScreenSharing(!screenSharing);
      } catch (error) {
        if (call && !leaving) dockError = describeError(error);
      } finally {
        screenPending = false;
      }
      return;
    }
    presencePending = field;
    try {
      await api.setPresence(
        field === 'muted' ? { muted: !muted } : { deafened: !deafened },
      );
    } catch (error) {
      dockError = describeError(error);
    } finally {
      presencePending = null;
    }
  }

  async function changeTier(event: Event) {
    const select = event.currentTarget as HTMLSelectElement;
    const tier = select.value as ScreenTier;
    if (tier === screenTier) return;
    dockError = null;
    tierPending = true;
    try {
      await api.setScreenTier(tier);
    } catch (error) {
      select.value = screenTier;
      dockError = describeError(error);
    } finally {
      tierPending = false;
    }
  }
</script>

<section class="call-dock" class:live={call !== null} aria-label="Chamada de voz">
  <div class="call-mark" aria-hidden="true">
    <Icon name={call ? 'phone' : 'mic'} size={16} />
    {#if call}<span class="call-pulse"></span>{/if}
  </div>

  <div class="call-copy">
    <div class="call-title-row">
      <span class="call-title">{call ? 'Chamada da sala' : 'Voz da sala'}</span>
      {#if call}
        <span class="call-state" class:warn={call.state === 'reconnecting'}>
          {callStateLabel(call.state)}
        </span>
      {/if}
    </div>
    <span class="call-sub">
      {#if call}
        {#if call.peers.length === 0}
          só você na chamada
        {:else}
          {call.peers.length} {call.peers.length === 1 ? 'pessoa' : 'pessoas'} na chamada
        {/if}

        {#if screenPending && !screenSharing}
          <span class="sub-sep">·</span>
          selecione a tela na janela do sistema
        {:else if screenSharing}
          <span class="sub-sep">·</span>
          compartilhando sua tela
        {:else if remoteScreens > 0}
          <span class="sub-sep">·</span>
          {remoteScreens} {remoteScreens === 1 ? 'tela remota' : 'telas remotas'}
        {/if}
      {:else}
        Converse com o grupo e compartilhe sua tela
      {/if}
    </span>
  </div>

  {#if call}
    <div class="call-peers" aria-label="Participantes na chamada">
      {#each call.peers.slice(0, 4) as peer (peer.user_id)}
        <span class="call-peer" class:failed={peer.state === 'failed'} title={`${peer.display_name} — ${peerStateLabel(peer.state)}${peer.screen_sharing ? ' · compartilhando a tela' : ''}`}>
          <Avatar name={peer.display_name} id={peer.user_id} size={23} />
          <span class="peer-state" class:online={peer.state === 'connected'}></span>
          {#if peer.screen_sharing}
            <span class="peer-screen" aria-hidden="true"><Icon name="screen" size={8} /></span>
          {/if}
        </span>
      {/each}
      {#if call.peers.length > 4}
        <span class="peer-more tabular">+{call.peers.length - 4}</span>
      {/if}
    </div>

    <div class="call-actions">
      <button
        class="call-button"
        class:active={muted}
        onclick={() => toggle('muted')}
        disabled={busy}
        aria-pressed={muted}
        title={muted ? 'Ativar microfone' : 'Silenciar microfone'}
      >
        <Icon name={muted ? 'mic-off' : 'mic'} size={15} />
        <span>{muted ? 'mudo' : 'mic'}</span>
      </button>
      <button
        class="call-button"
        class:active={deafened}
        onclick={() => toggle('deafened')}
        disabled={busy}
        aria-pressed={deafened}
        title={deafened ? 'Reativar áudio' : 'Desativar áudio'}
      >
        <Icon name={deafened ? 'headphones-off' : 'headphones'} size={15} />
        <span>{deafened ? 'sem áudio' : 'áudio'}</span>
      </button>
      <label
        class="quality-picker"
        title={`Qualidade da tela: ${screenTierHint}`}
      >
        <span class="quality-picker-label">qualidade</span>
        <select
          class="quality-select"
          value={screenTier}
          onchange={changeTier}
          disabled={busy || screenPending}
          aria-label="Qualidade da tela"
        >
          {#each qualityOptions as option (option.value)}
            <option value={option.value}>{option.label}</option>
          {/each}
        </select>
      </label>
      <button
        class="call-button"
        class:active={screenSharing}
        onclick={() => toggle('screen')}
        disabled={busy || screenPending}
        aria-busy={screenPending}
        aria-pressed={screenSharing}
        title={screenSharing ? 'Parar de compartilhar a tela' : 'Compartilhar a tela'}
      >
        <Icon name={screenSharing ? 'screen-off' : 'screen'} size={15} />
        <span>{screenPending ? 'aguarde…' : screenSharing ? 'parar' : 'tela'}</span>
      </button>
      <button class="call-button leave" onclick={leave} disabled={busy}>
        <Icon name="phone-off" size={15} />
        <span>{leaving ? 'saindo…' : 'sair'}</span>
      </button>
    </div>
  {:else}
    <div class="call-signal" aria-hidden="true">
      <i></i><i></i><i></i><i></i>
    </div>
    <button class="join-call" onclick={join} disabled={busy || !self}>
      {#if joining}
        <span class="join-spinner" aria-hidden="true"></span>
        abrindo áudio…
      {:else}
        <Icon name="phone" size={14} />
        Entrar na voz
      {/if}
    </button>
  {/if}
</section>

{#if call}<ScreenPreviews {call} />{/if}

{#if call}<details class="diagnostics"><summary>Diagnóstico da chamada</summary><p>Áudio Opus · 48 kHz mono · 32 kbps · Tela VP8 {screenTier}</p>    <div class="call-quality" class:warn={totalLost > 0 || totalConcealed > 0 || totalVideoLost > 0}>
      <span class="quality-dot" aria-hidden="true"></span>
      <span class="tabular">perda {totalLost}</span>
      <span class="quality-sep">/</span>
      <span class="tabular">PLC {totalConcealed}</span>
      {#if totalVideoLost > 0}
        <span class="quality-sep">/</span>
        <span class="tabular">vídeo {totalVideoLost}</span>
      {/if}
    </div>

<p>Quadros enviados: {call.video.tx_frames} · Participantes: {call.peers.length}</p></details>{/if}

{#if dockError}
  <div class="call-error" role="status">
    <Icon name="warning" size={13} />
    <span>{dockError}</span>
    <button onclick={() => (dockError = null)} aria-label="Fechar erro">
      <Icon name="close" size={12} />
    </button>
  </div>
{:else if screenError}
  <div class="call-error" role="status">
    <Icon name="warning" size={13} />
    <span>{screenError}</span>
    <button
      onclick={() => (dismissedScreenError = screenError)}
      aria-label="Fechar erro de tela"
    >
      <Icon name="close" size={12} />
    </button>
  </div>
{/if}

<style>
  .diagnostics { margin:0 24px 8px; color:var(--muted); font-size:11px; } .diagnostics summary { cursor:pointer; padding:5px 0; } .diagnostics p { margin:8px 0; }
  .call-dock {
    position: relative;
    display: flex;
    align-items: center;
    gap: 11px;
    flex: none;
    min-height: 50px;
    padding: 7px clamp(22px, 3.2vw, 42px);
    border-bottom: 1px solid var(--line);
    background:
      linear-gradient(90deg, rgba(241, 232, 215, 0.022), transparent 58%),
      rgba(8, 10, 9, 0.2);
  }

  .call-dock.live {
    background:
      linear-gradient(90deg, rgba(217, 242, 107, 0.055), transparent 62%),
      rgba(8, 10, 9, 0.26);
    border-bottom-color: rgba(217, 242, 107, 0.12);
  }

  .call-mark {
    position: relative;
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    flex: none;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: rgba(241, 232, 215, 0.035);
    color: var(--muted);
  }

  .live .call-mark {
    color: var(--lime);
    border-color: rgba(217, 242, 107, 0.18);
    background: var(--lime-soft);
  }

  .call-pulse {
    position: absolute;
    top: -2px;
    right: -2px;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    border: 1.5px solid var(--bg);
    background: var(--lime);
    box-shadow: 0 0 7px rgba(217, 242, 107, 0.65);
  }

  .call-copy {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }

  .call-title-row {
    display: flex;
    align-items: center;
    gap: 7px;
  }

  .call-title {
    color: var(--ink-soft);
    font-size: 10.5px;
    font-weight: 650;
    letter-spacing: 0.015em;
  }

  .call-state {
    padding: 0 5px;
    border-radius: 3px;
    background: rgba(217, 242, 107, 0.1);
    color: color-mix(in srgb, var(--lime) 72%, var(--faint));
    font-size: 7.5px;
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }

  .call-state.warn {
    background: color-mix(in srgb, var(--amber) 10%, transparent);
    color: var(--amber);
  }

  .call-sub {
    overflow: hidden;
    color: var(--faint);
    font-size: 9px;
    line-height: 1.35;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .sub-sep {
    margin: 0 3px;
    opacity: 0.55;
  }

  .call-peers {
    display: flex;
    align-items: center;
    gap: 4px;
    flex: none;
    margin-left: auto;
  }

  .call-peer {
    position: relative;
    display: block;
    border-radius: 8px;
  }

  .call-peer.failed {
    opacity: 0.5;
  }

  .peer-state {
    position: absolute;
    right: -1px;
    bottom: -1px;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    border: 2px solid var(--bg);
    background: var(--amber);
  }

  .peer-state.online {
    background: var(--lime);
  }

  .peer-screen {
    position: absolute;
    top: -3px;
    left: -3px;
    display: grid;
    place-items: center;
    width: 11px;
    height: 11px;
    border: 1.5px solid var(--bg);
    border-radius: 4px;
    background: var(--lime-soft);
    color: var(--lime);
  }

  .peer-more {
    display: grid;
    place-items: center;
    width: 23px;
    height: 23px;
    margin-left: 1px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--surface-raised);
    color: var(--faint);
    font-size: 8px;
  }

  .call-quality {
    display: flex;
    align-items: center;
    gap: 4px;
    flex: none;
    padding: 0 8px;
    border-left: 1px solid var(--line);
    color: var(--faint);
    font-size: 8.5px;
    white-space: nowrap;
  }

  .call-quality.warn {
    color: var(--amber);
  }

  .quality-dot {
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: currentColor;
  }

  .quality-sep {
    opacity: 0.4;
  }

  .quality-picker {
    display: flex;
    align-items: center;
    gap: 5px;
    flex: none;
    padding-left: 8px;
    border-left: 1px solid var(--line);
    color: var(--faint);
    font-size: 8px;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }

  .quality-select {
    height: 29px;
    padding: 0 6px;
    border: 1px solid var(--line);
    border-radius: 7px;
    background: var(--surface-raised);
    color: var(--ink-soft);
    font: inherit;
    font-size: 9px;
    font-weight: 650;
    letter-spacing: 0.01em;
    text-transform: none;
    cursor: pointer;
  }

  .quality-select:hover:not(:disabled) {
    border-color: var(--line-strong);
  }

  .quality-select:focus-visible {
    outline: 1px solid var(--lime);
    outline-offset: 1px;
  }

  .quality-select:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .call-actions {
    display: flex;
    align-items: center;
    gap: 4px;
    flex: none;
  }

  .call-button,
  .join-call {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 29px;
    padding: 0 9px;
    border: 1px solid var(--line);
    border-radius: 7px;
    color: var(--faint);
    font-size: 9px;
    font-weight: 650;
    letter-spacing: 0.02em;
    transition:
      color 130ms ease,
      border-color 130ms ease,
      background 130ms ease;
  }

  .call-button:hover:not(:disabled) {
    color: var(--ink-soft);
    border-color: var(--line-strong);
    background: rgba(241, 232, 215, 0.04);
  }

  .call-button.active {
    color: var(--coral);
    border-color: color-mix(in srgb, var(--coral) 22%, transparent);
    background: color-mix(in srgb, var(--coral) 6%, transparent);
  }

  .call-button.leave:hover:not(:disabled) {
    color: var(--coral);
    border-color: color-mix(in srgb, var(--coral) 24%, transparent);
    background: color-mix(in srgb, var(--coral) 7%, transparent);
  }

  .call-button:disabled,
  .join-call:disabled {
    opacity: 0.5;
  }

  .join-call {
    margin-left: auto;
    height: 30px;
    padding: 0 12px;
    border-color: rgba(217, 242, 107, 0.2);
    background: var(--lime-soft);
    color: color-mix(in srgb, var(--lime) 86%, var(--ink));
    font-size: 9.5px;
    flex: none;
  }

  .join-call:hover:not(:disabled) {
    border-color: rgba(217, 242, 107, 0.38);
    background: rgba(217, 242, 107, 0.16);
  }

  .join-spinner {
    width: 10px;
    height: 10px;
    border: 1.5px solid rgba(217, 242, 107, 0.25);
    border-top-color: var(--lime);
    border-radius: 50%;
    animation: spin 700ms linear infinite;
  }

  .call-signal {
    display: flex;
    align-items: flex-end;
    gap: 2px;
    height: 15px;
    margin-left: auto;
    opacity: 0.45;
  }

  .call-signal i {
    width: 2px;
    border-radius: 2px;
    background: var(--lime);
  }

  .call-signal i:nth-child(1) {
    height: 4px;
  }

  .call-signal i:nth-child(2) {
    height: 8px;
  }

  .call-signal i:nth-child(3) {
    height: 12px;
  }

  .call-signal i:nth-child(4) {
    height: 6px;
  }

  .call-error {
    display: flex;
    align-items: flex-start;
    gap: 6px;
    flex: none;
    padding: 7px clamp(22px, 3.2vw, 42px);
    border-bottom: 1px solid color-mix(in srgb, var(--coral) 18%, transparent);
    background: color-mix(in srgb, var(--coral) 6%, transparent);
    color: var(--coral);
    font-size: 10px;
    line-height: 1.4;
  }

  .call-error span {
    flex: 1;
    min-width: 0;
  }

  .call-error button {
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    flex: none;
    border-radius: 4px;
    color: inherit;
  }

  .call-error button:hover {
    background: color-mix(in srgb, var(--coral) 12%, transparent);
  }

  @media (max-width: 1100px) {
    .call-quality {
      display: none;
    }

    .quality-picker {
      padding-left: 4px;
      border-left: 0;
    }

    .quality-picker-label {
      display: none;
    }

    .call-button span {
      display: none;
    }

    .call-button {
      width: 29px;
      padding: 0;
      justify-content: center;
    }
  }
</style>
