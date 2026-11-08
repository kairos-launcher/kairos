import { defineConfig } from 'vitepress'

const repo = 'https://github.com/kairos-launcher/kairos'

export default defineConfig({
  title: 'Kairos',
  description: 'A fast, low-memory keyboard launcher for Linux and Windows, extended with plugins.',
  base: '/kairos/',
  cleanUrls: true,
  lastUpdated: true,
  head: [['link', { rel: 'icon', type: 'image/png', href: '/kairos/favicon.png' }]],

  themeConfig: {
    logo: '/logo.png',
    search: { provider: 'local' },
    socialLinks: [{ icon: 'github', link: repo }],
    editLink: { pattern: `${repo}/edit/main/docs/:path`, text: 'Edit this page' },

    nav: [
      { text: 'Guide', link: '/guide/installation', activeMatch: '/guide/' },
      { text: 'Plugins', link: '/plugins/', activeMatch: '/plugins/' },
      { text: 'Write a plugin', link: '/develop/writing-plugins', activeMatch: '/develop/' },
      { text: 'Releases', link: `${repo}/releases` },
    ],

    sidebar: {
      '/guide/': [
        {
          text: 'Guide',
          items: [
            { text: 'Installation', link: '/guide/installation' },
            { text: 'Usage', link: '/guide/usage' },
          ],
        },
        {
          text: 'Configuration',
          items: [
            { text: 'Overview and settings pages', link: '/guide/configuration' },
            { text: 'The settings file', link: '/guide/config-file' },
            { text: 'Secrets', link: '/guide/secrets' },
            { text: 'Files and folders', link: '/guide/files' },
          ],
        },
      ],
      '/plugins/': [
        {
          text: 'Plugins',
          items: [{ text: 'Overview and management', link: '/plugins/' }],
        },
        {
          text: 'Built in',
          items: [
            { text: 'Applications', link: '/plugins/apps' },
            { text: 'Files', link: '/plugins/files' },
            { text: 'Web Search', link: '/plugins/web' },
            { text: 'Shell', link: '/plugins/shell' },
            { text: 'SSH', link: '/plugins/ssh' },
            { text: 'Windows', link: '/plugins/windows' },
            { text: 'Processes', link: '/plugins/processes' },
            { text: 'System', link: '/plugins/system' },
            { text: 'Custom actions', link: '/plugins/actions' },
            { text: 'Script commands', link: '/plugins/scripts' },
            { text: 'Update', link: '/plugins/update' },
          ],
        },
        {
          text: 'Shipped',
          items: [
            { text: 'Calculator', link: '/plugins/calc' },
            { text: 'AI', link: '/plugins/ai' },
            { text: 'Bookmarks', link: '/plugins/bookmarks' },
            { text: 'Emoji', link: '/plugins/emoji' },
            { text: 'Timer', link: '/plugins/timer' },
            { text: 'Dev utilities', link: '/plugins/devtools' },
          ],
        },
      ],
      '/develop/': [
        {
          text: 'Write a plugin',
          items: [
            { text: 'Getting started', link: '/develop/writing-plugins' },
            { text: 'Script commands', link: '/develop/script-commands' },
          ],
        },
        {
          text: 'Languages',
          items: [
            { text: 'Node.js', link: '/develop/nodejs' },
            { text: 'Rust', link: '/develop/rust' },
            { text: 'Go', link: '/develop/go' },
            { text: 'Python', link: '/develop/python' },
          ],
        },
        {
          text: 'Reference',
          items: [
            { text: 'Protocol reference', link: '/develop/protocol' },
            { text: 'Remote plugins', link: '/develop/remote-plugins' },
            { text: 'Bundled plugins in Rust', link: '/develop/bundled-plugins' },
          ],
        },
      ],
    },

    footer: {
      message: 'Released under the MIT License.',
    },
  },
})
