<script lang="ts">
  import * as api from './api';
  import { describeError } from './api';
  import Avatar from './Avatar.svelte';
  import ParticipantVolume from './ParticipantVolume.svelte';
  import Icon from './Icon.svelte';
  import type { ActiveRoom, CallInfo, AudioActivityEvent } from './types';

  interface Props {
    room: ActiveRoom;
    call: CallInfo | null;
    activity: AudioActivityEvent | null;
    selfId: string;
    serverName: string | null;
    serverVersion: string | null;
    protocolVersion: number | null;
    ondisconnect: () => Promise<void>;
  }

  let {
    room, call, activity,
    selfId,
    serverName,
    serverVersion,
    protocolVersion,
    ondisconnect,
  }: Props = $props();

  type PresenceField = 'muted' | 'deafened' | 'screen';

  let pending = $state<PresenceField | null>(null);
  let leaving = $state(false);
  let panelError = $state<string | null>(null);

  const self = $derived(
    room.participants.find((participant) => participant.user_id === selfId) ?? null,
  );
  const muted = $derived(self?.muted ?? false);
  const deafened = $derived(self?.deafened ?? false);
  const screenSharing = $derived(self?.screen_sharing ?? false);
  const busy = $derived(pending !== null || leaving);

  async function togglePresence(field: PresenceField) {
    if (busy || !self) return;
    panelError = null;
    pending = field;
    try {
      if (field === 'muted') {
        await api.setPresence({ muted: !muted });
      } else if (field === 'deafened') {
        await api.setPresence({ deafened: !deafened });
      } else {
        await api.setScreenSharing(!screenSharing);
      }
    } catch (error) {
      panelError = describeError(error);
    } finally {
      pending = null;
    }
  }

  async function disconnect() {
    if (busy) return;
    panelError = null;
    leaving = true;
    try {
      await ondisconnect();
    } catch (error) {
      panelError = describeError(error);
      leaving = false;
    }
  }

  function speaking(userId: string, muted: boolean, inCall: boolean): boolean {
    if (!call || muted || !inCall || (userId === selfId && call.muted)) return false;
    const live = activity?.call_id === call.id ? activity.users.find(user => user.user_id === userId) : undefined;
    return live?.speaking ?? (userId === selfId ? call.audio.speaking : call.peers.find(peer => peer.user_id === userId)?.speaking ?? false);
  }

  function presenceLine(
    muted: boolean,
    deafened: boolean,
    screenSharing: boolean,
    inCall: boolean,
  ): string {
    if (screenSharing) return 'compartilhando a tela';
    if (deafened) return inCall ? 'sem áudio · na voz' : 'sem áudio';
    if (muted) return inCall ? 'microfone fechado · na voz' : 'microfone fechado';
    if (inCall) return 'na chamada de voz';
    return 'presente';
  }
</script>

<aside class="participants" aria-label="Participantes e sessão">
  <div class="panel-accent" aria-hidden="true"></div>

  <header class="panel-head">
    <div>
      <span class="mono-label">Nesta sala</span>
      <h2 class="display">Quem está aqui</h2>
    </div>
    <span class="count-pill tabular">
      {room.participants.length.toString().padStart(2, '0')}
    </span>
  </header>

  <div class="live-strip">
    <span class="live-dot" aria-hidden="true"></span>
    <span>sessão ativa</span>
    <span class="live-line" aria-hidden="true"></span>
    <span class="tabular">{room.room.member_count}/{room.room.capacity}</span>
  </div>

  <div class="participant-list" aria-label="Participantes da sala">
    {#if room.participants.length === 0}
      <p class="empty">Ninguém apareceu ainda. Sua presença chega em instantes.</p>
    {:else}
      {#each room.participants as participant (participant.user_id)}
        {@const peer = call?.peers.find(peer => peer.user_id === participant.user_id)}
        {@const talking = speaking(participant.user_id, participant.muted, participant.in_call)}
        <div class="participant-entry">
        <div
          class="participant"
          class:self={participant.user_id === selfId}
          class:away={participant.deafened}
          class:speaking={talking}
        >
          <span class="avatar-wrap">
            <Avatar name={participant.display_name} id={participant.user_id} size={32} />
            <span class="presence-dot" aria-hidden="true"></span>
          </span>

          <div class="participant-copy">
            <div class="name-row">
              <strong>{participant.display_name}</strong>
              {#if participant.user_id === selfId}
                <span class="you-tag">você</span>
              {/if}
            </div>
            <span class="presence-line">
              {talking ? 'falando agora' : presenceLine(
                participant.muted,
                participant.deafened,
                participant.screen_sharing,
                participant.in_call,
              )}
            </span>
          </div>

          <div class="flags" aria-label="Estado de mídia">
            {#if participant.in_call}
              <span class="flag call" title="Na chamada de voz" aria-label="Na chamada de voz">
                <Icon name="phone" size={12} />
              </span>
            {/if}
            {#if participant.muted}
              <span class="flag" title="Microfone fechado" aria-label="Microfone fechado">
                <Icon name="mic-off" size={12} />
              </span>
            {/if}
            {#if participant.deafened}
              <span class="flag" title="Sem áudio" aria-label="Sem áudio">
                <Icon name="headphones-off" size={12} />
              </span>
            {/if}
            {#if participant.screen_sharing}
              <span class="flag sharing" title="Compartilhando a tela" aria-label="Compartilhando a tela">
                <Icon name="screen" size={12} />
              </span>
            {/if}
            {#if !participant.muted &&
              !participant.deafened &&
              !participant.screen_sharing &&
              !participant.in_call}
              <span class="flag quiet" title="Presente" aria-label="Presente">
                <Icon name="check" size={12} />
              </span>
            {/if}
          </div>
        </div>
        {#if peer && participant.user_id !== selfId}
          <ParticipantVolume userId={participant.user_id} name={participant.display_name} volume={peer.volume_percent} />
        {/if}
        </div>
      {/each}
    {/if}
  </div>

  <section class="presence-panel" aria-labelledby="presence-title">
    <div class="section-label">
      <h3 class="mono-label" id="presence-title">Sua presença</h3>
      {#if pending}
        <span class="syncing">sincronizando</span>
      {/if}
    </div>

    <div class="presence-controls">
      <button
        class="presence-button"
        class:active={muted}
        onclick={() => togglePresence('muted')}
        disabled={busy || !self}
        aria-pressed={muted}
        title={muted ? 'Ativar microfone' : 'Silenciar microfone'}
      >
        <span class="button-icon"><Icon name={muted ? 'mic-off' : 'mic'} size={17} /></span>
        <span>{muted ? 'Mudo' : 'Microfone'}</span>
        <i class="state-light" aria-hidden="true"></i>
      </button>

      <button
        class="presence-button"
        class:active={deafened}
        onclick={() => togglePresence('deafened')}
        disabled={busy || !self}
        aria-pressed={deafened}
        title={deafened ? 'Reativar áudio' : 'Desativar áudio'}
      >
        <span class="button-icon">
          <Icon name={deafened ? 'headphones-off' : 'headphones'} size={17} />
        </span>
        <span>{deafened ? 'Sem áudio' : 'Áudio'}</span>
        <i class="state-light" aria-hidden="true"></i>
      </button>

      <button
        class="presence-button"
        class:active={screenSharing}
        onclick={() => togglePresence('screen')}
        disabled={busy || !self || !self.in_call}
        aria-pressed={screenSharing}
        title={!self?.in_call
          ? 'Entre na chamada para compartilhar a tela'
          : screenSharing
            ? 'Parar de compartilhar a tela'
            : 'Compartilhar a tela'}
      >
        <span class="button-icon">
          <Icon name={screenSharing ? 'screen-off' : 'screen'} size={17} />
        </span>
        <span>{screenSharing ? 'Parar' : 'Tela'}</span>
        <i class="state-light" aria-hidden="true"></i>
      </button>
    </div>

    <p class="presence-note">
      {#if screenSharing}
        Sua tela está sendo compartilhada e abre em uma janela nativa.
      {:else if self?.in_call}
        Mudo, áudio e tela controlam a sua chamada agora.
      {:else}
        Entre na chamada de voz para compartilhar a tela.
      {/if}
    </p>

    {#if panelError}
      <div class="panel-error" role="status">
        <Icon name="warning" size={13} />
        <span>{panelError}</span>
        <button onclick={() => (panelError = null)} aria-label="Fechar erro">
          <Icon name="close" size={12} />
        </button>
      </div>
    {/if}
  </section>

  <div class="panel-spacer" aria-hidden="true"></div>

  <section class="server-card" aria-label="Informações do servidor">
    <div class="server-head">
      <span class="server-icon"><Icon name="server" size={14} /></span>
      <div class="server-copy">
        <span class="mono-label">Servidor</span>
        <strong>{serverName ?? 'Concord'}</strong>
      </div>
    </div>

    <details><summary>Diagnóstico do servidor</summary>
    <dl class="server-meta">
      <div>
        <dt>versão</dt>
        <dd class="tabular">v{serverVersion ?? '—'}</dd>
      </div>
      <div>
        <dt>protocolo</dt>
        <dd class="tabular">v{protocolVersion ?? '—'}</dd>
      </div>
      <div>
        <dt>acesso</dt>
        <dd>por convite</dd>
      </div>
    </dl></details>

    <p class="privacy-note">
      <Icon name="lock" size={11} />
      <span>Token validado na entrada. Nenhum diretório público.</span>
    </p>
  </section>

  <button class="disconnect" onclick={disconnect} disabled={busy}>
    <Icon name="logout" size={14} />
    <span>{leaving ? 'Encerrando…' : 'Desconectar do servidor'}</span>
  </button>
</aside>

<style>
  .participant-entry { border-radius: 10px; }
  .participant.speaking .avatar-wrap { border-radius: 11px; outline: 2px solid var(--lime); outline-offset: 3px; box-shadow: 0 0 12px var(--lime-soft); }
  .participant.speaking .presence-line { color: var(--lime); }
  .participant.speaking .presence-dot { background: var(--lime); box-shadow: 0 0 8px var(--lime); }

  .participants {
    position: relative;
    z-index: 2;
    display: flex;
    flex-direction: column;
    min-height: 0;
    height: 100dvh;
    padding: 22px 15px 16px;
    border-left: 1px solid var(--line);
    background:
      linear-gradient(180deg, rgba(241, 232, 215, 0.025), transparent 24%),
      var(--rail);
    overflow: hidden;
  }

  .panel-accent {
    position: absolute;
    top: 0;
    left: -1px;
    width: 1px;
    height: 130px;
    background: linear-gradient(180deg, var(--lime), transparent);
    opacity: 0.22;
    pointer-events: none;
  }

  .panel-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
    padding: 0 5px;
  }

  .panel-head h2 {
    margin: 5px 0 0;
    color: var(--ink-soft);
    font-size: 21px;
    font-weight: 480;
    line-height: 1.1;
  }

  .count-pill {
    display: grid;
    place-items: center;
    min-width: 27px;
    height: 22px;
    padding: 0 7px;
    border: 1px solid rgba(217, 242, 107, 0.15);
    border-radius: 999px;
    background: var(--lime-soft);
    color: color-mix(in srgb, var(--lime) 82%, var(--ink));
    font-size: 9.5px;
    font-weight: 600;
    letter-spacing: 0.04em;
  }

  .live-strip {
    display: flex;
    align-items: center;
    gap: 7px;
    margin: 17px 5px 12px;
    color: var(--faint);
    font-size: 8.5px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .live-dot {
    width: 5px;
    height: 5px;
    flex: none;
    border-radius: 50%;
    background: var(--lime);
    box-shadow: 0 0 8px rgba(217, 242, 107, 0.62);
  }

  .live-line {
    flex: 1;
    height: 1px;
    background: var(--line);
  }

  .participant-list {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-height: 0;
    max-height: 42vh;
    overflow-y: auto;
    padding-right: 2px;
  }

  .participant {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 8px 8px 8px 7px;
    border: 1px solid transparent;
    border-radius: 10px;
    transition:
      background 140ms ease,
      border-color 140ms ease,
      opacity 140ms ease;
  }

  .participant:hover {
    background: rgba(241, 232, 215, 0.035);
  }

  .participant.self {
    border-color: rgba(217, 242, 107, 0.13);
    background:
      linear-gradient(90deg, rgba(217, 242, 107, 0.075), transparent 80%),
      rgba(241, 232, 215, 0.025);
  }

  .participant.away {
    opacity: 0.66;
  }

  .avatar-wrap {
    position: relative;
    flex: none;
  }

  .presence-dot {
    position: absolute;
    right: -2px;
    bottom: -2px;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    border: 2px solid var(--rail);
    background: var(--lime);
  }

  .participant.away .presence-dot {
    background: var(--faint);
  }

  .participant-copy {
    display: flex;
    flex-direction: column;
    gap: 1px;
    flex: 1;
    min-width: 0;
  }

  .name-row {
    display: flex;
    align-items: center;
    gap: 5px;
    min-width: 0;
  }

  .name-row strong {
    overflow: hidden;
    color: var(--ink-soft);
    font-size: 12px;
    font-weight: 620;
    line-height: 1.35;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .participant.self .name-row strong {
    color: var(--ink);
  }

  .you-tag {
    flex: none;
    padding: 0 4px;
    border-radius: 3px;
    background: rgba(217, 242, 107, 0.11);
    color: color-mix(in srgb, var(--lime) 72%, var(--faint));
    font-size: 7.5px;
    font-weight: 650;
    letter-spacing: 0.04em;
    text-transform: lowercase;
  }

  .presence-line {
    overflow: hidden;
    color: var(--faint);
    font-size: 9.5px;
    line-height: 1.35;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .flags {
    display: flex;
    align-items: center;
    gap: 3px;
    flex: none;
  }

  .flag {
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    border-radius: 5px;
    background: rgba(241, 232, 215, 0.045);
    color: var(--faint);
  }

  .flag.sharing {
    color: var(--sky);
    background: color-mix(in srgb, var(--sky) 9%, transparent);
  }

  .flag.call {
    color: var(--lime);
    background: var(--lime-soft);
  }

  .flag.quiet {
    color: color-mix(in srgb, var(--lime) 50%, var(--faint));
    background: transparent;
  }

  .empty {
    margin: 4px 5px;
    padding: 12px;
    border: 1px dashed var(--line);
    border-radius: 9px;
    color: var(--faint);
    font-size: 10.5px;
    line-height: 1.5;
  }

  .presence-panel {
    margin-top: 17px;
    padding-top: 15px;
    border-top: 1px solid var(--line);
  }

  .section-label {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 0 5px;
  }

  .section-label h3 {
    margin: 0;
  }

  .syncing {
    color: var(--amber);
    font-size: 8px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    animation: pulse-soft 1.1s ease-in-out infinite;
  }

  .presence-controls {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 5px;
    margin-top: 10px;
  }

  .presence-button {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    padding: 9px 4px 8px;
    border: 1px solid var(--line);
    border-radius: 9px;
    background: rgba(8, 10, 9, 0.2);
    color: var(--faint);
    transition:
      color 140ms ease,
      border-color 140ms ease,
      background 140ms ease;
  }

  .presence-button:hover:not(:disabled) {
    color: var(--ink-soft);
    border-color: var(--line-strong);
    background: rgba(241, 232, 215, 0.04);
  }

  .presence-button.active {
    border-color: color-mix(in srgb, var(--coral) 25%, transparent);
    background: color-mix(in srgb, var(--coral) 7%, transparent);
    color: var(--coral);
  }

  .presence-button:disabled {
    opacity: 0.5;
  }

  .button-icon {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
  }

  .presence-button > span:not(.button-icon) {
    font-size: 8.5px;
    font-weight: 600;
    letter-spacing: 0.02em;
    white-space: nowrap;
  }

  .state-light {
    position: absolute;
    top: 7px;
    right: 7px;
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: rgba(241, 232, 215, 0.14);
  }

  .presence-button.active .state-light {
    background: var(--coral);
    box-shadow: 0 0 7px color-mix(in srgb, var(--coral) 55%, transparent);
  }

  .presence-note {
    margin: 9px 5px 0;
    color: var(--faint);
    font-size: 9.5px;
    line-height: 1.45;
  }

  .panel-error {
    display: flex;
    align-items: flex-start;
    gap: 6px;
    margin-top: 9px;
    padding: 7px 8px;
    border: 1px solid color-mix(in srgb, var(--coral) 24%, transparent);
    border-radius: 7px;
    background: color-mix(in srgb, var(--coral) 7%, transparent);
    color: var(--coral);
    font-size: 9.5px;
    line-height: 1.45;
  }

  .panel-error span {
    flex: 1;
    min-width: 0;
  }

  .panel-error button {
    display: grid;
    place-items: center;
    width: 17px;
    height: 17px;
    flex: none;
    border-radius: 4px;
    color: inherit;
  }

  .panel-error button:hover {
    background: rgba(255, 159, 139, 0.1);
  }

  .panel-spacer {
    flex: 1;
    min-height: 16px;
  }

  .server-card {
    flex: none;
    margin: 0 2px;
    padding: 11px 10px 10px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background:
      linear-gradient(160deg, rgba(241, 232, 215, 0.03), transparent 60%),
      rgba(8, 10, 9, 0.22);
  }

  .server-head {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  .server-icon {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    flex: none;
    border-radius: 7px;
    background: rgba(241, 232, 215, 0.055);
    color: var(--muted);
  }

  .server-copy {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }

  .server-copy strong {
    overflow: hidden;
    color: var(--ink-soft);
    font-family: var(--font-display);
    font-size: 14px;
    font-weight: 500;
    line-height: 1.2;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .server-meta {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 7px 8px;
    margin: 12px 0 0;
    padding-top: 9px;
    border-top: 1px solid var(--line);
  }

  .server-meta div {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }

  .server-meta div:last-child {
    grid-column: 1 / -1;
  }

  .server-meta dt {
    color: var(--faint);
    font-size: 7.5px;
    font-weight: 600;
    letter-spacing: 0.09em;
    text-transform: uppercase;
  }

  .server-meta dd {
    margin: 0;
    overflow: hidden;
    color: var(--muted);
    font-size: 9.5px;
    line-height: 1.35;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .privacy-note {
    display: flex;
    align-items: flex-start;
    gap: 5px;
    margin: 10px 0 0;
    padding-top: 9px;
    border-top: 1px solid var(--line);
    color: var(--faint);
    font-size: 8.5px;
    line-height: 1.45;
  }

  .privacy-note :global(svg) {
    margin-top: 1px;
    flex: none;
  }

  .disconnect {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    flex: none;
    width: calc(100% - 4px);
    height: 31px;
    margin: 9px 2px 0;
    border: 1px solid var(--line);
    border-radius: 8px;
    color: var(--faint);
    font-size: 9.5px;
    font-weight: 600;
    transition:
      color 130ms ease,
      border-color 130ms ease,
      background 130ms ease;
  }

  .disconnect:hover:not(:disabled) {
    color: var(--coral);
    border-color: color-mix(in srgb, var(--coral) 24%, transparent);
    background: color-mix(in srgb, var(--coral) 7%, transparent);
  }

  .disconnect:disabled {
    opacity: 0.55;
  }
</style>
