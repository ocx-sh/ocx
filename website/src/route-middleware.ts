// Collapses every top-level sidebar group but the current page's to one link to its first page.
// The full sidebar is ~310 elements and ~3.4 KB gzip on every page; the page budget cannot afford it.
import { defineRouteMiddleware } from '@astrojs/starlight/route-data';
import type { StarlightRouteData } from '@astrojs/starlight/route-data';

type Entry = StarlightRouteData['sidebar'][number];
type Group = Extract<Entry, { type: 'group' }>;

const firstLink = (entries: Entry[]): Extract<Entry, { type: 'link' }> | undefined => {
  for (const e of entries) {
    const found = e.type === 'link' ? e : firstLink(e.entries);
    if (found) return found;
  }
};

const holdsCurrent = (g: Group): boolean =>
  g.entries.some((e) => (e.type === 'link' ? e.isCurrent : holdsCurrent(e)));

export const onRequest = defineRouteMiddleware(async (context, next) => {
  const route = context.locals.starlightRoute;
  route.sidebar = route.sidebar.map((e) => {
    if (e.type === 'link' || holdsCurrent(e)) return e;
    const first = firstLink(e.entries);
    if (!first) return e;
    // Same shape as a link entry, so the theme's sidebar renders it as a top-level link.
    return { type: 'link', label: e.label, href: first.href, isCurrent: false, badge: e.badge, attrs: {} };
  });
  await next();
});
