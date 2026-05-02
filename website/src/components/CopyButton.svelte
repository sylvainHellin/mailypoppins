<script lang="ts">
  interface Props {
    text: string;
  }

  let { text }: Props = $props();

  let copied = $state(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(text);
      copied = true;
      setTimeout(() => {
        copied = false;
      }, 1500);
    } catch {
      /* clipboard unavailable */
    }
  }
</script>

<button
  class="copy-btn"
  onclick={copy}
  aria-label={copied ? 'Copied' : 'Copy to clipboard'}
  title={copied ? 'Copied' : 'Copy to clipboard'}
>
  {#if copied}
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
      <polyline points="20 6 9 17 4 12" />
    </svg>
  {:else}
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
      <rect x="9" y="9" width="13" height="13" rx="2" ry="2" />
      <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
    </svg>
  {/if}
</button>

<style>
  .copy-btn {
    position: absolute;
    top: var(--space-2);
    right: var(--space-2);
    display: flex;
    align-items: center;
    justify-content: center;
    width: 32px;
    height: 32px;
    padding: 0;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--color-overlay0);
    cursor: pointer;
    transition: background 0.15s ease, color 0.15s ease;
  }

  .copy-btn:hover {
    background: var(--color-surface0);
    color: var(--color-text);
  }
</style>
