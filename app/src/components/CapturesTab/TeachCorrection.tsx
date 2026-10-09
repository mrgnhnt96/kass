import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';
import { BookPlus, Check, MessageSquareQuote } from 'lucide-react';
import { type ReactNode, useId, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Kbd } from '@/components/ui/kbd';
import { useToast } from '@/components/ui/use-toast';
import { PERSONAL_EXAMPLES_KEY } from '@/components/WritingStyle/PersonalExamples';
import { apiClient } from '@/lib/api/client';
import type { CaptureFeedbackResponse, CaptureResponse } from '@/lib/api/types';
import { useAddDictionaryEntry } from '@/lib/hooks/useDictionary';
import { useWritingStyle, WRITING_STYLE_KEY } from '@/lib/hooks/useWritingStyle';
import { cn } from '@/lib/utils/cn';
import type { DictionaryWord } from './AddToDictionary';
import {
  dictionaryWord,
  type PhraseDraft,
  phraseFromHunk,
  spellingEntry,
} from './captureDictionary';
import { MakePhraseDialog } from './MakePhrase';
import { type DiffHunk, diffWords } from './wordDiff';

export type TeachTarget = 'raw' | 'refined';

const MAX_HUNKS_SHOWN = 3;

/**
 * State for teaching one transcript of a capture: the draft of what the user
 * meant, the optional note, saving it as a correction, and undoing it or
 * removing any of the capture's corrections.
 *
 * Corrections stack: each round of edits is saved as its own correction,
 * made from the newest one before it and holding the whole text, so the
 * newest is the text as corrected so far (what learning uses). `reports`
 * is the capture's corrections, newest first.
 */
export function useTeachCorrection(
  capture: CaptureResponse,
  target: TeachTarget,
  original: string,
  reports: CaptureFeedbackResponse[] | undefined,
) {
  const { t } = useTranslation();
  const { toast } = useToast();
  const queryClient = useQueryClient();
  const [draft, setDraft] = useState<string | null>(null);
  const [notes, setNotes] = useState('');
  const [learned, setLearned] = useState<CaptureFeedbackResponse | null>(null);

  const invalidate = () => {
    queryClient.invalidateQueries({ queryKey: ['capture-feedback', capture.id] });
    // Refined-output corrections also teach the writing style.
    queryClient.invalidateQueries({ queryKey: WRITING_STYLE_KEY });
    queryClient.invalidateQueries({ queryKey: PERSONAL_EXAMPLES_KEY });
  };

  const save = useMutation({
    mutationFn: ({
      before: _,
      ...body
    }: {
      expected_text: string;
      notes: string;
      replaces?: string;
      /** The text as shown before this fix. */
      before: string;
    }) => apiClient.reportCaptureOutput(capture.id, { snapshot: capture, target, ...body }),
    onSuccess: (report, body) => {
      // Fixes the text where Kass just wrote it too, if it's still as Kass
      // left it there (docs/plans/CORRECTIONS_IN_PLACE.md). Silent unless
      // it changed.
      invoke<string | null>('apply_correction', {
        captureId: capture.id,
        before: body.before,
        after: body.expected_text,
      })
        .then((app) => {
          if (app === null) return;
          toast({
            title: app ? t('captures.teach.updatedIn', { app }) : t('captures.teach.updatedInApp'),
          });
        })
        .catch(() => {});
      // Shown at once, before the list is fetched again.
      queryClient.setQueryData<CaptureFeedbackResponse[]>(
        ['capture-feedback', capture.id],
        (old) => old && [report, ...old.filter((r) => r.id !== body.replaces)],
      );
      setLearned(report);
      setDraft(null);
      setNotes('');
      invalidate();
    },
    onError: (error: Error) =>
      toast({
        title: t('captures.feedback.failed'),
        description: error.message,
        variant: 'destructive',
      }),
  });

  // Undo withdraws the report, and with it everything it taught: the
  // writing-style example, habits, names and learned rules. Any report of
  // the capture can be withdrawn this way, a voice edit's included.
  const undo = useMutation({
    mutationFn: (report: CaptureFeedbackResponse) =>
      apiClient.withdrawCaptureReport(capture.id, report.id),
    onSuccess: (_, report) => {
      setLearned((current) => (current?.id === report.id ? null : current));
      invalidate();
    },
    onError: (error: Error) =>
      toast({
        title: t('captures.teach.undoFailed'),
        description: error.message,
        variant: 'destructive',
      }),
  });

  /** This transcript's corrections, newest first. */
  const rounds = useMemo(
    () => (reports ?? []).filter((report) => report.target === target),
    [reports, target],
  );
  /** The newest correction: the text as corrected so far. */
  const saved = rounds[0] ?? null;
  /** The text an edit starts from: the newest correction, else Kass's. */
  const base = saved?.expected_text ?? original;
  // Exact, so adding or removing a line break at either end counts as a fix.
  // Back to Kass's own text isn't a fix; Undo takes corrections back.
  const changed = draft !== null && draft !== base && draft !== original;
  /** Saves `text` as a new round, or amends the round `replaces` names. */
  const submit = (text: string, note: string, replaces?: string) => {
    if (text === original || text === base || save.isPending) return;
    save.mutate(
      replaces
        ? { expected_text: text, notes: note, replaces, before: base }
        : { expected_text: text, notes: note, before: base },
    );
  };
  /** The text as it reads now: the edit, else the saved correction, else Kass's. */
  const current = draft ?? base;

  return {
    target,
    original,
    draft,
    notes,
    changed,
    learned,
    saved,
    base,
    /** The text before `report`'s round: the round under it, else Kass's. */
    before: (report: CaptureFeedbackResponse) => {
      const at = rounds.findIndex((r) => r.id === report.id);
      return (at >= 0 ? rounds[at + 1]?.expected_text : undefined) ?? original;
    },
    saving: save.isPending,
    undoing: undo.isPending,
    /** The report being undone or removed, while it is. */
    removingId: undo.isPending ? (undo.variables?.id ?? null) : null,
    /** Starts editing from the current text, so the user fixes it in place. */
    begin: () => setDraft((d) => d ?? base),
    setDraft,
    setNotes,
    cancel: () => {
      setDraft(null);
      setNotes('');
    },
    /** Drops an untouched draft when focus leaves, back to the plain text. */
    settle: () => {
      if (!changed && !notes) setDraft(null);
    },
    save: () => {
      if (draft !== null) submit(draft, notes);
    },
    current,
    /**
     * Saves `text` as the fix, showing it in the edit box while it saves.
     * While editing, it saves that round; otherwise, with a correction
     * already saved, it amends the newest one, keeping its note.
     */
    saveText: (text: string) => {
      if (text === original || text === base || save.isPending) return;
      setDraft(text);
      if (draft === null && saved) submit(text, saved.notes, saved.id);
      else submit(text, notes);
    },
    /** Takes back the newest round, the one the card shows. */
    undo: () => saved && undo.mutate(saved),
    /** Withdraws one of the capture's reports and everything it taught. */
    remove: (report: CaptureFeedbackResponse) => undo.mutate(report),
  };
}

export type TeachState = ReturnType<typeof useTeachCorrection>;

/** A small action beside a change; it keeps the edit open while the pointer is down. */
function HunkAction({
  icon,
  label,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
}) {
  return (
    <Button
      variant="ghost"
      size="sm"
      // Keeps the edit open: the text losing focus first would settle it.
      onMouseDown={(event) => event.preventDefault()}
      onClick={onClick}
      className="ml-2 h-6 gap-1 px-1.5 align-middle font-sans text-xs text-muted-foreground hover:text-foreground"
    >
      {icon}
      {label}
    </Button>
  );
}

/**
 * "post grass → Postgres" for each place the text changed. With `onAdd`,
 * a change that can be a dictionary entry offers to add it; with
 * `onPhrase`, words replaced by other text offer to become a phrase.
 */
export function HunkList({
  hunks,
  className,
  onAdd,
  onPhrase,
}: {
  hunks: DiffHunk[];
  className?: string;
  onAdd?: (word: DictionaryWord) => void;
  onPhrase?: (phrase: PhraseDraft) => void;
}) {
  const { t } = useTranslation();
  const shown = hunks.slice(0, MAX_HUNKS_SHOWN);
  return (
    <ul className={className}>
      {shown.map((hunk, i) => (
        // Hunks have no identity beyond their order in this diff.
        // biome-ignore lint/suspicious/noArrayIndexKey: order is the identity
        <li key={i} className="font-mono text-xs leading-relaxed">
          {hunk.removed && <span className="text-destructive line-through">{hunk.removed}</span>}
          {hunk.removed && hunk.added && <span className="text-muted-foreground"> → </span>}
          {hunk.added && <span className="text-success">{hunk.added}</span>}
          {hunk.count && <span className="text-muted-foreground"> ×{hunk.count}</span>}
          {onAdd && dictionaryWord(hunk) && (
            <HunkAction
              icon={<BookPlus className="size-3.5!" />}
              label={t('captures.dictionary.add')}
              onClick={() => {
                const word = dictionaryWord(hunk);
                if (word) onAdd(word);
              }}
            />
          )}
          {onPhrase && phraseFromHunk(hunk) && (
            <HunkAction
              icon={<MessageSquareQuote className="size-3.5!" />}
              label={t('captures.phrase.make')}
              onClick={() => {
                const phrase = phraseFromHunk(hunk);
                if (phrase) onPhrase(phrase);
              }}
            />
          )}
        </li>
      ))}
      {hunks.length > shown.length && (
        <li className="font-mono text-xs text-muted-foreground">
          {t('captures.teach.moreChanges', { count: hunks.length - shown.length })}
        </li>
      )}
    </ul>
  );
}

function onTeachKeyDown(teach: TeachState) {
  return (event: React.KeyboardEvent) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      teach.cancel();
      (event.target as HTMLElement).blur();
    } else if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing) {
      event.preventDefault();
      teach.save();
    }
  };
}

/**
 * The transcript itself, editable in place: click the text and fix it. It
 * looks like the text it replaces and grows with it. ⏎ saves the fix, ⇧⏎
 * adds a line, esc cancels, and leaving an unchanged edit puts the text back.
 */
export function EditableTranscript({
  teach,
  className,
  children,
}: {
  teach: TeachState;
  className: string;
  /** The text as shown when not editing, with its highlights. */
  children: ReactNode;
}) {
  const { t } = useTranslation();
  const field = useRef<HTMLTextAreaElement>(null);
  const editing = teach.draft !== null;

  // Grow with the text; field-sizing isn't in every WebKit this ships on.
  // biome-ignore lint/correctness/useExhaustiveDependencies: resize when the draft changes
  useLayoutEffect(() => {
    const element = field.current;
    if (!element) return;
    element.style.height = 'auto';
    element.style.height = `${element.scrollHeight}px`;
  }, [teach.draft]);

  const surface = 'm-0 -mx-1.5 -my-1 rounded-md px-1.5 py-1 whitespace-pre-wrap break-words';
  if (!editing) {
    // Not a <button>: WebKit can't select text inside one, and selecting a
    // word offers to add it to the dictionary. A click that ends a
    // selection selected; it doesn't start editing.
    const edit = () => {
      if (!window.getSelection()?.isCollapsed) return;
      teach.begin();
    };
    return (
      // biome-ignore lint/a11y/useSemanticElements: a <button> can't hold a text selection
      <div
        role="button"
        tabIndex={0}
        data-transcript
        title={t('captures.teach.editHint')}
        aria-label={t('captures.teach.editHint')}
        onClick={edit}
        onKeyDown={(event) => {
          if (event.key === 'Enter' || event.key === ' ') {
            event.preventDefault();
            teach.begin();
          }
        }}
        className={cn(
          surface,
          className,
          'block w-[calc(100%+0.75rem)] text-left cursor-text select-text hover:bg-foreground/[0.04] focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring',
        )}
      >
        {children}
      </div>
    );
  }
  return (
    <textarea
      ref={field}
      // biome-ignore lint/a11y/noAutofocus: opened by clicking the text, which the user expects to edit
      autoFocus
      onFocus={(event) => {
        const end = event.target.value.length;
        event.target.setSelectionRange(end, end);
      }}
      rows={1}
      value={teach.draft ?? ''}
      aria-label={t('captures.teach.editHint')}
      maxLength={100000}
      disabled={teach.saving}
      onBlur={teach.settle}
      onChange={(event) => teach.setDraft(event.target.value)}
      onKeyDown={onTeachKeyDown(teach)}
      className={cn(
        surface,
        className,
        'block w-[calc(100%+0.75rem)] resize-none overflow-hidden border-0 bg-foreground/[0.04] outline-none ring-1 ring-ring',
      )}
    />
  );
}

/**
 * Under an edited transcript: what this round changed, an optional note and
 * Save. Shown only once the text differs from where the round started. A changed word goes
 * straight into the dictionary in one click, spelled as corrected and
 * applying everywhere, and the correction saves too. Words replaced by
 * other text can become a phrase, which saves the correction too.
 */
export function TeachActions({ teach }: { teach: TeachState }) {
  const { t } = useTranslation();
  const { toast } = useToast();
  const notesId = useId();
  const add = useAddDictionaryEntry();
  const addWord = (word: DictionaryWord) => {
    if (add.isPending) return;
    const body = spellingEntry(word.said, word.written);
    add.mutate(body, {
      onSuccess: () => {
        toast({ title: t('captures.dictionary.added', { written: body.written }) });
        teach.save();
      },
      onError: (error: Error) =>
        toast({
          title: t('captures.dictionary.addFailed'),
          description: error.message,
          variant: 'destructive',
        }),
    });
  };
  const [phrase, setPhrase] = useState<PhraseDraft | null>(null);
  const hunks = useMemo(
    () => (teach.changed && teach.draft !== null ? diffWords(teach.base, teach.draft).hunks : []),
    [teach.changed, teach.draft, teach.base],
  );

  if (teach.draft === '') {
    return <p className="text-xs text-muted-foreground">{t('captures.feedback.emptyHint')}</p>;
  }
  if (!teach.changed) return null;
  return (
    <div className="flex flex-col gap-3.5">
      {hunks.length > 0 && (
        <HunkList hunks={hunks} className="space-y-0.5" onAdd={addWord} onPhrase={setPhrase} />
      )}
      <MakePhraseDialog
        phrase={phrase}
        onAdded={() => teach.save()}
        onClose={() => setPhrase(null)}
      />
      <div className="flex flex-col gap-1.5">
        <label htmlFor={notesId} className="text-xs text-muted-foreground">
          {t('captures.feedback.notes')}
        </label>
        <Input
          id={notesId}
          value={teach.notes}
          maxLength={5000}
          disabled={teach.saving}
          onChange={(event) => teach.setNotes(event.target.value)}
          onKeyDown={onTeachKeyDown(teach)}
          className="h-9 bg-background"
        />
      </div>
      <div className="flex justify-end gap-2">
        {/* Keeps the edit open while the pointer is down, so Cancel and Save
            aren't removed by the text losing focus first. */}
        <Button
          variant="outline"
          size="sm"
          disabled={teach.saving}
          onMouseDown={(event) => event.preventDefault()}
          onClick={teach.cancel}
        >
          {t('common.cancel')}
          <Kbd className="border-0 px-0">esc</Kbd>
        </Button>
        <Button
          size="sm"
          className="font-semibold"
          disabled={teach.saving}
          onMouseDown={(event) => event.preventDefault()}
          onClick={() => teach.save()}
        >
          {t('captures.teach.save')}
          <Kbd className="border-0 px-0 text-accent-foreground">⏎</Kbd>
        </Button>
      </div>
    </div>
  );
}

/**
 * The confirmation after a round of corrections is saved, with what it
 * changed and the learning status. Its Undo is on the correction in the
 * inspector.
 */
export function LearnedNotice({ teach }: { teach: TeachState }) {
  const { t } = useTranslation();
  const { data: style } = useWritingStyle();
  const learning = useQuery({
    queryKey: ['correction-learning'],
    queryFn: () => apiClient.correctionLearningStatus(),
    refetchInterval: (query) => (query.state.data?.model?.running ? 2000 : 60_000),
  });
  // Only while it is still the newest round.
  const report = teach.learned?.id === teach.saved?.id ? teach.learned : null;
  const before = report ? teach.before(report) : '';
  const hunks = useMemo(
    () => (report ? diffWords(before, report.expected_text).hunks : []),
    [report, before],
  );
  if (!report) return null;
  const model = learning.data?.model;
  const examples = style?.example_count ?? 0;

  return (
    <div className="flex flex-col gap-3.5">
      <div
        aria-live="polite"
        className="flex flex-col gap-2.5 rounded-md border border-border bg-background p-3"
      >
        <div className="flex flex-wrap items-center gap-2 text-[13px]">
          <Check className="h-3.5 w-3.5 text-success" strokeWidth={3} />
          {t('captures.teach.learned')}
          {hunks.length > 0 && <HunkList hunks={hunks} className="text-muted-foreground" />}
        </div>
        <p className="text-xs leading-normal text-muted-foreground">
          {t('captures.feedback.saved')}
          {model?.running && ` ${t(`captures.feedback.learning.modelPhases.${model.phase}`)}`}
        </p>
        {model?.running && (
          <div className="h-[3px] overflow-hidden rounded-sm bg-border">
            <div className="h-full w-1/3 rounded-sm bg-accent animate-pulse" />
          </div>
        )}
      </div>
      {examples > 0 && (
        <span className="font-mono text-[11px] text-muted-foreground">
          {t('captures.teach.examplesLearned', { count: examples })}
        </span>
      )}
    </div>
  );
}
