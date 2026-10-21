// The site navigation, shared by Nav.astro and MobileMenu.svelte.
// The reference pages (commands, config, draft format) sit under one entry
// and carry their own tab row (DocsTabs.astro).
export interface NavLink {
  href: string;
  label: string;
  // Extra path prefixes that mark this entry active.
  also?: string[];
}

export const navLinks: NavLink[] = [
  { href: '/#download', label: 'Download' },
  { href: '/getting-started/', label: 'Guide' },
  { href: '/commands/', label: 'Reference', also: ['/config/', '/draft-format/'] },
  { href: '/faq/', label: 'FAQ' },
];

export const docsTabs: NavLink[] = [
  { href: '/commands/', label: 'Commands' },
  { href: '/draft-format/', label: 'Drafts' },
  { href: '/config/', label: 'Config' },
];

export function isActive(link: NavLink, currentPath: string): boolean {
  if (link.href.startsWith('/#')) return false;
  return [link.href, ...(link.also ?? [])].some((prefix) => currentPath.startsWith(prefix));
}
