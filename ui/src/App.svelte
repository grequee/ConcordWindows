<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import * as api from './lib/api';
  import { describeError } from './lib/api';
  import DesktopSettings from './lib/DesktopSettings.svelte';
  import Downloads from './lib/DownloadsPanel.svelte';
  import { updateDownload, type Download } from './lib/downloads.svelte';
  import { listen } from '@tauri-apps/api/event';
  import { matchesShortcut } from './lib/shortcut';
  import { readKey, readPosition, unreadMessages } from './lib/unread';
  import Avatar from './lib/Avatar.svelte';
  import Brand from './lib/Brand.svelte';
  import ChatView from './lib/ChatView.svelte';
  import ConnectPanel from './lib/ConnectPanel.svelte';
  import Icon from './lib/Icon.svelte';
  import MicrophoneSettings from './lib/MicrophoneSettings.svelte';
  import Participants from './lib/Participants.svelte';
  import RoomList from './lib/RoomList.svelte';
  import { emptySnapshot, type RoomSummary, type Snapshot, type AudioActivityEvent } from './lib/types';

  let snapshot = $state<Snapshot>(emptySnapshot);
  let desktop = $state<api.DesktopStatus | null>(null);
  let focused = $state(document.hasFocus());
  let serverScope = $state((() => { try { return localStorage.getItem('concord.serverUrl') ?? ''; } catch { return ''; } })());
  let positions = $state<Record<string, number>>({});
  let viewPosition = $state({ room: '', bottom: false });
  const activeRoom = $derived(snapshot.active_room);
  const selfId = $derived(snapshot.self_id ?? '');
  const positionKey = $derived(activeRoom ? readKey(`${serverScope}|${snapshot.account?.id ?? 'guest'}`, activeRoom.room.id) : '');
  const lastRead = $derived(positions[positionKey] ?? null);
  const unread = $derived(activeRoom && lastRead !== null ? unreadMessages(activeRoom.messages, lastRead, selfId, snapshot.self_author_id) : []);
  function markRead() {
    if (!activeRoom) return;
    const latest = activeRoom.messages.at(-1)?.id ?? 0;
    if (latest <= (positions[positionKey] ?? -1)) return;
    positions[positionKey] = latest;
    try { localStorage.setItem(positionKey, String(latest)); } catch { /* Keep the position in memory. */ }
  }
  $effect(() => {
    const room = activeRoom; const key = positionKey;
    if (!room) return;
    untrack(() => {
      if (positions[key] === undefined) {
        const max = room.messages.at(-1)?.id ?? 0;
        let saved = readPosition(localStorage, key);
        if (!snapshot.chat_persistent && saved !== null && saved > max) saved = max;
        positions[key] = saved ?? max;
        if (saved === null) { try { localStorage.setItem(key, String(max)); } catch { /* Local memory remains available. */ } }
      }
    });
  });
  $effect(() => {
    const room = activeRoom;
    if (room && focused && viewPosition.room === room.room.id && viewPosition.bottom) {
      void room.messages.at(-1)?.id;
      untrack(markRead);
    }
  });
  function shortcut(event: KeyboardEvent) {
    if (!event.defaultPrevented && desktop && !desktop.global_shortcut_active && matchesShortcut(event, desktop.settings.mute_shortcut)) {
      event.preventDefault(); void api.toggleMicrophoneShortcut().catch(cause => commandError = describeError(cause));
    }
  }
  let bootError = $state<string | null>(null);
  let commandError = $state<string | null>(null);
  let gotLiveEvent = false;
  let audioActivity = $state<AudioActivityEvent | null>(null);

  const connected = $derived(snapshot.status === 'connected');
  const connecting = $derived(['connecting', 'waiting_for_server', 'reconnecting'].includes(snapshot.status));
  const connectionLabel = $derived(
    snapshot.status === 'reconnecting' ? 'reconectando'
      : snapshot.status === 'waiting_for_server' ? 'aguardando'
      : connecting ? 'conectando' : connected ? 'online' : 'offline',
  );

  const visibleError = $derived(
    commandError ?? (connected ? snapshot.error?.message ?? null : null),
  );
  const visibleErrorCode = $derived(
    commandError ? null : (snapshot.error?.code ?? null),
  );

  onMount(() => {
    let alive = true;
    let unlisten: (() => void) | undefined;
    const extra: (() => void)[] = [];
    let unlistenActivity: (() => void) | undefined;

    void (async () => {
      try {
        extra.push(await api.listenDesktopSettings(status => { if (alive) desktop = status; }));
        extra.push(await listen<Partial<Download> & { id: string }>('concord:download-progress', event => updateDownload(event.payload)));
        desktop = await api.getDesktopSettings();
        unlisten = await api.listenToState((next) => {
          if (!alive) return;
          gotLiveEvent = true;
          snapshot = next;
          bootError = null;
        });
        unlistenActivity = await api.listenAudioActivity((event) => { if (alive) audioActivity = event; });
        const initial = await api.getSnapshot();
        if (alive && !gotLiveEvent) snapshot = initial;
        if (alive && snapshot.status === 'disconnected' && await api.getSavedAccount(serverScope)) {
          try { await api.authenticateAccount({ server_url: serverScope, action: 'resume' }); }
          catch (cause) { if (alive) commandError = describeError(cause); }
        }
      } catch (error) {
        if (alive) bootError = describeError(error);
      }
    })();

    return () => {
      alive = false;
      extra.forEach(stop => stop());
      unlisten?.();
      unlistenActivity?.();
    };
  });

  $effect(() => {
    document.title = activeRoom ? `${unread.length ? `(${unread.length}) ` : ""}Concord — ${activeRoom.room.name}` : 'Concord';
  });

  async function handleAccount(request: api.AccountRequest) {
    commandError = null;
    serverScope = request.server_url.trim();
    return await api.authenticateAccount(request);
  }

  async function handleJoin(roomId: string) {
    commandError = null;
    const currentRoomId = snapshot.active_room?.room.id;
    if (currentRoomId === roomId) return;

    try {
      if (currentRoomId) await api.leaveRoom();
      await api.joinRoom(roomId);
    } catch (error) {
      commandError = describeError(error);
    }
  }

  async function handleRefresh() {
    commandError = null;
    try {
      await api.refreshRooms();
    } catch (error) {
      commandError = describeError(error);
    }
  }

  async function handleSend(text: string, file: File | api.NativeChatAttachment | null, clientId: string, progress: (percent: number) => void, replyTo?: number | null) {
    commandError = null;
    if (file instanceof File) await api.sendChatFile(text, file, clientId, progress, replyTo);
    else if (file) await api.sendChatAttachmentPath(text, file, clientId, progress, replyTo);
    else await api.sendChat(text, null, clientId, replyTo);
  }

  async function handleLeave() {
    commandError = null;
    try {
      await api.leaveRoom();
    } catch (error) {
      commandError = describeError(error);
      throw error;
    }
  }

  async function handleJoinCall() {
    commandError = null;
    try {
      await api.joinCall();
    } catch (error) {
      commandError = describeError(error);
      throw error;
    }
  }

  async function handleLeaveCall() {
    commandError = null;
    try {
      await api.leaveCall();
    } catch (error) {
      commandError = describeError(error);
      throw error;
    }
  }

  async function handleDisconnect() {
    commandError = null;
    try {
      if (snapshot.account) await api.logoutAccount();
      else await api.disconnectFromServer();
    } catch (error) {
      commandError = describeError(error);
    }
  }

  function dismissError() {
    commandError = null;
    void api.clearError().catch(() => undefined);
  }

  function errorCodeLabel(code: string | null): string {
    if (!code) return 'Atenção';
    const labels: Record<string, string> = {
      unauthorized: 'Acesso negado',
      bad_protocol_version: 'Versão incompatível',
      rate_limited: 'Calma lá',
      invalid_message: 'Mensagem inválida',
      message_too_large: 'Mensagem grande',
      not_authenticated: 'Sessão inválida',
      already_authenticated: 'Sessão duplicada',
      unknown_room: 'Sala inexistente',
      room_full: 'Sala cheia',
      already_in_room: 'Você já está numa sala',
      not_in_room: 'Entre numa sala',
      target_unavailable: 'Parceiro indisponível',
      server_full: 'Servidor cheio',
      internal: 'Erro interno',
    };
    return labels[code] ?? code.replaceAll('_', ' ');
  }

  async function joinFromLobby(room: RoomSummary) {
    await handleJoin(room.id);
  }
</script>

<svelte:window onkeydown={shortcut} onfocus={() => focused = true} onblur={() => focused = false} />
<svelte:document onvisibilitychange={() => focused = document.visibilityState === "visible" && document.hasFocus()} />
<Downloads />
<div class="shell" class:with-right={connected && activeRoom}>
  <aside class="rail" aria-label="Navegação do Concord">
    <div class="rail-head">
      <Brand size={36} />
      <div class="status-pill" class:online={connected} class:pending={connecting} aria-live="polite">
        <span class="status-dot" aria-hidden="true"></span>
        <span>{connectionLabel}</span>
      </div>
    </div>

    <div class="rail-divider"></div>

    {#if connected}
      <div class="server-context">
        <span class="mono-label">Servidor</span>
        <strong>{snapshot.server_name ?? 'Concord'}</strong>
        
      </div>

      <RoomList
        unreadCounts={activeRoom ? { [activeRoom.room.id]: unread.length } : {}}
        rooms={snapshot.rooms}
        activeRoomId={activeRoom?.room.id ?? null}
        onjoin={handleJoin}
        onrefresh={handleRefresh}
      />

      <div class="rail-spacer"></div>

      <div class="manifesto-note">
        <span class="note-index">01</span>
        <p>Um grupo pequeno não precisa de um palco. Precisa de um lugar.</p>
      </div>

      <div class="rail-footer">
        <div class="self-card">
          <Avatar name={snapshot.display_name ?? 'Você'} id={selfId} size={34} />
          <div class="self-copy">
            <strong>{snapshot.display_name ?? 'Você'}</strong>
            <span>{snapshot.account ? `@${snapshot.account.username}` : 'sessão local'}</span>
          </div>
          <button
            class="leave-server"
            onclick={handleDisconnect}
            title={snapshot.account ? "Sair da conta" : "Desconectar"}
            aria-label={snapshot.account ? "Sair da conta" : "Desconectar do servidor"}
          >
            <Icon name="logout" size={16} />
          </button>
        </div>
        <div class="build-line">
          <span>CONCORD</span>
          <span>seu grupo, por perto</span>
        </div>
      </div>
    {:else}
      <div class="offline-copy">
        <span class="mono-label">Um lugar tranquilo</span>
        <p class="display">
          Para conversar<br />
          sem plateia.
        </p>
        <p class="offline-body">
          Texto em tempo real, voz e compartilhamento de tela. A chamada acontece direto
          entre os participantes.
        </p>
      </div>

      <div class="rail-spacer"></div>

      <div class="offline-steps">
        <span class="mono-label">Como funciona</span>
        <ol>
          <li><span>01</span> O servidor da turma pode estar na internet.</li>
          <li><span>02</span> Crie sua conta com o convite da turma.</li>
          <li><span>03</span> Depois, é só escolher uma sala.</li>
        </ol>
      </div>

      <div class="rail-footer">
        <div class="build-line">
          <span>BUILD 0.1.0</span>
          <span>CONCORD</span>
        </div>
      </div>
    {/if}
    <DesktopSettings status={desktop} />
    <MicrophoneSettings settings={snapshot.microphone} audioDevices={snapshot.audio_devices} activity={audioActivity} selfId={selfId}
      desktop={desktop} ondesktopupdate={status => desktop = status}
      noiseSuppressionAvailable={snapshot.noise_suppression_available} call={snapshot.call} />
  </aside>

  <main class="stage" aria-label="Área principal">
    <div class="stage-glow" aria-hidden="true"></div>
    <div class="stage-grid" aria-hidden="true"></div>

    {#if connected && snapshot.connection_message}
      <div class="boot-error" role="status">{snapshot.connection_message}</div>
    {/if}

    {#if visibleError}
      <div class="toast" role="status">
        <div class="toast-icon"><Icon name="warning" size={18} /></div>
        <div class="toast-copy">
          <span class="toast-code">{errorCodeLabel(visibleErrorCode)}</span>
          <span class="toast-message">{visibleError}</span>
        </div>
        <button class="toast-close" onclick={dismissError} aria-label="Fechar aviso">
          <Icon name="close" size={15} />
        </button>
      </div>
    {/if}

    {#if bootError && !connected}
      <div class="boot-error" role="status">
        <Icon name="warning" size={15} />
        <span>{bootError}</span>
      </div>
    {/if}

    {#if connected && activeRoom}
      <ChatView {lastRead} unreadCount={unread.length} onposition={(room, bottom) => viewPosition = { room, bottom }} richChat={snapshot.rich_chat} selfAuthorId={snapshot.self_author_id}
        persistent={snapshot.chat_persistent}
        imagesEnabled={snapshot.image_uploads}
        attachmentsEnabled={snapshot.attachment_uploads}
        room={activeRoom}
        selfId={selfId}
        call={snapshot.call}
        onsend={handleSend}
        onleave={handleLeave}
        oncalljoin={handleJoinCall}
        oncallleave={handleLeaveCall}
      />
    {:else if connected}
      <div class="lobby">
        <header class="lobby-head">
          <div>
            <span class="eyebrow"><i></i>Conectado a {snapshot.server_name ?? 'servidor'}</span>
            <h1 class="display">Escolha uma sala.</h1>
            <p>
              O texto chega em tempo real. Quando você entrar, os participantes
              desta sessão aparecem à direita.
            </p>
          </div>
          <div class="lobby-stats">
            <div>
              <strong class="tabular">{snapshot.rooms.length.toString().padStart(2, '0')}</strong>
              <span>salas</span>
            </div>
            <div>
              <strong class="tabular">
                {snapshot.rooms.reduce((total, room) => total + room.member_count, 0)
                  .toString()
                  .padStart(2, '0')}
              </strong>
              <span>presentes</span>
            </div>
          </div>
        </header>

        {#if snapshot.rooms.length > 0}
          <div class="room-cards">
            {#each snapshot.rooms as room, index (room.id)}
              <button class="room-card" onclick={() => joinFromLobby(room)}>
                <span class="card-number tabular">{(index + 1).toString().padStart(2, '0')}</span>
                <span class="card-icon"><Icon name="hash" size={20} /></span>
                <span class="card-copy">
                  <strong>{room.name}</strong>
                  <span>{room.description ?? 'Uma sala do grupo.'}</span>
                </span>
                <span class="card-foot">
                  <span class="capacity-ticks" aria-hidden="true">
                    {#each Array.from({ length: Math.min(room.capacity, 8) }) as _, slot}
                      <i class:filled={slot < room.member_count}></i>
                    {/each}
                  </span>
                  <span class="card-count tabular">{room.member_count}/{room.capacity}</span>
                  <span class="card-arrow"><Icon name="arrow-right" size={16} /></span>
                </span>
              </button>
            {/each}
          </div>
        {:else}
          <div class="lobby-empty">
            <Icon name="users" size={24} />
            <p>Nenhuma sala configurada no servidor.</p>
            <button onclick={handleRefresh}>Atualizar lista</button>
          </div>
        {/if}

        <footer class="lobby-foot">
          <span><Icon name="lock" size={13} /> Conta autenticada</span>
          <span>protocolo v{snapshot.protocol_version ?? 1}</span>
          
        </footer>
      </div>
    {:else}
      <div class="landing">
        <div class="landing-copy">
          <span class="eyebrow"><i></i>Comunicação privada</span>
          <h1 class="display">
            Conversa.<br />
            <span class="quiet">Sem ruído.</span>
          </h1>
          <p class="lede">
            Um app de voz, texto e tela para o seu grupo — não para uma audiência.
            Leve por dentro, privado por escolha.
          </p>

          <div class="principles">
            <div class="principle">
              <span class="principle-index">01</span>
              <div>
                <strong>Grupo fechado</strong>
                <p>O convite libera seu cadastro. Depois, entre com sua conta. Sem diretório público.</p>
              </div>
            </div>
            <div class="principle">
              <span class="principle-index">02</span>
              <div>
                <strong>Rápido de verdade</strong>
                <p>Rust no núcleo, WebView só como interface. Nada de Chromium embutido.</p>
              </div>
            </div>
            <div class="principle">
              <span class="principle-index">03</span>
              <div>
                <strong>Seu servidor</strong>
                <p>Sinalização na sua rede. A mídia segue direto entre os participantes.</p>
              </div>
            </div>
          </div>

          <div class="landing-signature">
            <span class="sig-line"></span>
            <span>feito para durar mais que o hype</span>
          </div>
        </div>

        <div class="landing-action">
          <div class="arc" aria-hidden="true">
            <span></span><span></span><span></span>
            <i></i>
          </div>
          <ConnectPanel
            {connecting}
            connectionMessage={snapshot.connection_message}
            connectionAttempt={snapshot.connection_attempt}
            reconnecting={snapshot.status === 'reconnecting'}
            serverError={snapshot.error}
            onauthenticate={handleAccount}
            oncancel={handleDisconnect}
            externalError={commandError}
            onclearerror={dismissError}
          />
          <div class="landing-meta">
            <span class="mono-label">Concord</span>
            <span>sinalização · salas · texto · voz · tela</span>
          </div>
        </div>
      </div>

      <div class="version-stamp" aria-hidden="true">
        <span>PRIVATE COMMS</span>
        <span>0.1.0 / M4</span>
      </div>
    {/if}
  </main>

  {#if connected && activeRoom}
    <Participants
      call={snapshot.call} activity={audioActivity}
      room={activeRoom}
      selfId={selfId}
      serverName={snapshot.server_name}
      serverVersion={snapshot.server_version}
      protocolVersion={snapshot.protocol_version}
      ondisconnect={handleDisconnect}
    />
  {/if}
</div>

<style>
  .shell {
    position: relative;
    display: grid;
    grid-template-columns: 270px minmax(0, 1fr);
    height: 100dvh;
    background: var(--bg);
    overflow: hidden;
  }

  .shell.with-right {
    grid-template-columns: 270px minmax(0, 1fr) 254px;
  }

  .rail {
    position: relative;
    z-index: 2;
    display: flex;
    flex-direction: column;
    min-height: 0;
    padding: 22px 16px 18px;
    border-right: 1px solid var(--line);
    background:
      linear-gradient(180deg, rgba(241, 232, 215, 0.025), transparent 30%),
      var(--rail);
  }

  .rail::after {
    content: '';
    position: absolute;
    top: 0;
    right: -1px;
    width: 1px;
    height: 160px;
    background: linear-gradient(180deg, var(--lime), transparent);
    opacity: 0.25;
  }

  .rail-head {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 17px;
    padding: 0 6px;
  }

  .status-pill {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 22px;
    padding: 0 9px;
    border: 1px solid var(--line);
    border-radius: 999px;
    background: rgba(8, 10, 9, 0.34);
    color: var(--faint);
    font-size: 9.5px;
    font-weight: 600;
    letter-spacing: 0.09em;
    text-transform: uppercase;
  }

  .status-dot {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--faint);
  }

  .status-pill.online {
    border-color: rgba(217, 242, 107, 0.18);
    color: color-mix(in srgb, var(--lime) 80%, var(--ink));
  }

  .status-pill.online .status-dot {
    background: var(--lime);
    box-shadow: 0 0 8px rgba(217, 242, 107, 0.65);
  }

  .status-pill.pending {
    color: var(--amber);
    border-color: color-mix(in srgb, var(--amber) 22%, transparent);
  }

  .status-pill.pending .status-dot {
    background: var(--amber);
    animation: pulse-soft 1.1s ease-in-out infinite;
  }

  .rail-divider {
    height: 1px;
    margin: 19px 6px 18px;
    background: var(--line);
  }

  .server-context {
    display: grid;
    grid-template-columns: 1fr auto;
    align-items: baseline;
    gap: 2px 8px;
    padding: 0 10px 14px;
  }

  .server-context .mono-label {
    grid-column: 1 / -1;
  }

  .server-context strong {
    overflow: hidden;
    color: var(--ink-soft);
    font-family: var(--font-display);
    font-size: 16px;
    font-weight: 500;
    letter-spacing: -0.01em;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  :global(.server-version) {
    color: var(--faint);
    font-size: 9px;
    font-variant-numeric: tabular-nums;
  }

  .rail-spacer {
    flex: 1;
    min-height: 18px;
  }

  .manifesto-note {
    display: flex;
    gap: 9px;
    margin: 18px 6px;
    padding: 12px 10px;
    border-top: 1px solid var(--line);
    border-bottom: 1px solid var(--line);
  }

  .note-index {
    color: var(--lime);
    font-family: var(--font-display);
    font-size: 12px;
    line-height: 1.5;
  }

  .manifesto-note p {
    margin: 0;
    color: var(--faint);
    font-family: var(--font-display);
    font-size: 12px;
    font-style: italic;
    line-height: 1.5;
  }

  .rail-footer {
    padding: 0 6px;
  }

  .self-card {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 8px;
    border: 1px solid var(--line);
    border-radius: 11px;
    background: rgba(8, 10, 9, 0.28);
  }

  .self-copy {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
    line-height: 1.3;
  }

  .self-copy strong {
    overflow: hidden;
    color: var(--ink-soft);
    font-size: 12px;
    font-weight: 650;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .self-copy span {
    color: var(--faint);
    font-size: 9.5px;
  }

  .leave-server {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 7px;
    color: var(--faint);
    transition: color 130ms ease, background 130ms ease;
  }

  .leave-server:hover {
    color: var(--coral);
    background: color-mix(in srgb, var(--coral) 10%, transparent);
  }

  .build-line {
    display: flex;
    justify-content: space-between;
    margin-top: 13px;
    color: var(--faint);
    font-size: 8px;
    font-weight: 600;
    letter-spacing: 0.11em;
  }

  .offline-copy {
    padding: 4px 10px 0;
  }

  .offline-copy .display {
    margin: 16px 0 12px;
    color: var(--ink-soft);
    font-size: 27px;
    font-weight: 450;
    line-height: 1.12;
  }

  .offline-body {
    margin: 0;
    color: var(--faint);
    font-size: 11.5px;
    line-height: 1.6;
  }

  .offline-steps {
    margin: 0 10px;
    padding-top: 16px;
    border-top: 1px solid var(--line);
  }

  .offline-steps ol {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin: 13px 0 0;
    padding: 0;
    list-style: none;
  }

  .offline-steps li {
    display: flex;
    gap: 8px;
    color: var(--faint);
    font-size: 10.5px;
    line-height: 1.45;
  }

  .offline-steps li span {
    color: color-mix(in srgb, var(--lime) 65%, var(--faint));
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0.04em;
  }

  .stage {
    position: relative;
    z-index: 1;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    background:
      linear-gradient(135deg, rgba(241, 232, 215, 0.018), transparent 34%),
      var(--bg);
  }

  .stage-glow {
    position: absolute;
    top: -220px;
    right: -160px;
    width: 620px;
    height: 620px;
    border-radius: 50%;
    background: radial-gradient(
      circle,
      rgba(217, 242, 107, 0.075),
      rgba(217, 242, 107, 0.018) 44%,
      transparent 68%
    );
    pointer-events: none;
  }

  .stage-grid {
    position: absolute;
    inset: 0;
    background-image: radial-gradient(rgba(241, 232, 215, 0.075) 0.7px, transparent 0.7px);
    background-size: 22px 22px;
    mask-image: linear-gradient(145deg, rgba(0, 0, 0, 0.7), transparent 52%);
    opacity: 0.22;
    pointer-events: none;
  }

  .landing {
    position: relative;
    z-index: 1;
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(340px, 430px);
    align-items: center;
    gap: clamp(30px, 5vw, 80px);
    height: 100%;
    max-width: 1180px;
    margin: 0 auto;
    padding: clamp(40px, 6vh, 78px) clamp(34px, 5vw, 78px);
    overflow-y: auto;
  }

  .landing-copy {
    position: relative;
    max-width: 580px;
    animation: rise-in 600ms cubic-bezier(0.22, 0.8, 0.24, 1) both;
  }

  .eyebrow {
    display: inline-flex;
    align-items: center;
    gap: 9px;
    color: var(--faint);
    font-size: 9.5px;
    font-weight: 600;
    letter-spacing: 0.15em;
    text-transform: uppercase;
  }

  .eyebrow i {
    display: block;
    width: 22px;
    height: 1px;
    background: var(--lime);
  }

  .landing h1 {
    margin: 24px 0 24px;
    font-size: clamp(64px, 7vw, 98px);
    font-weight: 450;
    line-height: 0.92;
    letter-spacing: -0.045em;
  }

  .landing h1 .quiet {
    color: var(--muted);
    font-weight: 380;
  }

  .lede {
    max-width: 42ch;
    margin: 0;
    color: var(--muted);
    font-size: 14px;
    line-height: 1.66;
  }

  .principles {
    display: flex;
    flex-direction: column;
    gap: 0;
    max-width: 460px;
    margin-top: 40px;
    border-top: 1px solid var(--line);
  }

  .principle {
    display: flex;
    gap: 16px;
    padding: 14px 0;
    border-bottom: 1px solid var(--line);
  }

  .principle-index {
    padding-top: 2px;
    color: var(--faint);
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0.06em;
  }

  .principle strong {
    display: block;
    margin-bottom: 3px;
    color: var(--ink-soft);
    font-size: 12px;
    font-weight: 650;
  }

  .principle p {
    margin: 0;
    color: var(--faint);
    font-size: 11.5px;
    line-height: 1.5;
  }

  .landing-signature {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-top: 30px;
    color: var(--faint);
    font-family: var(--font-display);
    font-size: 11px;
    font-style: italic;
  }

  .sig-line {
    width: 36px;
    height: 1px;
    background: var(--line-strong);
  }

  .landing-action {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 13px;
    min-width: 0;
    animation: rise-in 700ms 90ms cubic-bezier(0.22, 0.8, 0.24, 1) both;
  }

  .arc {
    position: absolute;
    top: -92px;
    right: -62px;
    width: 260px;
    height: 260px;
    pointer-events: none;
    opacity: 0.5;
  }

  .arc span {
    position: absolute;
    inset: 0;
    border: 1px solid rgba(217, 242, 107, 0.14);
    border-radius: 50%;
  }

  .arc span:nth-child(2) {
    inset: 32px;
  }

  .arc span:nth-child(3) {
    inset: 66px;
  }

  .arc i {
    position: absolute;
    top: 50%;
    left: 50%;
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--lime);
    box-shadow: 0 0 14px rgba(217, 242, 107, 0.7);
  }

  .landing-meta {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 3px;
    color: var(--faint);
    font-size: 9px;
    letter-spacing: 0.04em;
  }

  .version-stamp {
    position: absolute;
    right: 18px;
    bottom: 18px;
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 2px;
    color: rgba(241, 232, 215, 0.16);
    font-size: 7.5px;
    font-weight: 600;
    letter-spacing: 0.16em;
    pointer-events: none;
  }

  .lobby {
    position: relative;
    z-index: 1;
    display: flex;
    flex-direction: column;
    min-height: 100%;
    padding: clamp(34px, 5.5vh, 66px) clamp(30px, 4.4vw, 64px) 26px;
    overflow-y: auto;
  }

  .lobby-head {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: 30px;
    max-width: 1000px;
    animation: rise-in 450ms ease both;
  }

  .lobby-head h1 {
    margin: 14px 0 10px;
    font-size: clamp(42px, 5vw, 68px);
    font-weight: 450;
    line-height: 0.98;
  }

  .lobby-head p {
    max-width: 44ch;
    margin: 0;
    color: var(--muted);
    font-size: 13px;
    line-height: 1.6;
  }

  .lobby-stats {
    display: flex;
    gap: 24px;
    padding-bottom: 4px;
  }

  .lobby-stats div {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 1px;
  }

  .lobby-stats strong {
    color: var(--ink-soft);
    font-family: var(--font-display);
    font-size: 27px;
    font-weight: 420;
    line-height: 1;
  }

  .lobby-stats span {
    color: var(--faint);
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
  }

  .room-cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
    gap: 12px;
    max-width: 1000px;
    margin-top: 42px;
  }

  .room-card {
    position: relative;
    display: flex;
    flex-direction: column;
    min-height: 174px;
    padding: 17px;
    border: 1px solid var(--line);
    border-radius: 14px;
    background:
      linear-gradient(155deg, rgba(241, 232, 215, 0.035), transparent 50%),
      rgba(17, 21, 17, 0.62);
    text-align: left;
    transition:
      transform 160ms ease,
      border-color 160ms ease,
      background 160ms ease;
    animation: rise-in 480ms ease both;
  }

  .room-card:nth-child(2) {
    animation-delay: 55ms;
  }

  .room-card:nth-child(3) {
    animation-delay: 110ms;
  }

  .room-card:nth-child(4) {
    animation-delay: 165ms;
  }

  .room-card:hover {
    transform: translateY(-2px);
    border-color: rgba(217, 242, 107, 0.28);
    background:
      linear-gradient(155deg, rgba(217, 242, 107, 0.06), transparent 55%),
      rgba(17, 21, 17, 0.8);
  }

  .card-number {
    position: absolute;
    top: 14px;
    right: 15px;
    color: var(--faint);
    font-size: 9px;
    letter-spacing: 0.08em;
  }

  .card-icon {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    margin-bottom: 17px;
    border-radius: 9px;
    background: rgba(241, 232, 215, 0.055);
    color: var(--lime);
  }

  .card-copy {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .card-copy strong {
    color: var(--ink);
    font-family: var(--font-display);
    font-size: 22px;
    font-weight: 500;
    letter-spacing: -0.02em;
    line-height: 1.1;
  }

  .card-copy span {
    overflow: hidden;
    color: var(--faint);
    font-size: 11px;
    line-height: 1.45;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .card-foot {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: auto;
    padding-top: 18px;
  }

  .capacity-ticks {
    display: flex;
    gap: 3px;
  }

  .capacity-ticks i {
    width: 12px;
    height: 2px;
    border-radius: 2px;
    background: rgba(241, 232, 215, 0.12);
  }

  .capacity-ticks i.filled {
    background: var(--lime);
  }

  .card-count {
    color: var(--faint);
    font-size: 9px;
  }

  .card-arrow {
    margin-left: auto;
    color: var(--faint);
    transition: transform 160ms ease, color 160ms ease;
  }

  .room-card:hover .card-arrow {
    color: var(--lime);
    transform: translateX(2px);
  }

  .lobby-empty {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 8px;
    max-width: 420px;
    margin-top: 46px;
    padding: 24px;
    border: 1px dashed var(--line-strong);
    border-radius: 14px;
    color: var(--faint);
  }

  .lobby-empty p {
    margin: 0;
    color: var(--muted);
    font-size: 13px;
  }

  .lobby-empty button {
    margin-top: 4px;
    color: var(--lime);
    font-size: 12px;
    font-weight: 600;
  }

  .lobby-foot {
    display: flex;
    align-items: center;
    gap: 22px;
    margin-top: auto;
    padding-top: 38px;
    color: var(--faint);
    font-size: 9px;
    letter-spacing: 0.03em;
  }

  .lobby-foot span {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }

  .toast {
    position: absolute;
    z-index: 20;
    top: 16px;
    left: 50%;
    display: flex;
    align-items: flex-start;
    gap: 10px;
    width: min(520px, calc(100% - 40px));
    padding: 11px 11px 11px 13px;
    transform: translateX(-50%);
    border: 1px solid color-mix(in srgb, var(--coral) 28%, transparent);
    border-radius: 11px;
    background: rgba(24, 17, 15, 0.94);
    box-shadow: var(--shadow-md);
    backdrop-filter: blur(12px);
    animation: rise-in 220ms ease both;
  }

  .toast-icon {
    padding-top: 1px;
    color: var(--coral);
  }

  .toast-copy {
    display: flex;
    flex-direction: column;
    gap: 1px;
    flex: 1;
    min-width: 0;
  }

  .toast-code {
    color: var(--coral);
    font-size: 8.5px;
    font-weight: 700;
    letter-spacing: 0.09em;
    text-transform: uppercase;
  }

  .toast-message {
    color: var(--ink-soft);
    font-size: 11.5px;
    line-height: 1.4;
  }

  .toast-close {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    flex: none;
    border-radius: 6px;
    color: var(--faint);
  }

  .toast-close:hover {
    color: var(--ink);
    background: rgba(241, 232, 215, 0.07);
  }

  .boot-error {
    position: absolute;
    z-index: 15;
    top: 16px;
    left: 50%;
    display: flex;
    align-items: center;
    gap: 8px;
    max-width: calc(100% - 40px);
    padding: 8px 12px;
    transform: translateX(-50%);
    border: 1px solid var(--line-strong);
    border-radius: 8px;
    background: var(--surface-raised);
    color: var(--muted);
    font-size: 11px;
  }

  @media (max-width: 1180px) {
    .landing {
      grid-template-columns: minmax(0, 1fr) 380px;
      gap: 36px;
      padding-right: 40px;
      padding-left: 40px;
    }

    .landing h1 {
      font-size: clamp(58px, 6.4vw, 82px);
    }
  }

  @media (max-width: 1080px) {
    .shell.with-right {
      grid-template-columns: 270px minmax(0, 1fr);
    }

    .shell.with-right :global(.participants) {
      display: none;
    }

    .landing {
      grid-template-columns: minmax(0, 1fr);
      align-content: center;
      gap: 38px;
      max-width: 640px;
    }

    .landing-copy {
      max-width: 100%;
    }

    .landing h1 {
      font-size: clamp(54px, 8vw, 76px);
    }

    .principles {
      display: none;
    }

    .arc {
      display: none;
    }

    .landing-action {
      max-width: 460px;
    }
  }

  @media (max-width: 760px) {
    .shell,
    .shell.with-right {
      grid-template-columns: 1fr;
    }

    .rail {
      display: none;
    }

    .landing {
      padding: 30px 22px;
      height: auto;
      min-height: 100%;
    }

    .landing h1 {
      font-size: 52px;
    }

    .version-stamp {
      display: none;
    }
  }
</style>
