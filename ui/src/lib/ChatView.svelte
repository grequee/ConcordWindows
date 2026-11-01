<script lang="ts">
  import { describeError, loadChatHistory, editChatMessage, deleteChatMessage, inspectChatAttachment, isTauri, type NativeChatAttachment } from './api';
  import ChatImage from './ChatImage.svelte';
  import ChatSearch from './ChatSearch.svelte';
  import MessageText from './MessageText.svelte';
  import { onDestroy, onMount, untrack } from 'svelte';
  import { getCurrentWebview } from '@tauri-apps/api/webview';
  import ChatAttachment from './ChatAttachment.svelte';
  import { tick } from 'svelte';
  import Avatar from './Avatar.svelte';
  import CallDock from './CallDock.svelte';
  import Icon from './Icon.svelte';
  import type { ActiveRoom, CallInfo, ChatMessage } from './types';

  interface Props {
    lastRead: number | null;
    unreadCount: number;
    onposition: (room: string, bottom: boolean) => void;
    room: ActiveRoom;
    selfId: string;
    selfAuthorId: string;
    richChat: boolean;
    persistent: boolean;
    imagesEnabled: boolean;
    attachmentsEnabled: boolean;
    call: CallInfo | null;
    onsend: (text: string, file: File | NativeChatAttachment | null, clientId: string, progress: (percent: number) => void, replyTo?: number | null) => Promise<void>;
    onleave: () => Promise<void>;
    oncalljoin: () => Promise<void>;
    oncallleave: () => Promise<void>;
  }

  let { lastRead, unreadCount, onposition, room, selfId, selfAuthorId, richChat, persistent, imagesEnabled, attachmentsEnabled, call, onsend, onleave, oncalljoin, oncallleave }: Props = $props();

  let unreadBoundary = $state<number | null>(null);
  $effect(() => { if (lastRead !== null && unreadCount === 0 && atBottom) unreadBoundary = lastRead; });
  let draggingFiles = $state(false);
  let pendingFiles = $state<(File | NativeChatAttachment)[]>([]);
  $effect(() => { onposition(room.room.id, atBottom); });
  function acceptFiles(files: (File | NativeChatAttachment)[]) {
    if (sending || !(imagesEnabled || attachmentsEnabled) || !files.length) return;
    clearAttachment();
    pendingFiles = files.slice(1); setAttachment(files[0]);
  }
  function setAttachment(file: File | NativeChatAttachment) {
    attachment = file;
    if (file instanceof File && file.size <= 2 * 1024 * 1024 && ['image/png','image/jpeg','image/webp','image/gif'].includes(file.type)) attachmentPreview = URL.createObjectURL(file);
    requestId = crypto.randomUUID(); sendError = null;
  }
  async function acceptDroppedPaths(paths: string[]) {
    draggingFiles = false;
    if (sending || !(imagesEnabled || attachmentsEnabled) || !paths.length) return;
    const targetRoom = room.room.id;
    const results = await Promise.allSettled(paths.map(path => inspectChatAttachment(path)));
    if (room.room.id !== targetRoom) return;
    const files = results.flatMap(result => result.status === 'fulfilled' ? [result.value] : []);
    const rejected = results.length - files.length;
    if (files.length) acceptFiles(files);
    if (rejected) sendError = rejected === 1
      ? 'Um dos arquivos não pôde ser lido. Tente arrastá-lo novamente.'
      : `${rejected} arquivos não puderam ser lidos. Tente arrastá-los novamente.`;
  }
  function paste(event: ClipboardEvent) {
    const images = Array.from(event.clipboardData?.files ?? []).filter(file => file.type.startsWith('image/'));
    if (images.length && !sending && (imagesEnabled || attachmentsEnabled)) { event.preventDefault(); acceptFiles(images); }
  }
  let draft = $state('');
  let sending = $state(false);
  let leaving = $state(false);
  let sendError = $state<string | null>(null);
  let listEl = $state<HTMLDivElement | null>(null);
  let inputEl = $state<HTMLTextAreaElement | null>(null);
  let atBottom = $state(true);
  let attachment = $state<File | NativeChatAttachment | null>(null);
  let attachmentPreview = $state<string | null>(null);
  let uploadProgress = $state(0);
  let loadingHistory = $state(false);
  let fileInput = $state<HTMLInputElement>();
  let requestId = crypto.randomUUID();
  let replyTo = $state<ChatMessage | null>(null);
  let editingId = $state<number | null>(null);
  let editDraft = $state('');
  let confirmingDelete = $state<number | null>(null);
  let mutating = $state(false);
  let searchOpen = $state(false);

  onMount(() => {
    if (!isTauri()) return;
    let alive = true;
    let unlisten: (() => void) | undefined;
    void getCurrentWebview().onDragDropEvent(event => {
      if (!alive) return;
      const payload = event.payload;
      if (payload.type === 'enter' || payload.type === 'over') draggingFiles = true;
      else if (payload.type === 'leave') draggingFiles = false;
      else {
        draggingFiles = false;
        void acceptDroppedPaths(payload.paths);
      }
    }).then(stop => {
      if (alive) unlisten = stop;
      else void stop();
    }).catch(() => {
      if (alive) sendError = 'Não foi possível ativar o arrastar e soltar de arquivos.';
    });
    return () => {
      alive = false;
      unlisten?.();
    };
  });
  const composerReply = $derived(room.messages.find(message => message.id === replyTo?.id) ?? replyTo);
  function isOwn(message: ChatMessage): boolean { return message.author_id ? message.author_id === selfAuthorId : message.user_id === selfId; }
  async function reply(message: ChatMessage) {
    replyTo = message; requestId = crypto.randomUUID();
    await tick(); inputEl?.focus();
  }
  async function saveEdit(id: number) {
    if (mutating) return;
    mutating = true; sendError = null;
    const targetRoom = room.room.id;
    try { await editChatMessage(id, editDraft); if (room.room.id === targetRoom) editingId = null; }
    catch (cause) { if (room.room.id === targetRoom) sendError = describeError(cause); }
    finally { mutating = false; }
  }
  async function remove(id: number) {
    if (mutating) return;
    mutating = true; sendError = null;
    const targetRoom = room.room.id;
    try { await deleteChatMessage(id); if (room.room.id === targetRoom) confirmingDelete = null; }
    catch (cause) { if (room.room.id === targetRoom) sendError = describeError(cause); }
    finally { mutating = false; }
  }

  function clearAttachment() {
    if (attachmentPreview) URL.revokeObjectURL(attachmentPreview);
    attachmentPreview = null;
    attachment = null;
  }
  onDestroy(clearAttachment);

  function pickAttachment(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const files = Array.from(input.files ?? []);
    input.value = '';
    acceptFiles(files);
  }

  async function older() {
    if (loadingHistory) return;
    loadingHistory = true;
    sendError = null;
    const height = listEl?.scrollHeight ?? 0;
    const top = listEl?.scrollTop ?? 0;
    atBottom = false;
    try {
      await loadChatHistory();
      await tick();
      if (listEl) listEl.scrollTop = top + listEl.scrollHeight - height;
    } catch (error) { sendError = describeError(error); }
    finally { loadingHistory = false; }
  }

  const timeFormat = new Intl.DateTimeFormat('pt-BR', {
    hour: '2-digit',
    minute: '2-digit',
  });

  const longDate = new Intl.DateTimeFormat('pt-BR', {
    day: '2-digit',
    month: 'short',
  });

  const charsLeft = $derived(2000 - draft.length);
  const currentRoomId = $derived(room.room.id);
  const messageMarker = $derived(`${room.messages.length}:${room.messages.at(-1)?.id ?? ''}`);

  function dayKey(ms: number): string {
    const date = new Date(ms);
    return `${date.getFullYear()}-${date.getMonth()}-${date.getDate()}`;
  }

  function isToday(ms: number): boolean {
    return dayKey(ms) === dayKey(Date.now());
  }

  function dayLabel(ms: number): string {
    if (isToday(ms)) return 'Hoje';
    return longDate.format(new Date(ms)).replace('.', '');
  }

  function formatTime(ms: number): string {
    return timeFormat.format(new Date(ms));
  }

  function isGrouped(message: ChatMessage, previous: ChatMessage | undefined): boolean {
    if (!previous) return false;
    if (previous.user_id !== message.user_id) return false;
    if (dayKey(previous.sent_at_ms) !== dayKey(message.sent_at_ms)) return false;
    return message.sent_at_ms - previous.sent_at_ms < 5 * 60 * 1000;
  }

  function closeToBottom(): boolean {
    if (!listEl) return true;
    return listEl.scrollHeight - listEl.scrollTop - listEl.clientHeight < 80;
  }

  function scrollToBottom(smooth = false) {
    if (!listEl) return;
    listEl.scrollTo({
      top: listEl.scrollHeight,
      behavior: smooth ? 'smooth' : 'auto',
    });
  }

  function onScroll() {
    atBottom = closeToBottom();
  }

  $effect(() => {
    const count = messageMarker;
    const roomId = currentRoomId;
    void count;
    void roomId;

    if (!listEl) return;
    if (atBottom) {
      requestAnimationFrame(() => scrollToBottom());
    }
  });

  $effect(() => {
    const roomId = currentRoomId;
    void roomId;
    untrack(() => { unreadBoundary = lastRead; atBottom = unreadCount === 0; });
    untrack(clearAttachment);
    pendingFiles = [];
    draft = '';
    replyTo = null; editingId = null; confirmingDelete = null; searchOpen = false;
    sendError = null;
    requestId = crypto.randomUUID();
    requestAnimationFrame(() => {
      if (untrack(() => unreadCount > 0)) listEl?.querySelector('.unread-divider')?.scrollIntoView({ block: 'start' });
      else scrollToBottom();
      atBottom = closeToBottom();
    });
  });

  function resizeInput(event: Event) {
    requestId = crypto.randomUUID();
    const target = event.currentTarget as HTMLTextAreaElement;
    target.style.height = 'auto';
    target.style.height = `${Math.min(target.scrollHeight, 120)}px`;
  }

  function resetInputHeight() {
    if (inputEl) inputEl.style.height = 'auto';
  }

  async function send() {
    const text = draft.trim();
    if ((!text && !attachment) || sending) return;
    if (text.length > 2000) {
      sendError = 'A mensagem passa de 2000 caracteres.';
      return;
    }

    sending = true;
    sendError = null;
    const targetRoom = room.room.id;
    try {
      uploadProgress = 0;
      await onsend(text, attachment, requestId, (percent) => { if (room.room.id === targetRoom) uploadProgress = percent; }, replyTo?.id);
      if (room.room.id !== targetRoom) return;
      draft = '';
      replyTo = null;
      clearAttachment();
      const next = pendingFiles[0];
      pendingFiles = pendingFiles.slice(1);
      if (next) setAttachment(next);
      requestId = crypto.randomUUID();
      resetInputHeight();
      atBottom = true;
      requestAnimationFrame(() => scrollToBottom(true));
      inputEl?.focus();
    } catch (error) {
      if (room.room.id === targetRoom) sendError = describeError(error);
    } finally {
      sending = false;
    }
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key !== 'Enter' || event.shiftKey || event.isComposing) return;
    event.preventDefault();
    void send();
  }

  async function leave() {
    if (leaving) return;
    leaving = true;
    sendError = null;
    try {
      await onleave();
    } catch (error) {
      sendError = describeError(error);
      leaving = false;
    }
  }
</script>

<div class="chat">
  {#if draggingFiles}<div class="drop-overlay" aria-live="polite"><div class="drop-card"><span class="drop-icon"><Icon name="attach" size={25} /></span><strong>{imagesEnabled || attachmentsEnabled ? 'Solte para anexar' : 'Anexos indisponíveis'}</strong><span>{imagesEnabled || attachmentsEnabled ? 'O arquivo será preparado antes do envio.' : 'O servidor precisa habilitar o envio de anexos.'}</span></div></div>{/if}
  <header class="chat-head">
    <div class="room-heading">
      <span class="hash-tile"><Icon name="hash" size={18} /></span>
      <div class="heading-copy">
        <div class="title-row">
          <h1 class="display">{room.room.name}</h1>
          <span class="members-pill">
            <Icon name="users" size={12} />
            {room.room.member_count}/{room.room.capacity}
          </span>
        </div>
        <p>{room.room.description ?? 'Conversa em tempo real'}</p>
      </div>
    </div>

    <div class="head-actions">
      {#if richChat}<button class="search-button" title="Buscar no histórico" aria-label="Buscar no histórico" onclick={() => searchOpen = !searchOpen}><Icon name="search" size={18} /></button>{/if}
      <div class="avatar-stack" title="Participantes">
        {#each room.participants.slice(0, 4) as participant (participant.user_id)}
          <span class="stack-avatar">
            <Avatar name={participant.display_name} id={participant.user_id} size={26} />
          </span>
        {/each}
        {#if room.participants.length > 4}
          <span class="stack-more tabular">+{room.participants.length - 4}</span>
        {/if}
      </div>
      <button class="leave-button" onclick={leave} disabled={leaving}>
        <Icon name="logout" size={14} />
        {leaving ? 'Saindo…' : 'Sair'}
      </button>
    </div>
  </header>

  {#if searchOpen}<ChatSearch roomId={room.room.id} onreply={reply} onclose={() => searchOpen = false} />{/if}
  <div class="media-strip">
    <span class="live-dot" class:call-live={call !== null} aria-hidden="true"></span>
    <span>{call ? 'chamada ativa' : 'texto em tempo real'}</span>
    <span class="strip-sep"></span>
    {#if call}<span class="mic-status" class:mic-open={!call.muted}>{call.muted ? 'Microfone fechado' : 'Microfone aberto'}</span>{/if}
  </div>

  <CallDock {call} {room} {selfId} onjoin={oncalljoin} onleave={oncallleave} />

  <div
    class="message-list"
    bind:this={listEl}
    onscroll={onScroll}
    aria-live="polite"
    aria-label="Mensagens da sala"
  >
    {#if room.has_more}
      <button class="load-history" onclick={older} disabled={loadingHistory}>
        {loadingHistory ? 'Carregando histórico…' : 'Carregar mensagens anteriores'}
      </button>
    {/if}
    {#if room.messages.length === 0}
      <div class="empty-chat">
        <div class="wave" aria-hidden="true">
          <i></i><i></i><i></i><i></i><i></i><i></i><i></i>
        </div>
        <p class="display">A sala está em silêncio.</p>
        <span>Mande a primeira mensagem e quebre o gelo.</span>
      </div>
    {:else}
      {#each room.messages as message, index (message.id)}
        {@const previous = index > 0 ? room.messages[index - 1] : undefined}
        {@const newDay = !previous || dayKey(previous.sent_at_ms) !== dayKey(message.sent_at_ms)}
        {@const grouped = !newDay && isGrouped(message, previous)}
        {#if newDay}
          <div class="day-divider">
            <span>{dayLabel(message.sent_at_ms)}</span>
          </div>
        {/if}
        {#if unreadBoundary !== null && message.id > unreadBoundary && (!previous || previous.id <= unreadBoundary)}<div class="unread-divider">Mensagens desde sua última leitura</div>{/if}
        <div
          class="message"
          class:grouped
          class:own={isOwn(message)}
          data-message-id={message.id}
        >
          {#if grouped}
            <span class="avatar-gap" aria-hidden="true"></span>
          {:else}
            <Avatar name={message.display_name} id={message.user_id} size={34} />
          {/if}
          <div class="message-body">
            {#if !grouped}
              <div class="message-meta">
                <strong>{message.display_name}</strong>
                {#if isOwn(message)}<span class="you-tag">você</span>{/if}
                <time datetime={new Date(message.sent_at_ms).toISOString()}>
                  {formatTime(message.sent_at_ms)}
                </time>
              </div>
            {/if}
            {#if message.reply_to}
              {@const original = room.messages.find(item => item.id === message.reply_to)}
              <div class="quoted-message"><Icon name="reply" size={12} /><div><strong>{original?.display_name ?? message.reply?.display_name ?? 'Mensagem removida'}</strong><span>{original?.text || original?.attachment?.name || message.reply?.text || 'Mensagem indisponível'}</span></div></div>
            {/if}
            {#if editingId === message.id}
              <div class="inline-editor"><textarea bind:value={editDraft} maxlength="2000" aria-label="Editar mensagem" disabled={mutating}></textarea><div><button disabled={mutating} onclick={() => editingId = null}>Cancelar</button><button disabled={mutating || (!editDraft.trim() && !message.attachment && !message.image)} onclick={() => saveEdit(message.id)}>{mutating ? 'Salvando…' : 'Salvar edição'}</button></div></div>
            {:else if message.text}<MessageText text={message.text} />{/if}
            {#if message.edited_at_ms}<span class="edited-mark">editada</span>{/if}
            {#if message.image}
              <ChatImage id={message.id} width={message.image.width} height={message.image.height} />
            {/if}
            {#if message.attachment}
              <ChatAttachment id={message.id} name={message.attachment.name} bytes={message.attachment.bytes} />
            {/if}
          </div>
          {#if richChat}<div class="message-actions">
            <button title="Responder" aria-label="Responder" onclick={() => reply(message)}><Icon name="reply" size={14} /></button>
            {#if isOwn(message)}
              <button title="Editar mensagem" aria-label="Editar mensagem" onclick={() => { editingId = message.id; editDraft = message.text; confirmingDelete = null; }}><Icon name="edit" size={14} /></button>
              <button title="Apagar mensagem" aria-label="Apagar mensagem" onclick={() => confirmingDelete = confirmingDelete === message.id ? null : message.id}><Icon name="trash" size={14} /></button>
            {/if}
          </div>{/if}
          {#if confirmingDelete === message.id}<div class="delete-confirm" role="group" aria-label="Confirmar exclusão"><span>Apagar esta mensagem e seu anexo?</span><button disabled={mutating} onclick={() => confirmingDelete = null}>Cancelar</button><button disabled={mutating} onclick={() => remove(message.id)}>Apagar</button></div>{/if}
        </div>
      {/each}
    {/if}
  </div>

  {#if !atBottom}<button class="jump-latest" onclick={() => { atBottom = true; scrollToBottom(true); }}>{unreadCount ? `${unreadCount} novas mensagens` : 'Ir para o fim'} ↓</button>{/if}
  <div class="composer-wrap">
    {#if composerReply}<div class="composer-reply"><Icon name="reply" size={15} /><div><strong>Respondendo a {composerReply.display_name}</strong><span>{composerReply.text || composerReply.attachment?.name || 'Imagem'}</span></div><button title="Cancelar resposta" aria-label="Cancelar resposta" onclick={() => { replyTo = null; requestId = crypto.randomUUID(); }}><Icon name="close" size={14} /></button></div>{/if}
    {#if attachment}
      <div class="attachment-preview">
        {#if attachmentPreview}<img src={attachmentPreview} alt="Prévia do anexo" onerror={() => { if (attachmentPreview) URL.revokeObjectURL(attachmentPreview); attachmentPreview = null; }} />{:else}<Icon name="file" size={28} />{/if}
        <span>{attachment.name} · {(attachment.size / 1024 / 1024).toLocaleString('pt-BR', { maximumFractionDigits: 2 })} MiB{sending ? ` · ${uploadProgress}% enviado` : ''}</span>
        <button disabled={sending} aria-label="Remover anexo" onclick={() => { pendingFiles = []; clearAttachment(); requestId = crypto.randomUUID(); }}><Icon name="close" size={15} /></button>
      </div>
    {/if}
    {#if pendingFiles.length}<p class="queue-note">Mais {pendingFiles.length} arquivo(s) na fila. Cada envio anexa o próximo arquivo.</p>{/if}
    {#if sendError}
      <div class="composer-error" role="status">
        <Icon name="warning" size={14} />
        <span>{sendError}</span>
        <button onclick={() => (sendError = null)} aria-label="Fechar erro">
          <Icon name="close" size={13} />
        </button>
      </div>
    {/if}

    <div class="composer">
      <textarea
        bind:this={inputEl}
        bind:value={draft}
        oninput={resizeInput}
        onkeydown={onKeydown}
        onpaste={paste}
        rows="1"
        maxlength="2000"
        placeholder="Mensagem para #{room.room.name}"
        aria-label="Escrever mensagem"
        disabled={sending}
      ></textarea>
      <div class="composer-actions">
        {#if imagesEnabled || attachmentsEnabled}
          <input class="image-file" type="file" multiple bind:this={fileInput} onchange={pickAttachment} />
          <span class="attach-label">Arraste para anexar</span>
          <button class="attach-button" onclick={() => fileInput?.click()} disabled={sending}
            aria-label="Adicionar anexo" title="Adicionar arquivo">
            <Icon name="attach" size={18} />
          </button>
        {/if}
        {#if draft.length > 1800}
          <span class="char-count tabular" class:over={charsLeft < 0}>{charsLeft}</span>
        {/if}
        <button
          class="send-button"
          onclick={() => send()}
          disabled={sending || (!attachment && draft.trim().length === 0)}
          aria-label="Enviar mensagem"
          title="Enviar (Enter)"
        >
          {#if sending}
            <span class="sending-dot" aria-hidden="true"></span>
          {:else}
            <Icon name="send" size={16} />
          {/if}
        </button>
      </div>
    </div>
    <div class="composer-hint">
      <span>{#if sending && attachment}Enviando anexo… {uploadProgress}%{:else}<kbd>Enter</kbd> envia · <kbd>Shift</kbd> + <kbd>Enter</kbd> quebra linha{/if}</span>
      <span class="protocol-note"><Icon name="lock" size={11} /> apenas o grupo</span>
    </div>
    <p class="retention-note">{persistent ? 'Histórico salvo. Ao atingir o limite, as mensagens mais antigas e seus anexos são removidos automaticamente.' : 'Histórico temporário: as mensagens podem desaparecer ao reiniciar o servidor.'}</p>
  </div>
</div>

<style>
  .chat { position:relative; }
  .drop-overlay { position:absolute; inset:10px; z-index:40; display:grid; place-items:center; pointer-events:none; border:2px dashed var(--lime); border-radius:16px; background:rgba(12, 15, 13, .82); backdrop-filter:blur(5px); }
  .drop-card { display:grid; justify-items:center; gap:10px; padding:28px 36px; border:1px solid var(--line-strong); border-radius:15px; background:var(--surface-raised); box-shadow:0 15px 50px #0008; text-align:center; }
  .drop-icon { display:grid; place-items:center; width:48px; height:48px; border-radius:14px; background:var(--lime-soft); color:var(--lime); }
  .drop-card strong { color:var(--lime); font-size:20px; }
  .drop-card > span:last-child { color:var(--muted); font-size:12px; }
  .unread-divider { margin:15px 0; padding:8px; text-align:center; border-top:1px solid var(--lime); color:var(--lime); font-size:11px; }
  .jump-latest { align-self:center; border:1px solid var(--line-strong); border-radius:20px; padding:8px 14px; color:var(--lime); background:var(--surface); font-size:12px; }
  .mic-status { color:var(--muted); margin-left:auto; font-size:11px; } .mic-status.mic-open { color:var(--lime); }
  .queue-note { color:var(--muted); font-size:11px; }

  .search-button { display: grid; place-items: center; color: var(--muted); width: 30px; height: 30px; }
  .search-button:hover { color: var(--lime); }
  .message { position: relative; }
  .message-body { flex: 1; }
  .message-actions { display: flex; gap: 2px; align-self: flex-start; opacity: 0; transition: opacity 120ms; }
  .message:hover .message-actions, .message:focus-within .message-actions { opacity: 1; }
  .message-actions button { padding: 5px; color: var(--faint); border-radius: 5px; }
  .message-actions button:hover { background: var(--surface-raised); color: var(--lime); }
  .quoted-message, .composer-reply { display: flex; align-items: center; gap: 9px; border-left: 2px solid var(--lime); padding: 7px 10px; margin-bottom: 6px; color: var(--muted); background: var(--lime-soft); border-radius: 0 7px 7px 0; }
  .quoted-message div, .composer-reply div { display: grid; gap: 3px; min-width: 0; flex: 1; }
  .quoted-message strong, .composer-reply strong { color: var(--ink-soft); font-size: 10px; font-weight: 600; }
  .quoted-message span, .composer-reply span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 11px; }
  .composer-reply button { padding: 5px; color: var(--muted); }
  .edited-mark { margin-left: 5px; color: var(--faint); font-size: 8px; }
  .inline-editor textarea { width: 100%; min-height: 55px; padding: 8px; border: 1px solid var(--line-strong); border-radius: 7px; background: var(--bg); }
  .inline-editor > div { display: flex; justify-content: flex-end; gap: 12px; }
  .inline-editor button { padding: 5px; color: var(--lime); font-size: 11px; }
  .delete-confirm { display: flex; align-items: center; gap: 10px; position: absolute; right: 5px; top: 32px; z-index: 3; padding: 10px; border: 1px solid var(--line-strong); border-radius: 8px; background: var(--surface-raised); box-shadow: 0 8px 25px #0006; font-size: 10px; }
  .delete-confirm button { color: var(--coral); padding: 5px; }

  .load-history { display: block; margin: 0 auto 20px; padding: 9px 14px; border: 1px solid var(--line); border-radius: 8px; color: var(--lime); font-size: 12px; }
  .image-file { display: none; }
  .attach-button { display: grid; place-items: center; width: 32px; height: 34px; color: var(--muted); border-radius: 7px; }
  .attach-button:hover { color: var(--lime); background: var(--lime-soft); }
  .attach-label { color:var(--faint); font-size:10px; white-space:nowrap; }
  .attachment-preview { display: flex; align-items: center; gap: 12px; padding: 10px; margin-bottom: 9px; border: 1px solid var(--line-strong); border-radius: 10px; background: var(--surface); }
  .attachment-preview img { width: 64px; height: 52px; object-fit: contain; border-radius: 5px; }
  .attachment-preview span { flex: 1; color: var(--ink-soft); font-size: 12px; }
  .attachment-preview button { color: var(--muted); padding: 8px; }
  .retention-note { margin: 7px 0 0; color: var(--faint); font-size: 10px; line-height: 1.4; }
  .chat {
    position: relative;
    z-index: 1;
    display: flex;
    flex-direction: column;
    height: 100dvh;
    min-height: 0;
  }

  .chat-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 24px;
    flex: none;
    min-height: 82px;
    padding: 17px clamp(22px, 3.2vw, 42px);
    border-bottom: 1px solid var(--line);
    background: linear-gradient(180deg, rgba(21, 26, 22, 0.6), rgba(12, 15, 13, 0.14));
    backdrop-filter: blur(8px);
  }

  .room-heading {
    display: flex;
    align-items: flex-start;
    gap: 13px;
    min-width: 0;
  }

  .hash-tile {
    display: grid;
    place-items: center;
    width: 38px;
    height: 38px;
    flex: none;
    border-radius: 10px;
    border: 1px solid rgba(217, 242, 107, 0.17);
    background: var(--lime-soft);
    color: var(--lime);
  }

  .heading-copy {
    min-width: 0;
  }

  .title-row {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
  }

  .title-row h1 {
    overflow: hidden;
    margin: 0;
    font-size: 26px;
    font-weight: 500;
    line-height: 1.08;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .members-pill {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 21px;
    padding: 0 8px;
    flex: none;
    border: 1px solid var(--line);
    border-radius: 999px;
    color: var(--faint);
    font-size: 9.5px;
    font-variant-numeric: tabular-nums;
  }

  .heading-copy p {
    overflow: hidden;
    margin: 3px 0 0;
    color: var(--faint);
    font-size: 11px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .head-actions {
    display: flex;
    align-items: center;
    gap: 18px;
    flex: none;
  }

  .avatar-stack {
    display: flex;
    align-items: center;
    padding-left: 8px;
  }

  .stack-avatar {
    margin-left: -8px;
    border-radius: 9px;
    box-shadow: 0 0 0 2px var(--bg);
  }

  .stack-more {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    margin-left: -7px;
    border-radius: 9px;
    border: 1px solid var(--line-strong);
    background: var(--surface-raised);
    color: var(--faint);
    font-size: 9px;
    box-shadow: 0 0 0 2px var(--bg);
  }

  .leave-button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 30px;
    padding: 0 11px;
    border: 1px solid var(--line);
    border-radius: 8px;
    color: var(--faint);
    font-size: 10.5px;
    font-weight: 600;
    transition: color 130ms ease, border-color 130ms ease, background 130ms ease;
  }

  .leave-button:hover:not(:disabled) {
    color: var(--coral);
    border-color: color-mix(in srgb, var(--coral) 24%, transparent);
    background: color-mix(in srgb, var(--coral) 7%, transparent);
  }

  .leave-button:disabled {
    opacity: 0.5;
  }

  .media-strip {
    display: flex;
    align-items: center;
    gap: 9px;
    flex: none;
    padding: 8px clamp(22px, 3.2vw, 42px);
    border-bottom: 1px solid var(--line);
    background: rgba(8, 10, 9, 0.18);
    color: var(--faint);
    font-size: 9px;
    letter-spacing: 0.02em;
  }

  .live-dot {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--lime);
    box-shadow: 0 0 8px rgba(217, 242, 107, 0.55);
  }

  .live-dot.call-live {
    animation: pulse-soft 1.8s ease-in-out infinite;
  }

  .strip-sep {
    width: 1px;
    height: 11px;
    margin: 0 2px;
    background: var(--line);
  }

  :global(.muted-note) {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: color-mix(in srgb, var(--faint) 78%, transparent);
  }

  .message-list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 18px clamp(22px, 3.2vw, 42px) 26px;
    scroll-behavior: auto;
  }

  .day-divider {
    display: flex;
    align-items: center;
    gap: 14px;
    margin: 18px 0 22px;
    color: var(--faint);
    font-size: 8.5px;
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
  }

  .day-divider::before,
  .day-divider::after {
    content: '';
    flex: 1;
    height: 1px;
    background: var(--line);
  }

  .day-divider span {
    padding: 0 4px;
  }

  .message {
    display: flex;
    gap: 11px;
    padding: 5px 8px 5px 6px;
    margin: 0 -6px;
    border-radius: 9px;
    animation: fade-in 180ms ease both;
  }

  .message:hover {
    background: rgba(241, 232, 215, 0.025);
  }

  .message.grouped {
    padding-top: 1px;
    padding-bottom: 1px;
  }

  .avatar-gap {
    width: 34px;
    flex: none;
  }

  .message-body {
    min-width: 0;
    padding-top: 1px;
  }

  .message-meta {
    display: flex;
    align-items: baseline;
    gap: 7px;
    margin-bottom: 2px;
    line-height: 1.3;
  }

  .message-meta strong {
    color: var(--ink-soft);
    font-size: 12.5px;
    font-weight: 650;
  }

  .own .message-meta strong {
    color: color-mix(in srgb, var(--amber) 80%, var(--ink));
  }

  .you-tag {
    padding: 0 4px;
    border-radius: 3px;
    background: rgba(241, 232, 215, 0.07);
    color: var(--faint);
    font-size: 8px;
    font-weight: 600;
    letter-spacing: 0.04em;
  }

  .message-meta time {
    color: var(--faint);
    font-size: 8.5px;
    font-variant-numeric: tabular-nums;
    opacity: 0;
    transition: opacity 120ms ease;
  }

  .message:hover .message-meta time,
  .message.grouped .message-meta time {
    opacity: 1;
  }



  .empty-chat {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    min-height: 100%;
    padding: 40px 20px;
    text-align: center;
    animation: fade-in 400ms ease both;
  }

  .wave {
    display: flex;
    align-items: center;
    gap: 4px;
    height: 34px;
    margin-bottom: 24px;
    opacity: 0.55;
  }

  .wave i {
    width: 2px;
    border-radius: 2px;
    background: var(--lime);
    animation: wave 2.4s ease-in-out infinite;
  }

  .wave i:nth-child(1) {
    height: 9px;
    animation-delay: -0.2s;
  }

  .wave i:nth-child(2) {
    height: 18px;
    animation-delay: -0.5s;
  }

  .wave i:nth-child(3) {
    height: 27px;
    animation-delay: -0.9s;
  }

  .wave i:nth-child(4) {
    height: 12px;
    animation-delay: -0.1s;
  }

  .wave i:nth-child(5) {
    height: 23px;
    animation-delay: -0.7s;
  }

  .wave i:nth-child(6) {
    height: 16px;
    animation-delay: -0.4s;
  }

  .wave i:nth-child(7) {
    height: 7px;
    animation-delay: -0.6s;
  }

  @keyframes wave {
    0%,
    100% {
      transform: scaleY(0.6);
      opacity: 0.55;
    }
    50% {
      transform: scaleY(1.12);
      opacity: 1;
    }
  }

  .empty-chat p {
    margin: 0 0 6px;
    color: var(--ink-soft);
    font-size: 28px;
    font-weight: 450;
  }

  .empty-chat span {
    color: var(--faint);
    font-size: 11.5px;
  }

  .composer-wrap {
    flex: none;
    padding: 0 clamp(22px, 3.2vw, 42px) 17px;
  }

  .composer {
    display: flex;
    align-items: flex-end;
    gap: 8px;
    padding: 9px 9px 9px 14px;
    border: 1px solid var(--line-strong);
    border-radius: 13px;
    background:
      linear-gradient(180deg, rgba(241, 232, 215, 0.025), transparent),
      rgba(16, 20, 17, 0.92);
    box-shadow: 0 8px 30px rgba(0, 0, 0, 0.2);
    transition: border-color 140ms ease, box-shadow 140ms ease;
  }

  .composer:focus-within {
    border-color: color-mix(in srgb, var(--lime) 36%, var(--line-strong));
    box-shadow:
      0 0 0 3px rgba(217, 242, 107, 0.055),
      0 8px 30px rgba(0, 0, 0, 0.2);
  }

  textarea {
    flex: 1;
    min-height: 26px;
    max-height: 120px;
    padding: 4px 0;
    border: 0;
    background: transparent;
    color: var(--ink);
    font-size: 13px;
    line-height: 1.5;
    resize: none;
    outline: none;
  }

  textarea::placeholder {
    color: var(--faint);
  }

  .composer-actions {
    display: flex;
    align-items: center;
    gap: 7px;
    flex: none;
    padding-bottom: 1px;
  }

  .char-count {
    color: var(--faint);
    font-size: 9px;
  }

  .char-count.over {
    color: var(--coral);
  }

  .send-button {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    border-radius: 8px;
    background: var(--lime);
    color: #11150f;
    transition: background 130ms ease, transform 130ms ease, opacity 130ms ease;
  }

  .send-button:hover:not(:disabled) {
    background: var(--lime-strong);
    transform: translateY(-1px);
  }

  .send-button:disabled {
    background: rgba(241, 232, 215, 0.08);
    color: var(--faint);
    opacity: 0.7;
  }

  .sending-dot {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: #11150f;
    animation: pulse-soft 700ms ease-in-out infinite;
  }

  .composer-hint {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-top: 7px;
    padding: 0 4px;
    color: var(--faint);
    font-size: 8.5px;
  }

  kbd {
    font-family: inherit;
    font-size: 8.5px;
    color: var(--muted);
  }

  .protocol-note {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }

  .composer-error {
    display: flex;
    align-items: center;
    gap: 7px;
    margin-bottom: 7px;
    padding: 7px 9px;
    border: 1px solid color-mix(in srgb, var(--coral) 24%, transparent);
    border-radius: 7px;
    background: color-mix(in srgb, var(--coral) 7%, transparent);
    color: var(--coral);
    font-size: 10.5px;
  }

  .composer-error span {
    flex: 1;
  }

  .composer-error button {
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    border-radius: 5px;
    color: inherit;
  }

  .composer-error button:hover {
    background: rgba(255, 159, 139, 0.1);
  }

  @media (max-width: 760px) {
    .chat-head {
      padding: 14px 18px;
    }

    .head-actions {
      gap: 8px;
    }

    .avatar-stack {
      display: none;
    }

    .media-strip {
      padding: 7px 18px;
      overflow: hidden;
      white-space: nowrap;
    }

    .message-list {
      padding: 16px 18px 20px;
    }

    .composer-wrap {
      padding: 0 18px 12px;
    }

    .composer-hint {
      display: none;
    }
  }
</style>
