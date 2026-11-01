<script lang="ts">
  interface Props {
    name: string;
    id?: string;
    size?: number;
  }

  let { name, id = name, size = 34 }: Props = $props();

  const tones = ['#d9f26b', '#f0c66d', '#9bd7e8', '#f0b5c5', '#b7e3c0', '#e7b58e'];

  const initials = $derived.by(() => {
    const parts = name.trim().split(/\s+/).filter(Boolean);
    if (parts.length === 0) return '?';
    if (parts.length === 1) return parts[0].slice(0, 2).toUpperCase();
    return `${parts[0][0]}${parts[parts.length - 1][0]}`.toUpperCase();
  });

  const tone = $derived.by(() => {
    let hash = 0;
    for (const char of id) hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
    return tones[hash % tones.length];
  });
</script>

<div
  class="avatar"
  style:width={`${size}px`}
  style:height={`${size}px`}
  style:--tone={tone}
  style:font-size={`${Math.max(10, size * 0.3)}px`}
  title={name}
  aria-hidden="true"
>
  {initials}
</div>

<style>
  .avatar {
    display: grid;
    place-items: center;
    flex: none;
    border-radius: 30%;
    background: color-mix(in srgb, var(--tone) 14%, transparent);
    border: 1px solid color-mix(in srgb, var(--tone) 32%, transparent);
    color: var(--tone);
    font-weight: 650;
    letter-spacing: 0.04em;
    line-height: 1;
    user-select: none;
  }
</style>
