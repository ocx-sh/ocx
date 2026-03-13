import { defineConfig } from 'vitepress'
import { groupIconMdPlugin, groupIconVitePlugin } from 'vitepress-plugin-group-icons'

// https://vitepress.dev/reference/site-config
export default defineConfig({
  srcDir: "src",

  title: "ocx",
  description: "the simple package manager",

  head: [
    ['link', { rel: 'icon', type: 'image/svg+xml', href: '/logo.svg' }],
    ['link', { rel: 'icon', type: 'image/png', sizes: '96x96', href: '/icons/favicon-96x96.png' }],
    ['link', { rel: 'icon', type: 'image/png', sizes: '32x32', href: '/icons/favicon-32x32.png' }],
    ['link', { rel: 'icon', type: 'image/png', sizes: '16x16', href: '/icons/favicon-16x16.png' }],
    ['link', { rel: 'apple-touch-icon', sizes: '180x180', href: '/apple-touch-icon.png' }],
    ['link', { rel: 'icon', type: 'image/png', sizes: '192x192', href: '/icons/favicon-192x192.png' }],
    ['link', { rel: 'icon', type: 'image/png', sizes: '512x512', href: '/icons/favicon-512x512.png' }],
    ['link', { rel: 'manifest', href: '/site.webmanifest' }],
  ],

  themeConfig: {
    // https://vitepress.dev/reference/default-theme-config
    logo: '/logo.svg',
    nav: [
      { text: 'Home', link: '/' },
      { text: 'Docs', link: '/docs/user-guide' },
      { text: 'Team', link: '/team' },
    ],

    sidebar: {
      "/team": [],
      "/": [
        {
          text: "Installation",
          link: "/docs/installation",
        },
        {
          text: "Getting Started",
          link: "/docs/getting-started",
        },
        {
          text: "User Guide",
          link: "/docs/user-guide",
        },
        {
          text: "FAQ",
          link: "/docs/faq",
        },
        {
          text: "Reference",
          collapsed: true,
          items: [
            { text: "Command Line", link: "/docs/reference/command-line" },
            { text: "Environment", link: "/docs/reference/environment" },
            { text: "Metadata", link: "/docs/reference/metadata" },
          ],
        },
        {
          text: "Changelog",
          link: "/docs/changelog",
        }
      ],
    },

    socialLinks: [
      { icon: 'github', link: 'https://github.com/ocx-sh/ocx' },
      { icon: 'discord', link: 'https://discord.gg/BuRhhAYy9r' }
    ],

    search: {
      provider: 'local'
    },

    footer: {
      copyright: 'Copyright © 2026 The OCX Authors · <a href="https://github.com/ocx-sh/ocx/blob/main/LICENSE">Apache 2.0</a>'
    }
  },

  markdown: {
    config(md) {
      md.use(groupIconMdPlugin)
    }
  },

  vite: {
    plugins: [
      groupIconVitePlugin({
        customIcon: {
          shell: 'vscode-icons:file-type-shell',
          powershell: 'vscode-icons:file-type-powershell',
        },
      })
    ]
  }
})
