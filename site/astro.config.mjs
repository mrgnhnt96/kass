// @ts-check

import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import sitemap from '@astrojs/sitemap';
import starlight from '@astrojs/starlight';
import { defineConfig } from 'astro/config';
import { OG_IMAGE, OG_IMAGE_ALT, SITE } from './src/site.ts';

const sourceOf = (pathname) => {
  if (pathname === '/') return 'src/pages/index.astro';
  if (pathname === '/changelog/') return '../CHANGELOG.md';
  if (pathname === '/docs/') return 'src/content/docs/docs/index.md';
  const page = pathname.slice(1, -1);
  const candidates = page.startsWith('docs/')
    ? [`src/content/docs/${page}.md`, `src/content/docs/${page}.mdx`]
    : [`src/pages/${page}.astro`];
  return candidates.find(existsSync);
};

const lastChanged = (file) =>
  file && execFileSync('git', ['log', '-1', '--format=%cI', '--', file], { encoding: 'utf8' }).trim();

export default defineConfig({
  site: SITE,
  integrations: [
    sitemap({
      serialize(item) {
        const lastmod = lastChanged(sourceOf(new URL(item.url).pathname));
        return lastmod ? { ...item, lastmod } : item;
      },
    }),
    starlight({
      title: 'Kass',
      // The 404 page is src/pages/404.astro.
      disable404Route: true,
      description: 'Private, local dictation for Apple Silicon Macs.',
      logo: { src: './src/assets/icon.png', alt: '' },
      favicon: '/favicon.svg',
      head: [
        { tag: 'link', attrs: { rel: 'apple-touch-icon', href: '/apple-touch-icon.png' } },
        { tag: 'link', attrs: { rel: 'icon', href: '/favicon-96.png', sizes: '96x96', type: 'image/png' } },
        { tag: 'meta', attrs: { property: 'og:image', content: OG_IMAGE } },
        { tag: 'meta', attrs: { property: 'og:image:width', content: '1200' } },
        { tag: 'meta', attrs: { property: 'og:image:height', content: '630' } },
        { tag: 'meta', attrs: { property: 'og:image:alt', content: OG_IMAGE_ALT } },
        { tag: 'meta', attrs: { name: 'twitter:image', content: OG_IMAGE } },
        { tag: 'script', attrs: { src: '/site-stats.js', async: true } },
        // Docs search-term tracking. Pagefind creates its search input lazily when the
        // search modal opens, so we delegate from the document and match the Pagefind
        // UI input (class `pagefind-ui__search-input`). Each settled query (debounced
        // ~800ms, >= 2 chars, deduped against the last-sent term) emits one
        // `docs_search` Amplitude event, including a numeric `result_count` so we can
        // chart top search terms and analyze zero-result searches later.
        //
        // Pagefind renders results asynchronously after the input settles, so once the
        // debounce fires we poll the scoped `.pagefind-ui` container until its results
        // DOM stops changing (stable across two ~150ms polls) or a ~1500ms cap, THEN
        // read the count and send exactly one event per settled query. The count comes
        // from `.pagefind-ui__message` ("[COUNT] result(s) for [TERM]" / "No results
        // for [TERM]") because Pagefind only renders pageSize (5) `.pagefind-ui__result`
        // items at a time behind a "Load more" button — the message holds the true
        // total. The leading-integer match avoids mistaking digits in the search term
        // for the count (e.g. "No results for iOS 17" -> 0).
        {
          tag: 'script',
          content: [
            '(function () {',
            "  var INPUT_SELECTOR = '.pagefind-ui__search-input';",
            "  var UI_SELECTOR = '.pagefind-ui';",
            "  var RESULT_SELECTOR = '.pagefind-ui__result';",
            "  var MESSAGE_SELECTOR = '.pagefind-ui__message';",
            '  var DEBOUNCE_MS = 800;',
            '  var POLL_MS = 150;',
            '  var STABLE_MAX_MS = 1500;',
            '  var lastSent = null;',
            '  var debounceTimer = null;',
            '  var pollToken = 0;',
            '  function readCount(root) {',
            '    if (!root) return 0;',
            '    var msg = root.querySelector(MESSAGE_SELECTOR);',
            '    if (msg) {',
            "      var text = (msg.textContent || '').trim();",
            '      var m = text.match(/^\\s*([\\d,]+)/);',
            '      if (m) {',
            "        var n = parseInt(m[1].replace(/[^\\d]/g, ''), 10);",
            '        if (!isNaN(n)) return n;',
            '      }',
            '      return 0;',
            '    }',
            '    return root.querySelectorAll(RESULT_SELECTOR).length;',
            '  }',
            '  function signature(root) {',
            "    if (!root) return '';",
            '    var msg = root.querySelector(MESSAGE_SELECTOR);',
            "    return (msg ? (msg.textContent || '') : '') + '|' + root.querySelectorAll(RESULT_SELECTOR).length;",
            '  }',
            '  function send(query, root) {',
            "    if (!window.amplitude || typeof window.amplitude.track !== 'function') return;",
            '    lastSent = query;',
            "    window.amplitude.track('docs_search', { search_term: query, result_count: readCount(root) });",
            '  }',
            '  function waitAndSend(query, root) {',
            '    var myToken = ++pollToken;',
            '    var lastSig = signature(root);',
            '    var stableCount = 0;',
            '    var elapsed = 0;',
            '    var iv = setInterval(function () {',
            '      if (myToken !== pollToken) { clearInterval(iv); return; }',
            '      elapsed += POLL_MS;',
            '      var sig = signature(root);',
            '      if (sig === lastSig) { stableCount += 1; } else { stableCount = 0; lastSig = sig; }',
            '      if (stableCount >= 2 || elapsed >= STABLE_MAX_MS) {',
            '        clearInterval(iv);',
            '        if (myToken === pollToken) send(query, root);',
            '      }',
            '    }, POLL_MS);',
            '  }',
            "  document.addEventListener('input', function (event) {",
            '    var target = event.target;',
            '    if (!target || typeof target.matches !== "function" || !target.matches(INPUT_SELECTOR)) return;',
            "    var query = (target.value || '').trim();",
            "    var root = typeof target.closest === 'function' ? target.closest(UI_SELECTOR) : null;",
            '    if (debounceTimer) clearTimeout(debounceTimer);',
            '    debounceTimer = setTimeout(function () {',
            '      if (query.length < 2) return;',
            '      if (query === lastSent) return;',
            '      waitAndSend(query, root);',
            '    }, DEBOUNCE_MS);',
            '  }, true);',
            '})();',
          ].join('\n'),
        },
      ],
      customCss: ['./src/styles/docs.css'],
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/mrgnhnt96/kass' }],
      editLink: { baseUrl: 'https://github.com/mrgnhnt96/kass/edit/main/site/' },
      sidebar: [
        {
          label: 'Start here',
          items: [
            { label: 'Overview', slug: 'docs' },
            { label: 'Install', slug: 'docs/install' },
            { label: 'Your first dictation', slug: 'docs/first-dictation' },
          ],
        },
        {
          label: 'Using Kass',
          items: [
            { label: 'Hotkeys', slug: 'docs/hotkeys' },
            { label: 'Spoken commands', slug: 'docs/spoken-commands' },
            { label: 'Writing styles', slug: 'docs/writing-styles' },
            { label: 'Dictionary', slug: 'docs/dictionary' },
            { label: 'Command Mode', slug: 'docs/command-mode' },
            { label: 'Captures and corrections', slug: 'docs/captures' },
          ],
        },
        {
          label: 'Setup',
          items: [
            { label: 'Models', slug: 'docs/models' },
            { label: 'Settings', slug: 'docs/settings' },
            { label: 'Privacy', slug: 'docs/privacy' },
            { label: 'Troubleshooting', slug: 'docs/troubleshooting' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'Local API', slug: 'docs/api' },
            { label: 'Build from source', slug: 'docs/build-from-source' },
            { label: 'Changelog', link: '/changelog/' },
            {
              label: 'Report an issue',
              link: 'https://github.com/mrgnhnt96/kass/issues/new/choose',
              attrs: { target: '_blank' },
            },
          ],
        },
      ],
    }),
  ],
});
