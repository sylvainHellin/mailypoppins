<script lang="ts">
  interface Props {
    currentPath: string;
  }

  let { currentPath }: Props = $props();

  let open = $state(false);

  const navLinks = [
    { href: '/', label: 'Home' },
    { href: '/getting-started/', label: 'Getting Started' },
    { href: '/commands/', label: 'Commands' },
    { href: '/draft-format/', label: 'Draft Format' },
    { href: '/config/', label: 'Config' },
    { href: '/faq/', label: 'FAQ' },
  ];

  function isActive(href: string): boolean {
    if (href === '/') return currentPath === '/';
    return currentPath.startsWith(href);
  }

  function toggle() {
    open = !open;
  }

  function close() {
    open = false;
  }
</script>

<button
  class="hamburger"
  onclick={toggle}
  aria-label={open ? 'Close menu' : 'Open menu'}
  aria-expanded={open}
>
  {#if open}
    <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round">
      <line x1="4" y1="4" x2="16" y2="16" />
      <line x1="16" y1="4" x2="4" y2="16" />
    </svg>
  {:else}
    <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round">
      <line x1="3" y1="5" x2="17" y2="5" />
      <line x1="3" y1="10" x2="17" y2="10" />
      <line x1="3" y1="15" x2="17" y2="15" />
    </svg>
  {/if}
</button>

{#if open}
  <div class="overlay" role="presentation" onclick={close}></div>
  <div class="menu">
    <nav>
      {#each navLinks as link}
        <a
          href={link.href}
          class="menu-link"
          class:active={isActive(link.href)}
          onclick={close}
        >
          {link.label}
        </a>
      {/each}
    </nav>
  </div>
{/if}

<style>
  .hamburger {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 36px;
    height: 36px;
    background: none;
    border: none;
    color: var(--color-subtext0);
    cursor: pointer;
    border-radius: 6px;
    transition: color 0.15s ease, background 0.15s ease;
  }

  .hamburger:hover {
    color: var(--color-text);
    background: var(--color-surface0);
  }

  .overlay {
    position: fixed;
    inset: var(--nav-height) 0 0 0;
    background: rgba(0, 0, 0, 0.5);
    z-index: 90;
  }

  .menu {
    position: fixed;
    top: var(--nav-height);
    left: 0;
    right: 0;
    background: var(--color-bg-nav);
    border-bottom: 1px solid var(--color-border-subtle);
    padding: var(--space-4);
    z-index: 95;
    animation: slide-down 0.15s ease-out;
  }

  @keyframes slide-down {
    from {
      opacity: 0;
      transform: translateY(-8px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }

  .menu nav {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }

  .menu-link {
    display: block;
    padding: var(--space-3) var(--space-4);
    font-size: 1rem;
    color: var(--color-subtext0);
    border-radius: 6px;
    text-decoration: none;
    transition: color 0.15s ease, background 0.15s ease;
  }

  .menu-link:hover {
    color: var(--color-text);
    background: var(--color-surface0);
  }

  .menu-link.active {
    color: var(--color-mauve);
    background: color-mix(in srgb, var(--color-mauve) 10%, transparent);
  }
</style>
