import { defineConfig } from 'vitepress'
import { groupIconMdPlugin, groupIconVitePlugin } from 'vitepress-plugin-group-icons'

// https://vitepress.dev/reference/site-config
export default defineConfig({
  srcDir: "src",
  
  title: "ocx",
  description: "the simple package manager",
  
  themeConfig: {
    // https://vitepress.dev/reference/default-theme-config
    logo: '/assets/logo.svg',
    nav: [
      { text: 'Home', link: '/' },
      { text: 'Docs', link: '/docs/user-guide' },
      { text: 'Team', link: '/team' },
      {
        text: 'Help',
        items: [
          { text: 'MD', link: '/help/markdown-examples' },
          { text: 'API', link: '/help/api-examples' },
          { text: 'Components', link: '/help/component-examples' }
        ]
      }
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
          text: "Reference",
          collapsed: true,
          items: [
            { text: "Command Line", link: "/docs/reference/command-line" },
            { text: "Environment", link: "/docs/reference/environment" },
          ],
        }
      ],
    },

    socialLinks: [
      { icon: 'github', link: 'https://github.com/ocx-sh/ocx' },
      { icon: 'discord', link: 'https://discord.gg/BuRhhAYy9r' }
    ]
  },

  markdown: {
    config(md) {
      md.use(groupIconMdPlugin)
    }
  },

  vite: {
    plugins: [
      groupIconVitePlugin({})
    ]
  }
})
