import { useNavigate, useSearch } from '@tanstack/react-router';
import { ChevronDown, ChevronRight, Loader2, Pencil, Trash2 } from 'lucide-react';
import { useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Toggle } from '@/components/ui/toggle';
import type { DictionaryEntry, DictionaryEntryUpdate } from '@/lib/api/types';
import {
  useAddDictionaryEntry,
  useDeleteDictionaryEntry,
  useDictionary,
  useResolvedDictionary,
  useUpdateDictionaryEntry,
} from '@/lib/hooks/useDictionary';
import { stylesById, useWritingStyles } from '@/lib/hooks/useWritingStyle';
import { cn } from '@/lib/utils/cn';
import {
  Arrow,
  MAX_LENGTH,
  MONO_LABEL,
  PlacesMenu,
  ScopeIcon,
  submitKeys,
  useScopeLabel,
} from './DictionaryControls';
import {
  allOptions,
  buildScopeOptions,
  type EntryAge,
  type EntryKind,
  EVERYWHERE_KEY,
  entriesIn,
  entryAge,
  entryNote,
  entryPlaceKeys,
  type InheritedGroup,
  inheritedForApp,
  inheritedForStyle,
  inheritedOfKind,
  isKind,
  looksLikeCode,
  newEntry,
  newPhrase,
  placeInput,
  placesFromKeys,
  type ScopeOption,
  type ScopeOptions,
  samePlaces,
  togglePlace,
} from './dictionaryScopes';
import {
  PhraseHint,
  PhraseSayField,
  PhraseTextField,
  useFilled,
  usePhraseDictation,
} from './PhraseDictation';

const P = 'dictionary';
/** Matches the server's limit. */
const MAX_ENTRIES = 1000;

/** Said, arrow, written, date, actions: shared by list rows so the columns line up. */
const ROW_GRID =
  'grid grid-cols-[minmax(0,200px)_auto_minmax(0,1fr)_auto_auto] items-center gap-x-3';

/**
 * Dictionary: words dictation should get right, and phrases that write
 * something for you (`?kind=phrases`). The scope list on the left picks
 * where (`?scope=`): everywhere, one writing style or one app. An entry can
 * apply in several places at once.
 */
export function DictionaryPage() {
  const { t } = useTranslation();
  const navigate = useNavigate({ from: '/settings/dictionary' });
  const { scope: scopeParam, kind: kindParam } = useSearch({ from: '/settings/dictionary' });
  const kind: EntryKind = kindParam ?? 'words';
  const styles = useWritingStyles();
  const dictionary = useDictionary();

  if (!styles.data || !dictionary.data) {
    return (
      <div className="py-12 flex justify-center text-muted-foreground">
        <Loader2 className="h-4 w-4 animate-spin" />
      </div>
    );
  }
  const all = dictionary.data.entries;
  const options = buildScopeOptions(styles.data, all);
  // A deleted style or unknown key falls back to everywhere.
  const current = allOptions(options).find((o) => o.key === scopeParam) ?? options.everywhere;
  const go = (key: string, nextKind: EntryKind) =>
    navigate({
      search: {
        scope: key === EVERYWHERE_KEY ? undefined : key,
        kind: nextKind === 'phrases' ? 'phrases' : undefined,
      },
      replace: true,
    });
  const styleNames = new Map([...stylesById(styles.data)].map(([id, s]) => [id, s.name]));

  return (
    <div>
      <header className="mb-5">
        <h2 className="text-lg font-semibold">{t(`${P}.title`)}</h2>
        <p className="mt-1 text-xs text-muted-foreground">{t(`${P}.description`)}</p>
      </header>
      <div className="flex min-h-[420px] overflow-hidden rounded-lg border border-border">
        <ScopeList options={options} current={current} onSelect={(key) => go(key, kind)} />
        <div className="min-w-0 flex-1 px-6 py-5">
          <ScopePane
            key={`${current.key}:${kind}`}
            option={current}
            options={options}
            entries={all}
            styleNames={styleNames}
            kind={kind}
            onKind={(next) => go(current.key, next)}
          />
        </div>
      </div>
    </div>
  );
}

function ScopeList({
  options,
  current,
  onSelect,
}: {
  options: ScopeOptions;
  current: ScopeOption;
  onSelect: (key: string) => void;
}) {
  const { t } = useTranslation();
  const label = useScopeLabel();
  const row = (option: ScopeOption) => {
    const selected = option.key === current.key;
    return (
      <li key={option.key}>
        <button
          type="button"
          onClick={() => onSelect(option.key)}
          aria-current={selected ? 'page' : undefined}
          className={cn(
            'flex h-8 w-full items-center gap-2 rounded-md px-2.5 text-left text-[13px] transition-colors',
            selected
              ? 'bg-accent/10 font-medium text-foreground shadow-[inset_2px_0_0_hsl(var(--accent))]'
              : 'text-muted-foreground hover:bg-muted/60 hover:text-foreground',
          )}
        >
          <ScopeIcon option={option} />
          <span className="min-w-0 flex-1 truncate">{label(option)}</span>
          {option.count > 0 && (
            <span
              className="font-mono text-[11px] tabular-nums text-muted-foreground"
              title={t(`${P}.scope.count`, { count: option.count })}
            >
              {option.count}
            </span>
          )}
        </button>
      </li>
    );
  };
  return (
    <nav
      aria-label={t(`${P}.scope.label`)}
      className="w-[220px] shrink-0 border-r border-border bg-muted/40 px-2 py-3"
    >
      <ul className="space-y-0.5">{row(options.everywhere)}</ul>
      {options.styles.length > 0 && (
        <>
          <h3 className={cn(MONO_LABEL, 'px-2.5 pt-4 pb-1.5')}>{t(`${P}.scope.styles`)}</h3>
          <ul className="space-y-0.5">{options.styles.map(row)}</ul>
        </>
      )}
      {options.apps.length > 0 && (
        <>
          <h3 className={cn(MONO_LABEL, 'px-2.5 pt-4 pb-1.5')}>{t(`${P}.scope.apps`)}</h3>
          <ul className="space-y-0.5">{options.apps.map(row)}</ul>
        </>
      )}
    </nav>
  );
}

/** Words | Phrases, each with how many the scope has. */
function KindSwitch({
  kind,
  counts,
  onKind,
}: {
  kind: EntryKind;
  counts: Record<EntryKind, number>;
  onKind: (kind: EntryKind) => void;
}) {
  const { t } = useTranslation();
  return (
    <div
      role="tablist"
      aria-label={t(`${P}.kind.label`)}
      className="flex shrink-0 rounded-[7px] border border-border bg-muted/40 p-0.5"
    >
      {(['words', 'phrases'] as const).map((each) => {
        const selected = each === kind;
        return (
          <button
            key={each}
            type="button"
            role="tab"
            aria-selected={selected}
            onClick={() => onKind(each)}
            className={cn(
              'flex h-7 items-center gap-1.5 rounded-[5px] px-3 text-xs transition-colors',
              selected
                ? 'bg-muted font-medium text-foreground'
                : 'text-muted-foreground hover:text-foreground',
            )}
          >
            {t(`${P}.kind.${each}`)}
            <span
              className={cn(
                'font-mono text-[11px] tabular-nums',
                selected ? 'text-accent' : 'text-muted-foreground',
              )}
            >
              {counts[each]}
            </span>
          </button>
        );
      })}
    </div>
  );
}

function ScopePane({
  option,
  options,
  entries,
  styleNames,
  kind,
  onKind,
}: {
  option: ScopeOption;
  options: ScopeOptions;
  entries: DictionaryEntry[];
  styleNames: Map<string, string>;
  kind: EntryKind;
  onKind: (kind: EntryKind) => void;
}) {
  const { t } = useTranslation();
  const label = useScopeLabel();
  const inScopeAll = entriesIn(entries, option.scope);
  const inScope = inScopeAll.filter((entry) => isKind(entry, kind));
  const counts = {
    words: inScopeAll.filter((entry) => isKind(entry, 'words')).length,
    phrases: inScopeAll.filter((entry) => isKind(entry, 'phrases')).length,
  };
  const name = label(option);
  const phrases = kind === 'phrases';

  let subtitle: string | null;
  if (option.scope.kind === 'global') subtitle = t(`${P}.header.everywhere`);
  else if (option.scope.kind === 'style')
    subtitle = t(`${P}.header.styleApps`, { count: option.appCount ?? 0 });
  else {
    const style = option.styleId ? styleNames.get(option.styleId) : undefined;
    subtitle = style ? t(`${P}.header.appStyle`, { style }) : null;
  }

  return (
    <>
      <div className="mb-5 flex items-center gap-2.5">
        {option.scope.kind === 'app' && <ScopeIcon option={option} className="size-7" />}
        <div className="min-w-0 flex-1">
          <h3 className="truncate text-[15px] font-semibold">{name}</h3>
          {subtitle && <p className="mt-0.5 text-xs text-muted-foreground">{subtitle}</p>}
        </div>
        <KindSwitch kind={kind} counts={counts} onKind={onKind} />
      </div>

      {phrases ? (
        <PhraseAddForm option={option} full={entries.length >= MAX_ENTRIES} />
      ) : (
        <AddForm option={option} full={entries.length >= MAX_ENTRIES} />
      )}

      <div className="mt-5 overflow-hidden rounded-md border border-border">
        <div className="flex items-center justify-between border-b border-border bg-muted/40 px-3 py-2">
          <span className={MONO_LABEL}>
            {t(phrases ? `${P}.phrases.listTitle` : `${P}.list.title`, {
              scope: name,
              count: inScope.length,
            })}
          </span>
          <span className={MONO_LABEL}>{t(`${P}.list.order`)}</span>
        </div>
        {inScope.length ? (
          <ul className="divide-y divide-border/70">
            {inScope.map((entry) => (
              <EntryRow key={entry.id} entry={entry} option={option} options={options} />
            ))}
          </ul>
        ) : (
          <p className="px-3 py-4 text-xs leading-relaxed text-muted-foreground">
            {t(phrases ? `${P}.phrases.empty` : `${P}.list.empty`)}
          </p>
        )}
      </div>

      {option.scope.kind === 'app' && (
        <AppInherited
          bundleId={option.scope.bundleId}
          scopeName={name}
          styleNames={styleNames}
          kind={kind}
        />
      )}
      {option.scope.kind === 'style' && (
        <Inherited
          scopeName={name}
          groups={inheritedOfKind(inheritedForStyle(entries, option.scope), kind)}
          styleNames={styleNames}
        />
      )}
    </>
  );
}

/** Enter submits, Escape cancels. */
function AddForm({ option, full }: { option: ScopeOption; full: boolean }) {
  const { t } = useTranslation();
  const add = useAddDictionaryEntry();
  const [written, setWritten] = useState('');
  const [spoken, setSpoken] = useState('');

  const submit = () => {
    if (!written.trim() || full || add.isPending) return;
    const appName = option.scope.kind === 'app' ? option.name : null;
    add.mutate(newEntry(option.scope, written, spoken, appName), {
      onSuccess: () => {
        setWritten('');
        setSpoken('');
      },
    });
  };
  // A stale error goes once the user changes what they typed.
  const edit = (set: (value: string) => void) => (value: string) => {
    if (add.error) add.reset();
    set(value);
  };
  const onKeyDown = submitKeys(submit);

  return (
    <div>
      <div className="grid grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)_auto] items-end gap-x-3 gap-y-1.5">
        <label htmlFor="dictionary-say" className="text-xs text-muted-foreground">
          {t(`${P}.add.say`)}
        </label>
        <span />
        <label htmlFor="dictionary-write" className="text-xs text-muted-foreground">
          {t(`${P}.add.write`)}
        </label>
        <span />
        <Input
          id="dictionary-say"
          value={spoken}
          onChange={(e) => edit(setSpoken)(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder={t(`${P}.add.sayPlaceholder`)}
          maxLength={MAX_LENGTH}
          className="h-8"
        />
        <div className="flex h-8 items-center">
          <Arrow amber />
        </div>
        <Input
          id="dictionary-write"
          value={written}
          onChange={(e) => edit(setWritten)(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder={t(`${P}.add.writePlaceholder`)}
          maxLength={MAX_LENGTH}
          className="h-8"
        />
        <Button size="sm" disabled={!written.trim() || full || add.isPending} onClick={submit}>
          {t(`${P}.add.action`)}
        </Button>
      </div>
      <p className="mt-2 text-xs text-muted-foreground">{t(`${P}.add.hint`)}</p>
      {full ? (
        <p className="mt-1.5 text-xs text-muted-foreground">
          {t(`${P}.add.limit`, { count: MAX_ENTRIES })}
        </p>
      ) : add.error ? (
        <p className="mt-1.5 text-xs text-destructive">{add.error.message}</p>
      ) : null}
    </div>
  );
}

/**
 * Adds a phrase: what you say, typed or said (the mic, or the shortcut
 * while no other field is focused), and the text it writes, exactly, line
 * breaks and all. ⏎ adds from "When I say"; in "Write exactly", ⏎ starts a
 * new line and ⌘⏎ adds.
 */
function PhraseAddForm({ option, full }: { option: ScopeOption; full: boolean }) {
  const { t } = useTranslation();
  const add = useAddDictionaryEntry();
  const [spoken, setSpoken] = useState('');
  const [written, setWritten] = useState('');
  const say = useRef<HTMLInputElement>(null);
  const [filled, markFilled] = useFilled();
  const dictation = usePhraseDictation(say, (phrase) => {
    if (add.error) add.reset();
    setSpoken(phrase);
    markFilled();
  });
  const ready = !!spoken.trim() && !!written.trim() && !full && !add.isPending;

  const submit = () => {
    if (!ready) return;
    const appName = option.scope.kind === 'app' ? option.name : null;
    add.mutate(newPhrase(spoken, written, [placeInput(option.scope, appName)]), {
      onSuccess: () => {
        setSpoken('');
        setWritten('');
      },
    });
  };
  const edit = (set: (value: string) => void) => (value: string) => {
    if (add.error) add.reset();
    set(value);
  };

  return (
    <div>
      <div className="grid grid-cols-[minmax(0,1fr)_auto_minmax(0,1.25fr)_auto] items-start gap-x-3 gap-y-1.5">
        <label htmlFor="dictionary-phrase-say" className="text-xs text-muted-foreground">
          {t(`${P}.add.say`)}
        </label>
        <span />
        <label htmlFor="dictionary-phrase-write" className="text-xs text-muted-foreground">
          {t(`${P}.phrases.write`)}
        </label>
        <span />
        <PhraseSayField
          id="dictionary-phrase-say"
          value={spoken}
          onChange={edit(setSpoken)}
          onKeyDown={submitKeys(submit)}
          inputRef={say}
          dictation={dictation}
          filled={filled}
          size="sm"
        />
        <div className="flex h-8 items-center">
          <Arrow amber />
        </div>
        <PhraseTextField
          id="dictionary-phrase-write"
          value={written}
          onChange={edit(setWritten)}
          onSubmit={submit}
        />
        <Button size="sm" disabled={!ready} onClick={submit}>
          {t(`${P}.add.action`)}
        </Button>
      </div>
      <PhraseHint dictation={dictation} className="mt-2" />
      {full ? (
        <p className="mt-1.5 text-xs text-muted-foreground">
          {t(`${P}.add.limit`, { count: MAX_ENTRIES })}
        </p>
      ) : add.error ? (
        <p className="mt-1.5 text-xs text-destructive">{add.error.message}</p>
      ) : null}
    </div>
  );
}

function AgeText({ createdAt }: { createdAt: string }) {
  const { t, i18n } = useTranslation();
  const age: EntryAge = entryAge(createdAt);
  let text: string;
  if (age.unit === 'date') {
    text = age.date.toLocaleDateString(i18n.language, {
      month: 'short',
      day: 'numeric',
      ...(age.sameYear ? {} : { year: 'numeric' }),
    });
  } else if (age.unit === 'now') {
    text = t(`${P}.list.justNow`);
  } else {
    text = t(`${P}.list.${age.unit === 'minutes' ? 'minutesAgo' : 'hoursAgo'}`, {
      count: age.count,
    });
  }
  return (
    <span className="whitespace-nowrap text-right text-xs tabular-nums text-muted-foreground">
      {text}
    </span>
  );
}

function SaidText({ spoken }: { spoken: string | null }) {
  const { t } = useTranslation();
  if (!spoken) {
    return (
      <span className="truncate text-xs italic text-muted-foreground">
        {t(`${P}.list.spellingOnly`)}
      </span>
    );
  }
  return <span className="truncate text-sm">{spoken}</span>;
}

function WrittenText({
  entry,
}: {
  entry: Pick<DictionaryEntry, 'written'> &
    Partial<Pick<DictionaryEntry, 'match_sound' | 'source' | 'phrase'>>;
}) {
  const { t } = useTranslation();
  if (entry.phrase) {
    // Shown as it is written: its lines, and its spaces.
    return (
      <span
        className={cn(
          'line-clamp-3 whitespace-pre-line break-words text-sm leading-normal',
          looksLikeCode(entry.written) && 'font-mono text-[13px]',
        )}
      >
        {entry.written}
      </span>
    );
  }
  const note = entryNote(entry);
  return (
    <span className="flex min-w-0 items-baseline gap-2">
      <span
        className={cn('truncate text-sm', looksLikeCode(entry.written) && 'font-mono text-[13px]')}
      >
        {entry.written}
      </span>
      {note && (
        <span
          className="shrink-0 whitespace-nowrap text-xs italic text-muted-foreground"
          title={t(`${P}.list.note.${note}Hint`)}
        >
          {t(`${P}.list.note.${note}`)}
        </span>
      )}
    </span>
  );
}

const ICON_BUTTON = 'h-7 w-7 text-muted-foreground [&_svg]:size-3.5';

function EntryRow({
  entry,
  option,
  options,
}: {
  entry: DictionaryEntry;
  option: ScopeOption;
  options: ScopeOptions;
}) {
  const { t } = useTranslation();
  const [mode, setMode] = useState<'view' | 'edit' | 'confirm'>('view');
  const remove = useDeleteDictionaryEntry();

  if (mode === 'edit') {
    return (
      <EditEntryRow
        entry={entry}
        option={option}
        options={options}
        onDone={() => setMode('view')}
      />
    );
  }
  const onDelete = () => {
    if (entryPlaceKeys(entry).length > 1) setMode('confirm');
    else remove.mutate(entry.id);
  };

  return (
    <li>
      <div className={cn(ROW_GRID, 'px-3 py-2', entry.phrase && 'items-start')}>
        <SaidText spoken={entry.spoken} />
        <Arrow />
        <WrittenText entry={entry} />
        <AgeText createdAt={entry.created_at} />
        <div className={cn('flex', entry.phrase && '-mt-1')}>
          <Button
            size="icon"
            variant="ghost"
            className={ICON_BUTTON}
            aria-label={t(`${P}.list.edit`, { written: entry.written })}
            onClick={() => setMode('edit')}
          >
            <Pencil />
          </Button>
          <Button
            size="icon"
            variant="ghost"
            className={ICON_BUTTON}
            aria-label={t(`${P}.list.delete`, { written: entry.written })}
            disabled={remove.isPending}
            onClick={onDelete}
          >
            <Trash2 />
          </Button>
        </div>
      </div>
      {mode === 'confirm' && (
        <div className="flex items-center gap-2 border-t border-border/70 bg-destructive/5 px-3 py-2">
          <span className="flex-1 text-xs">{t(`${P}.list.confirmDelete`)}</span>
          <Button size="sm" variant="ghost" className="h-7" onClick={() => setMode('view')}>
            {t(`${P}.list.cancel`)}
          </Button>
          <Button
            size="sm"
            variant="destructive"
            className="h-7"
            disabled={remove.isPending}
            onClick={() => remove.mutate(entry.id, { onSuccess: () => setMode('view') })}
          >
            {t(`${P}.list.remove`)}
          </Button>
        </div>
      )}
    </li>
  );
}

function EditEntryRow({
  entry,
  option,
  options,
  onDone,
}: {
  entry: DictionaryEntry;
  option: ScopeOption;
  options: ScopeOptions;
  onDone: () => void;
}) {
  const { t } = useTranslation();
  const update = useUpdateDictionaryEntry();
  const [written, setWritten] = useState(entry.written);
  const [spoken, setSpoken] = useState(entry.spoken ?? '');
  const initialPlaces = entryPlaceKeys(entry);
  const [places, setPlaces] = useState(initialPlaces);
  const initialMatchSound = entry.match_sound !== false;
  const [matchSound, setMatchSound] = useState(initialMatchSound);
  const canSave =
    !!written.trim() &&
    places.length > 0 &&
    !update.isPending &&
    (!entry.phrase || !!spoken.trim());

  const save = () => {
    if (!canSave) return;
    const nextWritten = written.trim();
    const nextSpoken = spoken.trim() || null;
    const patch: DictionaryEntryUpdate = {};
    if (nextWritten !== entry.written) patch.written = nextWritten;
    if (nextSpoken !== entry.spoken) patch.spoken = nextSpoken;
    if (!samePlaces(places, initialPlaces)) patch.places = placesFromKeys(places, options);
    if (matchSound !== initialMatchSound) patch.match_sound = matchSound;
    if (!Object.keys(patch).length) return onDone();
    update.mutate({ id: entry.id, patch }, { onSuccess: onDone });
  };
  const onKeyDown = submitKeys(save, onDone);
  const edit = (set: (value: string) => void) => (value: string) => {
    if (update.error) update.reset();
    set(value);
  };

  return (
    <li>
      <div className={cn(ROW_GRID, 'px-3 py-2', entry.phrase && 'items-start')}>
        <Input
          value={spoken}
          onChange={(e) => edit(setSpoken)(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder={t(`${P}.add.say`)}
          aria-label={t(`${P}.add.say`)}
          maxLength={MAX_LENGTH}
          className="h-7"
        />
        <Arrow />
        {entry.phrase ? (
          <PhraseTextField
            value={written}
            onChange={edit(setWritten)}
            onSubmit={save}
            onCancel={onDone}
            autoFocus
          />
        ) : (
          <Input
            value={written}
            onChange={(e) => edit(setWritten)(e.target.value)}
            onKeyDown={onKeyDown}
            placeholder={t(`${P}.add.write`)}
            aria-label={t(`${P}.add.write`)}
            maxLength={MAX_LENGTH}
            autoFocus
            className="h-7"
          />
        )}
        <span />
        <span className="w-14" />
      </div>
      <div className="border-t border-border/70 bg-accent/5 px-3 py-2">
        <div className="flex items-center gap-2">
          <span className="text-xs text-muted-foreground">{t(`${P}.list.appliesIn`)}</span>
          <PlacesMenu
            selected={places}
            onToggle={(key) => {
              if (update.error) update.reset();
              setPlaces((current) => togglePlace(current, key));
            }}
            options={options}
            viewedFrom={option}
          />
          <span className="flex-1" />
          <Button size="sm" variant="ghost" className="h-7" onClick={onDone}>
            {t(`${P}.list.cancel`)}
          </Button>
          <Button size="sm" className="h-7" disabled={!canSave} onClick={save}>
            {t(`${P}.list.save`)}
          </Button>
        </div>
        {!entry.phrase && (
          <div className="mt-2 flex items-center gap-2" title={t(`${P}.list.matchSoundHint`)}>
            <Toggle
              id={`dictionary-match-sound-${entry.id}`}
              checked={matchSound}
              onCheckedChange={(checked) => {
                if (update.error) update.reset();
                setMatchSound(checked);
              }}
            />
            <label
              htmlFor={`dictionary-match-sound-${entry.id}`}
              className="cursor-pointer text-xs text-muted-foreground"
            >
              {t(`${P}.list.matchSound`)}
            </label>
          </div>
        )}
        {update.error && <p className="mt-1.5 text-xs text-destructive">{update.error.message}</p>}
      </div>
    </li>
  );
}

function AppInherited({
  bundleId,
  scopeName,
  styleNames,
  kind,
}: {
  bundleId: string;
  scopeName: string;
  styleNames: Map<string, string>;
  kind: EntryKind;
}) {
  const { t } = useTranslation();
  const resolved = useResolvedDictionary(bundleId);
  const dropped = resolved.data?.dropped_terms.length ?? 0;
  return (
    <>
      <Inherited
        scopeName={scopeName}
        groups={inheritedOfKind(inheritedForApp(resolved.data?.entries), kind)}
        styleNames={styleNames}
      />
      {dropped > 0 && kind === 'words' && (
        <p className="mt-3 text-xs leading-relaxed text-muted-foreground">
          {t(`${P}.inherited.dropped`, { count: dropped })}
        </p>
      )}
    </>
  );
}

/** Read-only: what also applies in a style or app from elsewhere, one collapsible row per source. */
function Inherited({
  scopeName,
  groups,
  styleNames,
}: {
  scopeName: string;
  groups: InheritedGroup[];
  styleNames: Map<string, string>;
}) {
  const { t } = useTranslation();
  if (!groups.length) return null;
  const source = (group: InheritedGroup) =>
    group.source === 'style'
      ? (styleNames.get(group.scopeId ?? '') ?? t(`${P}.scope.styles`))
      : t(`${P}.scope.everywhere`);
  return (
    <section className="mt-6">
      <h4 className={cn(MONO_LABEL, 'mb-2')}>{t(`${P}.inherited.title`, { scope: scopeName })}</h4>
      <div className="divide-y divide-border/70 overflow-hidden rounded-md border border-border">
        {groups.map((group) => (
          <InheritedGroupRow
            key={`${group.source}:${group.scopeId}`}
            group={group}
            source={source(group)}
          />
        ))}
      </div>
    </section>
  );
}

function InheritedGroupRow({ group, source }: { group: InheritedGroup; source: string }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const Chevron = open ? ChevronDown : ChevronRight;
  return (
    <div>
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
        className="flex w-full items-center gap-2 px-3 py-2 text-left hover:bg-muted/40"
      >
        <Chevron className="size-3.5 shrink-0 text-muted-foreground" />
        <span className="shrink-0 text-[13px]">{t(`${P}.inherited.from`, { source })}</span>
        <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground">
          {group.preview}
        </span>
        <span className="font-mono text-[11px] tabular-nums text-muted-foreground">
          {group.entries.length}
        </span>
      </button>
      {open && (
        <ul className="divide-y divide-border/50 border-t border-border/70 bg-muted/20">
          {group.entries.map((entry) => (
            <li
              key={entry.id}
              className={cn(ROW_GRID, 'px-3 py-1.5 pl-8', entry.phrase && 'items-start')}
            >
              <SaidText spoken={entry.spoken} />
              <Arrow />
              <WrittenText entry={entry} />
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
