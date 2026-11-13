---
title: "Changelog"
# Release notes carry a character outside the preloaded Latin subset (the "ä" in a commit subject), so the browser
# fetches the theme's "rest" font only after layout: one extra round trip that pushes LCP past the 100 budget.
# The hash is the font file's content hash; a theme bump that renames it turns the Lighthouse gate red.
head:
  - tag: link
    attrs:
      rel: preload
      href: /docs/_astro/plex-sans-400-rest.DJbOTdeZ.woff2
      as: font
      type: font/woff2
      crossorigin: ""
---
<!-- doc_type: changelog -->

<!--@include: ../../_changelog.md{3,}-->

Older releases are on the [GitHub releases page][releases].

[releases]: https://github.com/ocx-sh/ocx/releases
