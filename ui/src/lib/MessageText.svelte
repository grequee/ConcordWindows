<script lang="ts">
  import { linkify } from './linkify';
  import { openExternal, describeError } from './api';
  let { text }: { text: string } = $props();
  let error = $state<string | null>(null);
  async function open(event: MouseEvent, url: string) {
    event.preventDefault();
    try { await openExternal(url); } catch (cause) { error = describeError(cause); }
  }
</script>
<p>{#each linkify(text) as part}{#if part.url}<a href={part.url} rel="noopener noreferrer" target="_blank" onclick={event => open(event, part.url!)}>{part.text}</a>{:else}{part.text}{/if}{/each}</p>
{#if error}<span class="link-error" role="status">{error}</span>{/if}
<style>
  p { margin: 0; color: var(--ink-soft); font-size: 13px; line-height: 1.55; overflow-wrap: anywhere; white-space: pre-wrap; }
  a { color: var(--lime); text-decoration: underline; text-underline-offset: 3px; }
  a:hover { color: var(--ink); }
  a:focus-visible { outline: 2px solid var(--lime); outline-offset: 2px; }
  .link-error { color: var(--coral); font-size: 10px; }
</style>
