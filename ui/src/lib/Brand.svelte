<script lang="ts">
  interface Props {
    size?: number;
  }

  let { size = 38 }: Props = $props();
</script>

<div class="brand" aria-label="Concord">
  <div class="mark" style:width={`${size}px`} style:height={`${size}px`}>
    <svg viewBox="0 0 64 64" aria-hidden="true">
      <path
        d="M41 19a17 17 0 1 0 0 26"
        fill="none"
        stroke="var(--ink)"
        stroke-width="7"
        stroke-linecap="round"
      />
      <circle cx="44" cy="32" r="4.5" fill="var(--lime)" />
    </svg>
  </div>
  <span class="word">Concord</span>
</div>

<style>
  .brand {
    display: flex;
    align-items: center;
    gap: 11px;
    min-width: 0;
  }

  .mark {
    display: grid;
    place-items: center;
    flex: none;
    border-radius: 11px;
    background: #101410;
    border: 1px solid var(--line-strong);
    box-shadow: inset 0 1px 0 rgba(241, 232, 215, 0.06);
  }

  .mark svg {
    width: 88%;
    height: 88%;
  }

  .word {
    font-family: var(--font-display);
    font-size: 25px;
    font-weight: 500;
    letter-spacing: -0.02em;
    color: var(--ink);
    line-height: 1;
  }
</style>
