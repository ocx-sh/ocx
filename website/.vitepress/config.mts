import { defineConfig } from 'vitepress'
import { groupIconMdPlugin, groupIconVitePlugin, localIconLoader } from 'vitepress-plugin-group-icons'
import licensedAssetFallback from './plugins/licensed-asset-fallback.mts'

const deployTarget = process.env.OCX_DEPLOY_TARGET === 'prod' ? 'prod' : 'dev'

const devBannerStyle: [string, Record<string, string>, string] = [
  'style',
  {},
  ':root{--vp-layout-top-height:40px}@media (max-width:768px){:root{--vp-layout-top-height:56px}}',
]

// https://vitepress.dev/reference/site-config
export default defineConfig({
  srcDir: "src",
  cleanUrls: true,

  title: "ocx",
  description: "the simple package manager",

  head: [
    ...(deployTarget === 'dev' ? [devBannerStyle] : []),
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
    deployTarget,
    logo: '/logo.svg',
    nav: [
      { text: 'Home', link: '/' },
      { text: 'Roadmap', link: '/docs/roadmap' },
      { text: 'Catalog', link: '/catalog' },
      { text: 'Docs', link: '/docs/user-guide' },
      { text: 'Team', link: '/team' },
    ],

    sidebar: {
      "/docs/roadmap": [],
      "/catalog": [],
      "/team": [],
      "/": [
        {
          text: "Installation",
          link: "/docs/installation",
        },
        {
          text: "Docker",
          link: "/docs/docker",
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
          text: "Patching",
          link: "/docs/user-guide/patches",
        },
        {
          text: "Authoring",
          link: "/docs/authoring/",
          collapsed: true,
          items: [
            { text: "Overview",               link: "/docs/authoring/" },
            { text: "Bundle Anatomy",         link: "/docs/authoring/bundle-anatomy" },
            { text: "Declaring Dependencies", link: "/docs/authoring/dependencies" },
            { text: "Env Surface",            link: "/docs/authoring/env-surface" },
            { text: "Entry Points",           link: "/docs/authoring/entry-points" },
            { text: "Building & Pushing",     link: "/docs/authoring/building-pushing" },
            { text: "Testing locally",        link: "/docs/authoring/testing" },
            { text: "Multi-Platform",         link: "/docs/authoring/multi-platform" },
            { text: "Migration",              link: "/docs/authoring/migration" },
          ],
        },
        {
          text: "In Depth",
          collapsed: true,
          items: [
            { text: "Storage", link: "/docs/in-depth/storage" },
            { text: "Versioning", link: "/docs/in-depth/versioning" },
            { text: "Indices", link: "/docs/in-depth/indices" },
            { text: "Dependencies", link: "/docs/in-depth/dependencies" },
            { text: "Project Toolchain", link: "/docs/in-depth/project" },
            { text: "Configuration", link: "/docs/in-depth/configuration" },
            { text: "Environments", link: "/docs/in-depth/environments" },
            { text: "Entry Points", link: "/docs/in-depth/entry-points" },
            { text: "CI Integration", link: "/docs/in-depth/ci" },
            { text: "Signing", link: "/docs/in-depth/signing" },
          ],
        },
        {
          text: "Reference",
          collapsed: true,
          items: [
            { text: "Command Line", link: "/docs/reference/command-line" },
            { text: "Script Host API", link: "/docs/reference/script-host-api" },
            { text: "Configuration", link: "/docs/reference/configuration" },
            { text: "Environment", link: "/docs/reference/environment" },
            { text: "Metadata", link: "/docs/reference/metadata" },
            { text: "Platforms", link: "/docs/reference/platforms" },
            { text: "mirror.yml ↗", link: "https://ocx-sh.github.io/ocx-mirror/reference/mirror-yml/" },
          ],
        },
        {
          text: "FAQ",
          link: "/docs/faq",
        },
        {
          text: "Changelog",
          link: "/docs/changelog",
        },
        {
          text: "Dependencies",
          link: "/docs/reference/dependencies",
        }
      ],
    },

    socialLinks: [
      { icon: 'github', link: 'https://github.com/ocx-sh/ocx' },
      { icon: 'discord', link: 'https://discord.gg/mT2UCF8CVe' }
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
        // Icons are matched against the code-group tab LABEL (case-insensitive
        // substring), longest key first — so `powershell`/`nushell` win over the
        // shorter `shell` key. fish and elvish have no bundled iconify glyph, so
        // they load local brand SVGs from `.vitepress/icons/`.
        customIcon: {
          shell: 'vscode-icons:file-type-shell',
          powershell: 'vscode-icons:file-type-powershell',
          nushell: 'vscode-icons:file-type-nushell',
          fish: localIconLoader(import.meta.url, './icons/fish.svg'),
          elvish: localIconLoader(import.meta.url, './icons/elvish.svg'),
        },
      }),
      licensedAssetFallback(),
    ]
  }
})
