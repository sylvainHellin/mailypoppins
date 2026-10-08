import { defineConfig, passthroughImageService } from 'astro/config';
import starlight from '@astrojs/starlight';
import sitemap from '@astrojs/sitemap';

const repo = 'https://github.com/sylvainHellin/mailypoppins';

export default defineConfig({
  site: 'https://mailypoppins.dev',
  output: 'static',
  build: {
    assets: '_assets',
  },
  // Smart punctuation would turn every `--flag` in prose into an en dash.
  markdown: {
    smartypants: false,
  },
  // No sharp in this project: images ship as they are.
  image: {
    service: passthroughImageService(),
  },
  // The pre-Starlight site's URLs keep working.
  redirects: {
    '/getting-started': '/start/install/',
    '/commands': '/reference/commands/',
    '/config': '/reference/configuration/',
    '/draft-format': '/reference/draft-format/',
    '/faq': '/reference/faq/',
    '/github': '/project/open-source/',
  },
  integrations: [
    starlight({
      title: 'mailypoppins',
      description:
        'An email client for the terminal and the Mac. Drafts are Markdown files, received mail lives in a local store, and one local daemon serves the CLI, the TUI and the desktop app.',
      logo: {
        light: './src/assets/mark-ink.svg',
        dark: './src/assets/mark-cream.svg',
        alt: 'mailypoppins',
        replacesTitle: false,
      },
      favicon: '/favicon.svg',
      social: [{ icon: 'github', label: 'GitHub', href: repo }],
      editLink: {
        baseUrl: `${repo}/edit/main/website/`,
      },
      lastUpdated: false,
      customCss: ['./src/styles/fonts.css', './src/styles/theme.css'],
      components: {
        Hero: './src/components/Hero.astro',
      },
      head: [
        {
          tag: 'link',
          attrs: { rel: 'preload', href: '/fonts/Inter-Variable.woff2', as: 'font', type: 'font/woff2', crossorigin: '' },
        },
        { tag: 'meta', attrs: { name: 'theme-color', content: '#0C1B33' } },
      ],
      expressiveCode: {
        themes: ['github-dark-default', 'github-light-default'],
        styleOverrides: {
          borderRadius: '0.6rem',
          borderColor: 'var(--sl-color-hairline-light)',
          codeFontSize: '0.875rem',
          codeLineHeight: '1.7',
          codeBackground: 'var(--mp-code-bg)',
          frames: {
            shadowColor: 'transparent',
            editorBackground: 'var(--mp-code-bg)',
            terminalBackground: 'var(--mp-code-bg)',
            editorTabBarBackground: 'var(--mp-code-bar)',
            editorActiveTabBackground: 'var(--mp-code-bg)',
            editorActiveTabIndicatorTopColor: 'var(--mp-pumpkin)',
            editorTabBarBorderBottomColor: 'var(--sl-color-hairline-light)',
            terminalTitlebarBackground: 'var(--mp-code-bar)',
            terminalTitlebarBorderBottomColor: 'var(--sl-color-hairline-light)',
            terminalTitlebarDotsForeground: 'var(--mp-code-dots)',
            inlineButtonBackground: 'var(--sl-color-gray-5)',
          },
        },
      },
      tableOfContents: { minHeadingLevel: 2, maxHeadingLevel: 3 },
      sidebar: [
        { label: 'Overview', slug: 'docs' },
        {
          label: 'Get started',
          items: [
            { label: 'Install', slug: 'start/install' },
            { label: 'Connect an account', slug: 'start/connect' },
            { label: 'First sync', slug: 'start/first-sync' },
            { label: 'Read, write and send', slug: 'start/read-write-send' },
          ],
        },
        {
          label: 'Guides',
          items: [
            { label: 'Drafts and sending', slug: 'guides/drafts-and-sending' },
            { label: 'Searching mail', slug: 'guides/search' },
            { label: 'Invitations', slug: 'guides/invitations' },
            { label: 'Contacts and calendar', slug: 'guides/contacts-and-calendar' },
            { label: 'The desktop app', slug: 'guides/desktop-app' },
            { label: 'Microsoft 365 and Exchange', slug: 'guides/microsoft-365' },
            { label: 'The daemon', slug: 'guides/daemon' },
            { label: 'Scripting and agents', slug: 'guides/scripting' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'Commands', slug: 'reference/commands' },
            { label: 'Key bindings', slug: 'reference/keys' },
            { label: 'Configuration', slug: 'reference/configuration' },
            { label: 'Draft format', slug: 'reference/draft-format' },
            { label: 'Selectors', slug: 'reference/selectors' },
            { label: 'FAQ', slug: 'reference/faq' },
          ],
        },
        {
          label: 'Project',
          items: [
            { label: 'Open source', slug: 'project/open-source' },
            { label: 'Changelog', link: `${repo}/blob/main/CHANGELOG.md`, attrs: { target: '_blank' } },
          ],
        },
      ],
    }),
    sitemap(),
  ],
});
