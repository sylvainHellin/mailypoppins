<script lang="ts">
  // The clients, one at a time in a window frame, picked with a segmented
  // switch. The view advances on its own every few seconds: a CSS progress
  // bar runs for INTERVAL and its animationend moves to the next client.
  // Hovering or focusing the block pauses the bar, a click on the switch
  // stops the rotation, and prefers-reduced-motion removes the animation,
  // so no animationend fires and nothing rotates.
  // Every panel sits in the same grid cell, so the frame keeps the height of
  // the tallest one and the page does not jump when the view changes.
  interface Props {
    hasDesktopShot: boolean;
  }

  let { hasDesktopShot }: Props = $props();

  const INTERVAL_S = 8;

  const clients = [
    { id: 'tui', label: 'Terminal app', caption: 'Mailboxes, the message list and the reader sit side by side. The app also runs over SSH.' },
    { id: 'cli', label: 'Command line', caption: 'Search, reply, edit and send from the shell. Scripts and agents use these commands too.' },
    ...(hasDesktopShot
      ? [{ id: 'desktop', label: 'Mac app', caption: 'The Mac app uses the same keys as the terminal app and opens each draft in Neovim.' }]
      : []),
  ];

  let active = $state('tui');
  let rotating = $state(true);
  let paused = $state(false);

  function pick(id: string) {
    active = id;
    rotating = false;
  }

  function advance() {
    const i = clients.findIndex((c) => c.id === active);
    active = clients[(i + 1) % clients.length].id;
  }
</script>

<div
  class="clients"
  role="group"
  aria-label="The mailypoppins clients"
  onpointerenter={() => (paused = true)}
  onpointerleave={() => (paused = false)}
  onfocusin={() => (paused = true)}
  onfocusout={() => (paused = false)}
>
  <fieldset class="switch">
    <legend class="visually-hidden">Show a client</legend>
    {#each clients as c (c.id)}
      <label class="seg" class:on={active === c.id}>
        <input
          type="radio"
          name="client"
          value={c.id}
          checked={active === c.id}
          onchange={() => pick(c.id)}
        />
        <span class="seg-label">{c.label}</span>
        {#if rotating && active === c.id}
          {#key active}
            <span
              class="seg-progress"
              class:paused
              style={`animation-duration: ${INTERVAL_S}s`}
              onanimationend={advance}
              aria-hidden="true"
            ></span>
          {/key}
        {/if}
      </label>
    {/each}
  </fieldset>

  <div class="frame">
    <div class="bar" aria-hidden="true">
      <span class="dots"><i></i><i></i><i></i></span>
      <span class="title">{clients.find((c) => c.id === active)?.label}</span>
    </div>

    <div class="stack" aria-live={rotating ? 'off' : 'polite'}>
      {#each clients as c (c.id)}
        <div class="panel" class:shown={active === c.id} aria-hidden={active !== c.id}>
          {#if c.id === 'tui'}
            <img
              src="/tui-screenshot.png"
              alt="The terminal app: mailboxes, message list and reading pane"
              width="2496"
              height="1356"
              loading="lazy"
            />
          {:else if c.id === 'cli'}
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
      {/each}
    </div>
  </div>

  <p class="caption">{clients.find((c) => c.id === active)?.caption}</p>
</div>

<style>
  .switch {
    display: flex;
    width: fit-content;
    margin: 0 auto var(--space-6);
    padding: 4px;
    gap: 4px;
    border: 1px solid var(--color-border);
    border-radius: 999px;
    background: var(--color-surface);
  }

  .seg {
    position: relative;
    overflow: hidden;
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-4);
    border-radius: 999px;
    font-size: 0.875rem;
    font-weight: 500;
    color: var(--color-text-muted);
    cursor: pointer;
    transition: background 0.2s ease, color 0.2s ease;
  }

  .seg:hover {
    color: var(--color-text);
  }

  /* A radio dot, so the control reads as a choice of one. */
  .seg input {
    appearance: none;
    margin: 0;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    border: 1.5px solid currentColor;
    display: grid;
    place-content: center;
    cursor: pointer;
  }

  .seg input::before {
    content: '';
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--color-cta);
    transform: scale(0);
    transition: transform 0.15s ease;
  }

  .seg input:checked::before {
    transform: scale(1);
  }

  .seg input:focus-visible {
    outline: 2px solid var(--color-ring);
    outline-offset: 2px;
  }

  .seg.on {
    background: var(--color-code-header);
    color: #f4f1e8;
  }

  .seg.on input {
    border-color: var(--color-cta);
  }

  /* Inset from the rounded ends, so the whole run stays visible. */
  .seg-progress {
    position: absolute;
    left: var(--space-4);
    right: var(--space-4);
    bottom: 4px;
    height: 2px;
    border-radius: 1px;
    background: var(--color-cta);
    transform-origin: left;
    animation-name: fill;
    animation-timing-function: linear;
    animation-fill-mode: forwards;
  }

  .seg-progress.paused {
    animation-play-state: paused;
  }

  @keyframes fill {
    from { transform: scaleX(0); }
    to { transform: scaleX(1); }
  }

  @media (prefers-reduced-motion: reduce) {
    .seg-progress {
      display: none;
    }
  }

  .frame {
    border: 1px solid var(--color-code-border);
    border-radius: 10px;
    overflow: hidden;
    box-shadow: var(--shadow-shot);
    background: var(--color-code-bg);
  }

  .bar {
    position: relative;
    display: flex;
    align-items: center;
    height: 2.25rem;
    padding: 0 var(--space-4);
    background: var(--color-code-header);
    border-bottom: 1px solid var(--color-code-border);
  }

  .dots {
    display: flex;
    gap: 6px;
  }

  .dots i {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: rgba(244, 241, 232, 0.2);
  }

  .title {
    position: absolute;
    left: 50%;
    transform: translateX(-50%);
    font-size: 0.8125rem;
    color: var(--color-code-label);
  }

  .stack {
    display: grid;
  }

  .panel {
    grid-area: 1 / 1;
    align-self: center;
    opacity: 0;
    visibility: hidden;
    transition: opacity 0.35s ease, visibility 0s linear 0.35s;
  }

  .panel.shown {
    opacity: 1;
    visibility: visible;
    transition: opacity 0.35s ease;
  }

  .panel img {
    display: block;
    width: 100%;
    height: auto;
  }

  .caption {
    text-align: center;
    color: var(--color-text-muted);
    margin: var(--space-4) auto 0;
    max-width: 40rem;
    min-height: 3.2em;
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

    .seg {
      padding: var(--space-2) var(--space-3);
    }
  }
</style>
