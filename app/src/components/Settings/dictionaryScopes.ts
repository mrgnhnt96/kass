import type {
  DictionaryEntry,
  DictionaryEntryCreate,
  DictionaryPlace,
  DictionaryPlaceInput,
  ResolvedDictionaryEntry,
  WritingStylesResponse,
} from '@/lib/api/types';
import { parseServerDate } from '@/lib/utils/format';

/** A place dictionary entries apply: everywhere, one writing style or one app. */
export type DictionaryScopeRef =
  | { kind: 'global' }
  | { kind: 'style'; styleId: string }
  | { kind: 'app'; bundleId: string };

export const EVERYWHERE: DictionaryScopeRef = { kind: 'global' };
export const EVERYWHERE_KEY = 'global';

/** The scope as one string, for `?scope=` and place lists: `global`, `style:<id>` or `app:<bundle id>`. */
export function scopeKey(scope: DictionaryScopeRef): string {
  if (scope.kind === 'style') return `style:${scope.styleId}`;
  if (scope.kind === 'app') return `app:${scope.bundleId}`;
  return EVERYWHERE_KEY;
}

export function parseScopeKey(key: string | undefined): DictionaryScopeRef {
  if (key?.startsWith('style:') && key.length > 6) return { kind: 'style', styleId: key.slice(6) };
  if (key?.startsWith('app:') && key.length > 4) return { kind: 'app', bundleId: key.slice(4) };
  return EVERYWHERE;
}

export function placeScope(place: Pick<DictionaryPlace, 'scope' | 'scope_id'>): DictionaryScopeRef {
  if (place.scope === 'style' && place.scope_id) return { kind: 'style', styleId: place.scope_id };
  if (place.scope === 'app' && place.scope_id) return { kind: 'app', bundleId: place.scope_id };
  return EVERYWHERE;
}

/** The scope keys of every place an entry applies. */
export function entryPlaceKeys(entry: DictionaryEntry): string[] {
  return [...new Set(entry.places.map((place) => scopeKey(placeScope(place))))];
}

export function appliesIn(entry: DictionaryEntry, scope: DictionaryScopeRef): boolean {
  return entryPlaceKeys(entry).includes(scopeKey(scope));
}

function newestFirst<T extends { created_at: string }>(a: T, b: T): number {
  return parseServerDate(b.created_at).getTime() - parseServerDate(a.created_at).getTime();
}

/** The entries that apply in a scope, newest first. */
export function entriesIn(
  entries: DictionaryEntry[] | undefined,
  scope: DictionaryScopeRef,
): DictionaryEntry[] {
  return (entries ?? []).filter((entry) => appliesIn(entry, scope)).sort(newestFirst);
}

export interface ScopeOption {
  key: string;
  scope: DictionaryScopeRef;
  /** A style's or app's name; empty for everywhere, which the page labels. */
  name: string;
  /** Entries whose places include this scope. */
  count: number;
  /** For an app, the writing style it uses, when the app is still listed. */
  styleId?: string;
  /** For a style, how many apps use it. */
  appCount?: number;
}

export interface ScopeOptions {
  everywhere: ScopeOption;
  styles: ScopeOption[];
  apps: ScopeOption[];
}

/**
 * The scope list: everywhere, then each style in order, then each app by
 * name. An app with entries but no longer in the styles listing still shows,
 * so its entries stay reachable.
 */
export function buildScopeOptions(
  data: WritingStylesResponse | undefined,
  entries: DictionaryEntry[] | undefined,
): ScopeOptions {
  const counts = new Map<string, number>();
  for (const entry of entries ?? []) {
    for (const key of entryPlaceKeys(entry)) counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  const option = (scope: DictionaryScopeRef, name: string): ScopeOption => {
    const key = scopeKey(scope);
    return { key, scope, name, count: counts.get(key) ?? 0 };
  };

  const listedApps = data?.apps ?? [];
  const styles = [...(data?.styles ?? [])]
    .sort((a, b) => a.position - b.position)
    .map((style) => ({
      ...option({ kind: 'style', styleId: style.id }, style.name),
      appCount: listedApps.filter((app) => app.style_id === style.id).length,
    }));

  const apps = new Map<string, { name: string; styleId?: string }>();
  for (const app of listedApps) {
    apps.set(app.bundle_id, { name: app.name || app.bundle_id, styleId: app.style_id });
  }
  for (const entry of entries ?? []) {
    for (const place of entry.places) {
      if (place.scope === 'app' && place.scope_id && !apps.has(place.scope_id)) {
        apps.set(place.scope_id, { name: place.app_name || place.scope_id });
      }
    }
  }
  const appOptions = [...apps]
    .map(([bundleId, app]) => ({
      ...option({ kind: 'app', bundleId }, app.name),
      ...(app.styleId ? { styleId: app.styleId } : {}),
    }))
    .sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: 'base' }));

  return { everywhere: option(EVERYWHERE, ''), styles, apps: appOptions };
}

export function allOptions(options: ScopeOptions): ScopeOption[] {
  return [options.everywhere, ...options.styles, ...options.apps];
}

/** The body for one place: an app carries its name so the server can show it later. */
export function placeInput(
  scope: DictionaryScopeRef,
  appName?: string | null,
): DictionaryPlaceInput {
  if (scope.kind === 'style') return { scope: 'style', scope_id: scope.styleId };
  if (scope.kind === 'app')
    return { scope: 'app', scope_id: scope.bundleId, app_name: appName ?? null };
  return { scope: 'global' };
}

/** The body that adds an entry to one scope; a blank "when I say" teaches only the spelling. */
export function newEntry(
  scope: DictionaryScopeRef,
  written: string,
  spoken: string,
  appName?: string | null,
): DictionaryEntryCreate {
  return {
    written: written.trim(),
    spoken: spoken.trim() || null,
    places: [placeInput(scope, appName)],
  };
}

/**
 * The body that adds a phrase: what is said, and the text it writes with
 * its lines kept. The server tidies each line.
 */
export function newPhrase(
  spoken: string,
  written: string,
  places: DictionaryPlaceInput[],
): DictionaryEntryCreate {
  return { written: written.trim(), spoken: spoken.trim(), places, phrase: true };
}

/** What a row notes beside its word: added by a spoken fix or a correction, or fixed only where spelled exactly. */
export type EntryNote = 'spokenFix' | 'correction' | 'exact' | null;

export function entryNote(
  entry: Partial<Pick<DictionaryEntry, 'match_sound' | 'source'>>,
): EntryNote {
  if (entry.source === 'spoken_fix') return 'spokenFix';
  if (entry.source === 'correction') return 'correction';
  return entry.match_sound === false ? 'exact' : null;
}

/** The place bodies for chosen scope keys, naming apps from the scope list. */
export function placesFromKeys(keys: string[], options: ScopeOptions): DictionaryPlaceInput[] {
  const names = new Map(options.apps.map((o) => [o.key, o.name]));
  return keys.map((key) => placeInput(parseScopeKey(key), names.get(key) ?? null));
}

/**
 * Checking or unchecking one place in the "Applies in" menu. Everywhere
 * clears the rest; anything else clears everywhere. Unchecking the last
 * place leaves none, which the caller refuses to save.
 */
export function togglePlace(selected: string[], key: string): string[] {
  if (selected.includes(key)) return selected.filter((k) => k !== key);
  if (key === EVERYWHERE_KEY) return [EVERYWHERE_KEY];
  return [...selected.filter((k) => k !== EVERYWHERE_KEY), key];
}

export function samePlaces(a: string[], b: string[]): boolean {
  const set = new Set(a);
  return a.length === b.length && b.every((key) => set.has(key));
}

/** Whether written text reads as code (an identifier, path or domain), to show in monospace. */
export function looksLikeCode(text: string): boolean {
  return /[_./]/.test(text) && !/\s/.test(text);
}

/** The first few written words, then "…" when there are more. */
export function previewWritten(entries: { written: string }[], count = 2): string {
  const words = entries.slice(0, count).map((entry) => entry.written);
  return entries.length > count ? `${words.join(', ')}, …` : words.join(', ');
}

export interface InheritedEntry {
  id: string;
  written: string;
  spoken: string | null;
  phrase: boolean;
}

export interface InheritedGroup {
  source: 'style' | 'global';
  /** The style's id, for a style group. */
  scopeId: string | null;
  entries: InheritedEntry[];
  preview: string;
}

function group(
  source: InheritedGroup['source'],
  scopeId: string | null,
  entries: InheritedEntry[],
): InheritedGroup {
  return { source, scopeId, entries, preview: previewWritten(entries) };
}

/**
 * What an app picks up from elsewhere: its style's entries, then
 * everywhere's. An entry that is also the app's own is left out, and so is
 * one a more specific entry replaces there: it doesn't apply in the app.
 * Empty groups are dropped.
 */
export function inheritedForApp(resolved: ResolvedDictionaryEntry[] | undefined): InheritedGroup[] {
  const rows = resolved ?? [];
  const own = new Set(rows.filter((row) => row.scope === 'app').map((row) => row.id));
  const pick = (scope: 'style' | 'global') => {
    const seen = new Set<string>();
    return rows.filter((row) => {
      if (row.scope !== scope || row.overridden || own.has(row.id) || seen.has(row.id))
        return false;
      seen.add(row.id);
      return true;
    });
  };
  const style = pick('style');
  const groups: InheritedGroup[] = [];
  if (style.length) groups.push(group('style', style[0].scope_id, style.map(inheritedEntry)));
  const global = pick('global');
  if (global.length) groups.push(group('global', null, global.map(inheritedEntry)));
  return groups;
}

/** What a style picks up: everywhere's entries, newest first, less those its own replace. */
export function inheritedForStyle(
  entries: DictionaryEntry[] | undefined,
  style: DictionaryScopeRef,
): InheritedGroup[] {
  const own = new Set(entriesIn(entries, style).map(saidKey));
  const global = entriesIn(entries, EVERYWHERE).filter((entry) => !own.has(saidKey(entry)));
  return global.length ? [group('global', null, global.map(inheritedEntry))] : [];
}

function inheritedEntry(entry: InheritedEntry): InheritedEntry {
  return { id: entry.id, written: entry.written, spoken: entry.spoken, phrase: entry.phrase };
}

/** Words or phrases: the Dictionary page shows one kind at a time. */
export type EntryKind = 'words' | 'phrases';

export function isKind(entry: { phrase?: boolean }, kind: EntryKind): boolean {
  return !!entry.phrase === (kind === 'phrases');
}

/** Inherited groups with only `kind`'s entries; groups left empty are dropped. */
export function inheritedOfKind(groups: InheritedGroup[], kind: EntryKind): InheritedGroup[] {
  return groups.flatMap((g) => {
    const entries = g.entries.filter((entry) => isKind(entry, kind));
    return entries.length ? [group(g.source, g.scopeId, entries)] : [];
  });
}

/** How long ago an entry was added: minutes or hours within a day, then its date. */
export type EntryAge =
  | { unit: 'now' }
  | { unit: 'minutes' | 'hours'; count: number }
  | { unit: 'date'; date: Date; sameYear: boolean };

export function entryAge(createdAt: string, now: Date = new Date()): EntryAge {
  const date = parseServerDate(createdAt);
  const minutes = Math.floor((now.getTime() - date.getTime()) / 60_000);
  if (minutes < 1) return { unit: 'now' };
  if (minutes < 60) return { unit: 'minutes', count: minutes };
  if (minutes < 24 * 60) return { unit: 'hours', count: Math.floor(minutes / 60) };
  return { unit: 'date', date, sameYear: date.getFullYear() === now.getFullYear() };
}

/** The words an entry matches on, as the server compares them: what's said, else what's written. */
function saidKey(entry: { written: string; spoken: string | null }): string {
  return (entry.spoken ?? entry.written).trim().split(/\s+/).join(' ').toLowerCase();
}
