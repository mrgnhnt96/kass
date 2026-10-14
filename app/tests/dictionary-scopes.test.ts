import { describe, expect, test } from 'bun:test';
import {
  buildScopeOptions,
  EVERYWHERE,
  entriesIn,
  entryAge,
  entryNote,
  entryPlaceKeys,
  inheritedForApp,
  inheritedForStyle,
  inheritedOfKind,
  isKind,
  looksLikeCode,
  newEntry,
  newPhrase,
  parseScopeKey,
  placesFromKeys,
  previewWritten,
  samePlaces,
  scopeKey,
  togglePlace,
} from '../src/components/Settings/dictionaryScopes';
import type {
  DictionaryEntry,
  DictionaryPlace,
  ResolvedDictionaryEntry,
  WritingStylesResponse,
} from '../src/lib/api/types';

const GLOBAL: DictionaryPlace = { scope: 'global', scope_id: null, app_name: null };
const style = (id: string): DictionaryPlace => ({ scope: 'style', scope_id: id, app_name: null });
const app = (id: string, name: string | null = null): DictionaryPlace => ({
  scope: 'app',
  scope_id: id,
  app_name: name,
});

function entry(
  written: string,
  places: DictionaryPlace[] = [GLOBAL],
  created_at = '2026-09-28T10:00:00',
  spoken: string | null = null,
  phrase = false,
): DictionaryEntry {
  return {
    id: written,
    written,
    spoken,
    places,
    created_at,
    match_sound: true,
    source: 'user',
    phrase,
  };
}

function resolved(
  id: string,
  scope: DictionaryPlace['scope'],
  scope_id: string | null = null,
): ResolvedDictionaryEntry {
  return {
    id,
    written: id,
    spoken: null,
    scope,
    scope_id,
    app_name: null,
    created_at: '2026-09-28T10:00:00',
    match_sound: true,
    phrase: false,
    overridden: false,
  };
}

const styles = {
  styles: [
    { id: 'casual', name: 'Casual', position: 1, is_default: false },
    { id: 'formal', name: 'Formal', position: 0, is_default: true },
  ],
  apps: [
    { bundle_id: 'com.tinyspeck.slackmacgap', name: 'Slack', style_id: 'casual' },
    { bundle_id: 'dev.zed.Zed', name: 'Zed', style_id: 'casual' },
    { bundle_id: 'com.apple.mail', name: null, style_id: 'formal' },
  ],
  max_styles: 5,
} as unknown as WritingStylesResponse;

describe('dictionary scope keys', () => {
  test('round-trip through the key', () => {
    for (const scope of [
      EVERYWHERE,
      { kind: 'style', styleId: 'casual' } as const,
      { kind: 'app', bundleId: 'com.apple.mail' } as const,
    ]) {
      expect(parseScopeKey(scopeKey(scope))).toEqual(scope);
    }
  });

  test('a missing or malformed key is everywhere', () => {
    expect(parseScopeKey(undefined)).toEqual(EVERYWHERE);
    expect(parseScopeKey('style:')).toEqual(EVERYWHERE);
    expect(parseScopeKey('nonsense')).toEqual(EVERYWHERE);
  });
});

describe('dictionary entries by scope', () => {
  const all = [
    entry('kubernetes', [GLOBAL], '2026-09-20T10:00:00'),
    entry('LGTM', [style('casual'), app('dev.zed.Zed')], '2026-09-27T10:00:00'),
    entry('Morgan', [app('com.tinyspeck.slackmacgap')], '2026-09-26T10:00:00'),
    entry('pytest', [app('dev.zed.Zed')], '2026-09-28T10:00:00'),
  ];

  test('a scope keeps every entry that applies there, newest first', () => {
    expect(entriesIn(all, { kind: 'app', bundleId: 'dev.zed.Zed' }).map((e) => e.written)).toEqual([
      'pytest',
      'LGTM',
    ]);
    expect(entriesIn(all, { kind: 'style', styleId: 'casual' }).map((e) => e.written)).toEqual([
      'LGTM',
    ]);
    expect(entriesIn(all, EVERYWHERE).map((e) => e.written)).toEqual(['kubernetes']);
    expect(entriesIn(all, { kind: 'style', styleId: 'formal' })).toEqual([]);
  });

  test('scope list: styles by position, apps by name, counts per place', () => {
    const orphan = entry('x', [app('com.gone.app', 'Gone')]);
    const options = buildScopeOptions(styles, [...all, orphan]);
    expect(options.everywhere.count).toBe(1);
    expect(options.styles.map((o) => [o.name, o.count, o.appCount])).toEqual([
      ['Formal', 0, 1],
      ['Casual', 1, 2],
    ]);
    // Mail has no name, so its bundle id stands in; an app with entries but gone from styles stays.
    expect(options.apps.map((o) => [o.name, o.count, o.styleId])).toEqual([
      ['com.apple.mail', 0, 'formal'],
      ['Gone', 1, undefined],
      ['Slack', 1, 'casual'],
      ['Zed', 2, 'casual'],
    ]);
  });

  test('place keys ignore repeats', () => {
    expect(entryPlaceKeys(entry('a', [style('casual'), style('casual'), app('x')]))).toEqual([
      'style:casual',
      'app:x',
    ]);
  });
});

describe('adding and placing an entry', () => {
  test('a blank "when I say" teaches only the spelling', () => {
    expect(newEntry(EVERYWHERE, ' Kubernetes ', '  ')).toEqual({
      written: 'Kubernetes',
      spoken: null,
      places: [{ scope: 'global' }],
    });
  });

  test('a new entry goes in the selected scope only', () => {
    expect(newEntry({ kind: 'style', styleId: 'casual' }, 'LGTM', 'looks good')).toEqual({
      written: 'LGTM',
      spoken: 'looks good',
      places: [{ scope: 'style', scope_id: 'casual' }],
    });
    expect(newEntry({ kind: 'app', bundleId: 'com.apple.mail' }, 'Hi', '', 'Mail').places).toEqual([
      { scope: 'app', scope_id: 'com.apple.mail', app_name: 'Mail' },
    ]);
  });

  test('place keys become bodies with app names', () => {
    const options = buildScopeOptions(styles, []);
    expect(placesFromKeys(['style:casual', 'app:dev.zed.Zed'], options)).toEqual([
      { scope: 'style', scope_id: 'casual' },
      { scope: 'app', scope_id: 'dev.zed.Zed', app_name: 'Zed' },
    ]);
  });

  test('checking everywhere clears the rest', () => {
    expect(togglePlace(['style:casual', 'app:x'], 'global')).toEqual(['global']);
  });

  test('checking anything else clears everywhere', () => {
    expect(togglePlace(['global'], 'app:x')).toEqual(['app:x']);
    expect(togglePlace(['style:casual'], 'app:x')).toEqual(['style:casual', 'app:x']);
  });

  test('unchecking removes the place, even the last one', () => {
    expect(togglePlace(['style:casual', 'app:x'], 'app:x')).toEqual(['style:casual']);
    expect(togglePlace(['global'], 'global')).toEqual([]);
  });

  test('same places in any order', () => {
    expect(samePlaces(['a', 'b'], ['b', 'a'])).toBe(true);
    expect(samePlaces(['a'], ['a', 'b'])).toBe(false);
  });
});

describe('inherited entries', () => {
  test("an app's come from its style, then everywhere, without its own", () => {
    const groups = inheritedForApp([
      resolved('own', 'app', 'dev.zed.Zed'),
      resolved('shared', 'app', 'dev.zed.Zed'),
      resolved('shared', 'style', 'casual'),
      resolved('LGTM', 'style', 'casual'),
      resolved('Kubernetes', 'global'),
      resolved('pytest', 'global'),
      resolved('uv', 'global'),
    ]);
    expect(
      groups.map((g) => [g.source, g.scopeId, g.entries.map((e) => e.written), g.preview]),
    ).toEqual([
      ['style', 'casual', ['LGTM'], 'LGTM'],
      ['global', null, ['Kubernetes', 'pytest', 'uv'], 'Kubernetes, pytest, …'],
    ]);
  });

  test('nothing inherited is no groups', () => {
    expect(inheritedForApp(undefined)).toEqual([]);
    expect(inheritedForApp([resolved('own', 'app', 'x')])).toEqual([]);
    expect(
      inheritedForStyle([entry('a', [style('casual')])], { kind: 'style', styleId: 'casual' }),
    ).toEqual([]);
  });

  test("a style's come from everywhere", () => {
    const groups = inheritedForStyle(
      [
        entry('a', [GLOBAL], '2026-09-01T00:00:00'),
        entry('b', [GLOBAL], '2026-09-02T00:00:00'),
        entry('c', [style('casual')]),
      ],
      { kind: 'style', styleId: 'casual' },
    );
    expect(groups).toHaveLength(1);
    expect(groups[0].source).toBe('global');
    expect(groups[0].preview).toBe('b, a');
  });

  test('a word replaced by a more specific entry is not inherited', () => {
    const replaced = { ...resolved('everywhere', 'global'), overridden: true };
    expect(inheritedForApp([resolved('own', 'app', 'x'), replaced])).toEqual([]);

    const groups = inheritedForStyle(
      [
        entry('Voicebox', [GLOBAL], '2026-09-01T00:00:00', 'voice box'),
        entry('Tailscale', [GLOBAL]),
        entry('VoiceBox', [style('code')], '2026-09-02T00:00:00', 'Voice  Box'),
      ],
      { kind: 'style', styleId: 'code' },
    );
    expect(groups[0].entries.map((e) => e.written)).toEqual(['Tailscale']);
  });

  test('preview shows at most two words', () => {
    expect(previewWritten([])).toBe('');
    expect(previewWritten([{ written: 'a' }])).toBe('a');
    expect(previewWritten([{ written: 'a' }, { written: 'b' }, { written: 'c' }])).toBe('a, b, …');
  });
});

test('code-looking text has _, . or / and no spaces', () => {
  expect(looksLikeCode('snake_case')).toBe(true);
  expect(looksLikeCode('voicebox.sh')).toBe(true);
  expect(looksLikeCode('src/app')).toBe(true);
  expect(looksLikeCode('VoiceBox')).toBe(false);
  expect(looksLikeCode('Dr. Smith')).toBe(false);
});

describe('entry age', () => {
  const now = new Date('2026-09-29T12:00:00Z');
  test('entries from the last day count minutes and hours', () => {
    expect(entryAge('2026-09-29T11:59:40', now)).toEqual({ unit: 'now' });
    expect(entryAge('2026-09-29T11:58:00', now)).toEqual({ unit: 'minutes', count: 2 });
    expect(entryAge('2026-09-29T09:00:00', now)).toEqual({ unit: 'hours', count: 3 });
    expect(entryAge('2026-09-28T13:00:00', now)).toEqual({ unit: 'hours', count: 23 });
  });

  test('older entries show their date', () => {
    const age = entryAge('2026-09-27T12:00:00', now);
    expect(age.unit).toBe('date');
    expect(age.unit === 'date' && age.sameYear).toBe(true);
    const old = entryAge('2025-09-01T12:00:00Z', now);
    expect(old.unit === 'date' && old.sameYear).toBe(false);
  });
});

describe('entry note', () => {
  test('a word added by a spoken fix says so, and one fixed only as spelled says that', () => {
    const word = entry('Meghan');
    expect(entryNote(word)).toBeNull();
    expect(entryNote({ ...word, match_sound: false })).toBe('exact');
    expect(entryNote({ ...word, match_sound: false, source: 'spoken_fix' })).toBe('spokenFix');
    expect(entryNote({ ...word, source: 'correction' })).toBe('correction');
    // Inherited rows carry no flags.
    expect(entryNote({})).toBeNull();
  });
});

describe('phrases', () => {
  test('a phrase keeps the lines of what it writes, and what is said', () => {
    expect(newPhrase(' insert my email ', '\nThanks,\nMorgan\n', [GLOBAL])).toEqual({
      written: 'Thanks,\nMorgan',
      spoken: 'insert my email',
      places: [GLOBAL],
      phrase: true,
    });
  });

  test('words and phrases are told apart', () => {
    const word = entry('Kass');
    const phrase = entry('you@example.com', [GLOBAL], undefined, 'insert my email', true);
    expect([isKind(word, 'words'), isKind(word, 'phrases')]).toEqual([true, false]);
    expect([isKind(phrase, 'words'), isKind(phrase, 'phrases')]).toEqual([false, true]);
  });

  test('inherited groups keep one kind, and drop the groups left empty', () => {
    const groups = inheritedForStyle(
      [
        entry('Kass', [GLOBAL], '2026-09-01T00:00:00'),
        entry('you@example.com', [GLOBAL], '2026-09-02T00:00:00', 'insert my email', true),
      ],
      { kind: 'style', styleId: 'casual' },
    );
    const phrases = inheritedOfKind(groups, 'phrases');
    expect(phrases).toHaveLength(1);
    expect(phrases[0].entries.map((e) => e.written)).toEqual(['you@example.com']);
    expect(phrases[0].preview).toBe('you@example.com');
    expect(inheritedOfKind(inheritedForStyle([entry('Kass')], EVERYWHERE), 'phrases')).toEqual([]);
  });
});
