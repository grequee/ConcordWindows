<script lang="ts">
  import Icon from './Icon.svelte';
  import type { RoomSummary } from './types';

  interface Props {
    rooms: RoomSummary[];
    unreadCounts?: Record<string, number>;
    activeRoomId: string | null;
    onjoin: (roomId: string) => Promise<void>;
    onrefresh: () => Promise<void>;
  }

  let { unreadCounts = {}, rooms, activeRoomId, onjoin, onrefresh }: Props = $props();
  let pendingId = $state<string | null>(null);
  let refreshing = $state(false);

  async function join(room: RoomSummary) {
    if (room.id === activeRoomId || pendingId) return;
    pendingId = room.id;
    try {
      await onjoin(room.id);
    } finally {
      window.setTimeout(() => {
        pendingId = null;
      }, 350);
    }
  }

  async function refresh() {
    if (refreshing) return;
    refreshing = true;
    try {
      await onrefresh();
    } finally {
      window.setTimeout(() => {
        refreshing = false;
      }, 500);
    }
  }
</script>

<section class="room-section" aria-labelledby="rooms-title">
  <header class="section-head">
    <h2 class="mono-label" id="rooms-title">Salas</h2>
    <div class="head-actions">
      <span class="room-count tabular">{rooms.length}</span>
      <button class="icon-button" onclick={refresh} title="Atualizar salas" aria-label="Atualizar salas">
        <Icon name="refresh" size={14} class={refreshing ? 'spinning' : ''} />
      </button>
    </div>
  </header>

  {#if rooms.length === 0}
    <div class="no-rooms">
      <p>O servidor não tem salas configuradas.</p>
      <button onclick={refresh}>Tentar de novo</button>
    </div>
  {:else}
    <div class="room-list">
      {#each rooms as room (room.id)}
        <button
          class="room"
          class:active={room.id === activeRoomId}
          class:pending={room.id === pendingId}
          onclick={() => join(room)}
          aria-current={room.id === activeRoomId ? 'page' : undefined}
        >
          <span class="hash" aria-hidden="true"><Icon name="hash" size={16} /></span>
          <span class="room-copy">
            <span class="room-top">
              <span class="room-name">{room.name}</span>
              {#if unreadCounts[room.id]}<span class="unread-badge" aria-label={`${unreadCounts[room.id]} mensagens não lidas`}>{unreadCounts[room.id]}</span>{/if}
              <span class="member-count tabular">{room.member_count}/{room.capacity}</span>
            </span>
            {#if room.description}
              <span class="room-description">{room.description}</span>
            {/if}
            <span class="capacity-ticks" aria-hidden="true">
              {#each Array.from({ length: Math.min(room.capacity, 8) }) as _, index}
                <i class:filled={index < room.member_count}></i>
              {/each}
            </span>
          </span>
          {#if room.id !== activeRoomId}
            <span class="enter-hint" aria-hidden="true"><Icon name="arrow-right" size={14} /></span>
          {:else}
            <span class="active-dot" aria-hidden="true"></span>
          {/if}
        </button>
      {/each}
    </div>
  {/if}
</section>

<style>
  .unread-badge { border-radius:99px; padding:2px 6px; color:var(--bg); background:var(--lime); font-size:10px; }
  .room-section {
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-height: 0;
  }

  .section-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 0 4px 0 10px;
  }

  .section-head h2 {
    margin: 0;
    color: var(--faint);
  }

  .head-actions {
    display: flex;
    align-items: center;
    gap: 3px;
  }

  .room-count {
    min-width: 18px;
    padding: 1px 5px;
    border-radius: 999px;
    background: rgba(241, 232, 215, 0.06);
    color: var(--faint);
    font-size: 10px;
    text-align: center;
  }

  .icon-button {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: 7px;
    color: var(--faint);
    transition: background 130ms ease, color 130ms ease;
  }

  .icon-button:hover {
    color: var(--ink);
    background: rgba(241, 232, 215, 0.06);
  }

  .icon-button :global(.spinning) {
    animation: spin 650ms linear infinite;
  }

  .room-list {
    display: flex;
    flex-direction: column;
    gap: 3px;
    overflow-y: auto;
    padding-right: 2px;
  }

  .room {
    position: relative;
    display: flex;
    align-items: flex-start;
    gap: 9px;
    width: 100%;
    padding: 10px 10px 10px 9px;
    border-radius: 10px;
    border: 1px solid transparent;
    color: var(--muted);
    text-align: left;
    transition:
      background 140ms ease,
      border-color 140ms ease,
      color 140ms ease;
  }

  .room:hover {
    background: rgba(241, 232, 215, 0.045);
    color: var(--ink-soft);
  }

  .room.active {
    background:
      linear-gradient(90deg, rgba(217, 242, 107, 0.1), transparent 85%),
      rgba(241, 232, 215, 0.035);
    border-color: rgba(217, 242, 107, 0.16);
    color: var(--ink);
  }

  .room.active::before {
    content: '';
    position: absolute;
    left: -1px;
    top: 10px;
    bottom: 10px;
    width: 2px;
    border-radius: 2px;
    background: var(--lime);
    box-shadow: 0 0 12px rgba(217, 242, 107, 0.5);
  }

  .room.pending {
    opacity: 0.65;
  }

  .hash {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    flex: none;
    margin-top: 1px;
    border-radius: 7px;
    background: rgba(241, 232, 215, 0.055);
    color: var(--faint);
  }

  .room.active .hash {
    background: var(--lime-soft);
    color: var(--lime);
  }

  .room-copy {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1;
    min-width: 0;
  }

  .room-top {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 6px;
  }

  .room-name {
    overflow: hidden;
    color: inherit;
    font-size: 12.5px;
    font-weight: 600;
    line-height: 1.35;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .room.active .room-name {
    color: var(--ink);
  }

  .member-count {
    flex: none;
    color: var(--faint);
    font-size: 9px;
    letter-spacing: 0.03em;
  }

  .room.active .member-count {
    color: color-mix(in srgb, var(--lime) 70%, var(--faint));
  }

  .room-description {
    overflow: hidden;
    color: var(--faint);
    font-size: 10.5px;
    line-height: 1.35;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .capacity-ticks {
    display: flex;
    gap: 3px;
    margin-top: 4px;
  }

  .capacity-ticks i {
    width: 9px;
    height: 2px;
    border-radius: 2px;
    background: rgba(241, 232, 215, 0.11);
    transition: background 180ms ease;
  }

  .capacity-ticks i.filled {
    background: color-mix(in srgb, var(--lime) 58%, transparent);
  }

  .room.active .capacity-ticks i.filled {
    background: var(--lime);
  }

  .enter-hint {
    align-self: center;
    color: var(--faint);
    opacity: 0;
    transform: translateX(-3px);
    transition: opacity 140ms ease, transform 140ms ease;
  }

  .room:hover .enter-hint {
    opacity: 1;
    transform: translateX(0);
  }

  .active-dot {
    align-self: center;
    width: 5px;
    height: 5px;
    flex: none;
    border-radius: 50%;
    background: var(--lime);
    box-shadow: 0 0 8px rgba(217, 242, 107, 0.7);
  }

  .no-rooms {
    padding: 14px 12px;
    border: 1px dashed var(--line);
    border-radius: 10px;
    color: var(--faint);
    font-size: 11px;
    line-height: 1.5;
  }

  .no-rooms p {
    margin: 0 0 8px;
  }

  .no-rooms button {
    color: var(--lime);
    font-size: 11px;
    font-weight: 600;
  }
</style>
