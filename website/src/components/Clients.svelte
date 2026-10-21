<script lang="ts">
  // The three clients of the one daemon, as tabs. Every panel is in the
  // server-rendered HTML; the tabs only toggle which one shows.
  interface Props {
    hasDesktopShot: boolean;
  }

  let { hasDesktopShot }: Props = $props();

  const tabs = [
    { id: 'tui', label: 'Terminal', caption: 'Mailboxes, list and reader in three panes, driven by Vim keys, over SSH as well.' },
    { id: 'cli', label: 'Command line', caption: 'Every action is a command, for your scripts and for your agent.' },
    ...(hasDesktopShot
      ? [{ id: 'desktop', label: 'Mac app', caption: 'A native window with the same keys and a real Neovim for each draft.' }]
      : []),
  ];

  let active = $state('tui');

  function onKey(event: KeyboardEvent, index: number) {
    const step = event.key === 'ArrowRight' ? 1 : event.key === 'ArrowLeft' ? -1 : 0;
    if (!step) return;
    event.preventDefault();
    const next = tabs[(index + step + tabs.length) % tabs.length];
    active = next.id;
    document.getElementById(`tab-${next.id}`)?.focus();
  }
</script>

<div class="clients">
  <div class="tablist" role="tablist" aria-label="Clients">
    {#each tabs as tab, i (tab.id)}
      <button
        id={`tab-${tab.id}`}
        role="tab"
        class="tab"
        aria-selected={active === tab.id}
        aria-controls={`panel-${tab.id}`}
        tabindex={active === tab.id ? 0 : -1}
        onclick={() => (active = tab.id)}
        onkeydown={(e) => onKey(e, i)}
      >
        {tab.label}
      </button>
    {/each}
  </div>

  {#each tabs as tab (tab.id)}
    <div
      id={`panel-${tab.id}`}
      role="tabpanel"
      aria-labelledby={`tab-${tab.id}`}
      class="panel"
      hidden={active !== tab.id}
    >
      <p class="caption">{tab.caption}</p>
      <div class="frame">
        {#if tab.id === 'tui'}
          <img
            src="/tui-screenshot.png"
            alt="The terminal client: mailboxes, message list and reading pane"
            width="2496"
            height="1356"
            loading="lazy"
          />
        {:else if tab.id === 'cli'}
          <pre class="term" aria-label="A command-line session"><span class="p">$</span> mp search --local 'from:ada invoice'
<span class="dim">"from:ada invoice" in work (1 hit):</span>
<span class="dim">────────────────────────────────────────────────────────</span>
[<span class="b">unread</span>] mp://work/inbox/8c41f0e2@example.com → you@example.com
      <span class="dim">Tue, 13 Oct 2026 09:12  Invoice 2026-114, site survey</span>

<span class="p">$</span> mp reply mp://work/inbox/8c41f0e2@example.com
mp://work/drafts/3f9c27a1d4e8b605

<span class="p">$</span> mp edit mp://work/drafts/3f9c27a1d4e8b605  <span class="c"># opens in Neovim</span>
<span class="p">$</span> mp list
<span class="dim">Drafts:</span>
<span class="dim">────────────────────────────────────────────────────────</span>
[<span class="y">draft</span>] mp://work/drafts/3f9c27a1d4e8b605 → ada@example.com
      <span class="dim">Re: Invoice 2026-114, site survey</span>

<span class="p">$</span> mp mark-approved mp://work/drafts/3f9c27a1d4e8b605
<span class="p">$</span> mp send mp://work/drafts/3f9c27a1d4e8b605</pre>
        {:else}
          <img
            src="/desktop-screenshot.png"
            alt="The Mac app: sidebar, message list and reader"
            loading="lazy"
          />
        {/if}
      </div>
    </div>
  {/each}
</div>

<style>
  .tablist {
    display: flex;
    justify-content: center;
    gap: var(--space-1);
    margin-bottom: var(--space-4);
  }

  .tab {
    font: inherit;
    font-size: 0.9375rem;
    font-weight: 500;
    padding: var(--space-2) var(--space-4);
    border: 1px solid transparent;
    border-radius: 8px;
    background: none;
    color: var(--color-text-muted);
    cursor: pointer;
  }

  .tab:hover {
    color: var(--color-text);
    background: var(--color-surface-hover);
  }

  .tab[aria-selected='true'] {
    color: var(--color-text);
    background: var(--color-surface);
    border-color: var(--color-border);
  }

  .caption {
    text-align: center;
    color: var(--color-text-muted);
    margin: 0 0 var(--space-5);
  }

  .frame {
    border: 1px solid var(--color-code-border);
    border-radius: 10px;
    overflow: hidden;
    box-shadow: var(--shadow-shot);
    background: var(--color-code-bg);
  }

  .frame img {
    display: block;
    width: 100%;
    height: auto;
  }

  .term {
    margin: 0;
    padding: var(--space-6) var(--space-8);
    font-family: var(--font-mono);
    font-size: 0.875rem;
    line-height: 1.6;
    color: #f4f1e8;
    background: none;
    border: 0;
    overflow-x: auto;
  }

  .term .p { color: #ff6700; }
  .term .dim { color: #8f9cb2; }
  .term .c { color: #7d8ba3; font-style: italic; }
  .term .b { color: #6db6da; }
  .term .y { color: #f2c14e; }

  @media (max-width: 768px) {
    .term {
      font-size: 0.75rem;
      padding: var(--space-4);
    }
  }
</style>
