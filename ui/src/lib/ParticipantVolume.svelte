<script lang="ts">
  import { describeError, setParticipantVolume } from './api';
  let { userId, name, volume }: { userId: string; name: string; volume: number } = $props();
  let pending = $state<number | null>(null);
  let error = $state<string | null>(null);
  let sending = $state(false);
  const value = $derived(pending ?? volume);
  $effect(() => { if (!sending && pending === volume) pending = null; });

  async function apply() {
    if (sending) return;
    sending = true;
    let sent: number | null = null;
    try {
      while (pending !== null && pending !== sent) {
        const target: number = pending;
        await setParticipantVolume(userId, target);
        sent = target;
      }
    } catch (cause) { error = describeError(cause); pending = null; }
    finally { sending = false; }
  }
  function change(event: Event) {
    pending = Number((event.currentTarget as HTMLInputElement).value);
    error = null;
    void apply();
  }
</script>

<div class="participant-volume">
  <span title="Este volume muda apenas o que você ouve">Para você</span>
  <input type="range" min="0" max="200" step="1" value={value} oninput={change}
    aria-label={`Volume de ${name}`} aria-valuetext={`${value}%`} />
  <output>{value}%</output>
</div>
{#if error}<p class="volume-error" role="alert">{error}</p>{/if}

<style>
  .participant-volume { display: flex; align-items: center; gap: 8px; margin: 0 8px 8px 48px; }
  span { color: var(--faint); font-size: 8px; white-space: nowrap; }
  input { width: 0; flex: 1; min-width: 30px; accent-color: var(--lime); height: 14px; cursor: pointer; }
  output { min-width: 27px; text-align: right; color: var(--muted); font-size: 9px; font-variant-numeric: tabular-nums; }
  .volume-error { margin: 4px 8px 10px; color: var(--coral); font-size: 10px; }
</style>
