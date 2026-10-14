import { useNavigate } from '@tanstack/react-router';
import { ArrowRight, Check, ChevronRight, CircleHelp, Copy, Pencil } from 'lucide-react';
import { type ReactNode, useEffect, useId, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { useToast } from '@/components/ui/use-toast';
import { cn } from '@/lib/utils/cn';
import { type FiredPhrase, markPhrases, phraseKey } from './captureDictionary';
import {
  EditableTranscript,
  LearnedNotice,
  TeachActions,
  type TeachState,
} from './TeachCorrection';
import {
  countWords,
  type DiffSegment,
  diffWords,
  type MergedSegment,
  summarizeChanges,
} from './wordDiff';

const MARK_CLASS: Record<MergedSegment['kind'] | 'corrected', string> = {
  same: '',
  removed: 'bg-destructive/15 text-destructive line-through rounded-sm',
  added: 'bg-accent/20 text-foreground rounded-sm px-0.5',
  corrected: 'bg-success/15 rounded-sm px-0.5',
};

function Marked({ segments, mark }: { segments: DiffSegment[]; mark: 'corrected' }) {
  return (
    <>
      {segments.map((segment, i) => (
        // Segments have no identity beyond their position in the text.
        // biome-ignore lint/suspicious/noArrayIndexKey: position is the identity
        <span key={i} className={segment.changed ? MARK_CLASS[mark] : undefined}>
          {segment.text}
        </span>
      ))}
    </>
  );
}

/**
 * Text a dictionary phrase wrote, underlined; it opens what wrote it and a
 * way to edit the phrase. It doesn't start editing the transcript around it.
 */
function PhraseMark({ text, phrase }: { text: string; phrase: FiredPhrase }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const stop = (event: React.SyntheticEvent) => event.stopPropagation();
  const trigger = (
    // biome-ignore lint/a11y/useSemanticElements: inline in the text, where a <button> would break its lines and selection
    <span
      role="button"
      tabIndex={0}
      onClick={stop}
      onKeyDown={stop}
      title={t('captures.phrase.wroteHint', { spoken: phrase.spoken })}
      className="cursor-pointer rounded-sm underline decoration-accent decoration-dotted decoration-2 underline-offset-[5px] hover:bg-accent/10 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
    >
      {text}
    </span>
  );
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>{trigger}</DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-[280px]" onClick={stop}>
        <DropdownMenuLabel className="flex items-center gap-2 pt-2 pb-1 font-normal">
          <span className="inline-flex h-[18px] shrink-0 items-center whitespace-nowrap rounded-full bg-accent/10 px-1.5 font-mono text-[10px] text-accent">
            {t('captures.phrase.chip')}
          </span>
          <span className="text-xs text-muted-foreground">{t('captures.phrase.from')}</span>
        </DropdownMenuLabel>
        <div className="flex items-start gap-2 px-2 pb-2 text-[13px]">
          <span className="shrink-0">“{phrase.spoken}”</span>
          <ArrowRight className="mt-0.5 size-3.5 shrink-0 text-muted-foreground" />
          <span className="min-w-0 whitespace-pre-line break-words text-muted-foreground">
            {phrase.written}
          </span>
        </div>
        <DropdownMenuSeparator />
        <DropdownMenuItem
          onSelect={() => navigate({ to: '/settings/dictionary', search: { kind: 'phrases' } })}
        >
          {t('captures.phrase.edit')}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** Short text is set large and long text smaller, so the card reads well either way. */
function textSize(words: number): string {
  if (words <= 6) return 'text-[26px] leading-[1.25]';
  if (words <= 25) return 'text-xl leading-[1.55]';
  return 'text-base leading-[1.6]';
}

export function CopyButton({
  text,
  label,
  className,
}: {
  text: string;
  label: string;
  className?: string;
}) {
  const { t } = useTranslation();
  const { toast } = useToast();
  // The icon turns to a check for a second after a copy lands.
  const [copied, setCopied] = useState(false);
  const resetTimer = useRef<ReturnType<typeof setTimeout>>();
  useEffect(() => () => clearTimeout(resetTimer.current), []);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      clearTimeout(resetTimer.current);
      resetTimer.current = setTimeout(() => setCopied(false), 1000);
      toast({ title: t('captures.toast.transcriptCopied') });
    } catch {
      toast({ title: t('captures.toast.copyFailed'), variant: 'destructive' });
    }
  };
  return (
    <Button
      variant="ghost"
      size="icon"
      className={cn('h-7 w-7', className)}
      onClick={copy}
      aria-label={label}
      title={label}
    >
      {copied ? <Check className="size-3.5!" /> : <Copy className="size-3.5!" />}
    </Button>
  );
}

/**
 * The text the capture delivered: the refined transcript, or the raw one
 * when there's no refinement. Clicking it (or Alter) edits it in place to
 * teach Kass; after saving, it shows the corrected text with every change
 * marked, and editing again saves another round of corrections.
 */
export function TranscriptCard({
  refined,
  teach,
  phrases = [],
}: {
  refined: boolean;
  teach: TeachState;
  /** Phrases from the dictionary that wrote into Kass's text, to mark there. */
  phrases?: FiredPhrase[];
}) {
  const { t } = useTranslation();
  const [explained, setExplained] = useState(false);
  const aboutId = useId();
  // The explanation makes way for the edit, and stays closed after it.
  const editing = teach.draft !== null;
  useEffect(() => {
    if (editing) setExplained(false);
  }, [editing]);
  const { learned, saved, original } = teach;
  const shown = saved?.expected_text ?? original;
  const corrected = useMemo(
    () => (saved ? diffWords(original, saved.expected_text).after : null),
    [saved, original],
  );
  const textClass = cn(textSize(countWords(shown)), 'font-medium text-foreground');
  const label = t(refined ? 'captures.transcript.refined' : 'captures.transcript.raw');

  return (
    <section
      aria-label={label}
      className="flex flex-1 min-h-[50%] overflow-y-auto flex-col gap-4 rounded-xl border border-accent/30 bg-accent/[0.045] px-5 pt-4 pb-6"
    >
      <div className="flex items-center gap-2">
        <span className="font-mono text-[11px] uppercase tracking-wider text-accent">{label}</span>
        {saved && (
          <span className="inline-flex h-[18px] shrink-0 items-center whitespace-nowrap rounded-full bg-success/15 px-1.5 font-mono text-[10px] text-success">
            {t('captures.teach.correctedByYou')}
          </span>
        )}
        <span className="flex-1" />
        {!editing && (
          <>
            {!saved && (
              <Button
                variant="ghost"
                size="icon"
                className={cn('h-7 w-7 text-muted-foreground', explained && 'text-foreground')}
                aria-label={t('captures.feedback.about')}
                title={t('captures.feedback.about')}
                aria-expanded={explained}
                aria-controls={aboutId}
                onClick={() => setExplained((open) => !open)}
              >
                <CircleHelp className="size-3.5!" />
              </Button>
            )}
            <Button
              variant="outline"
              size="sm"
              className="h-7 gap-1.5 px-2.5 text-xs border-accent/45 bg-accent/10 text-accent hover:bg-accent/15 hover:text-accent"
              onClick={teach.begin}
            >
              <Pencil className="size-3!" />
              {t('captures.teach.alter')}
            </Button>
          </>
        )}
        <CopyButton text={shown} label={t('captures.panel.copy')} />
      </div>
      {explained && (
        <p id={aboutId} className="m-0 -mt-1 text-xs leading-relaxed text-muted-foreground">
          {t('captures.feedback.description')}
        </p>
      )}
      <EditableTranscript teach={teach} className={textClass}>
        {corrected ? (
          <Marked segments={corrected} mark="corrected" />
        ) : !shown ? (
          <span className="text-base text-muted-foreground">{t('captures.snippetEmpty')}</span>
        ) : phrases.length ? (
          markPhrases(shown, phrases).map((run, i) =>
            run.phrase ? (
              // biome-ignore lint/suspicious/noArrayIndexKey: position is the identity
              <PhraseMark key={i} text={run.text} phrase={run.phrase} />
            ) : (
              run.text
            ),
          )
        ) : (
          shown
        )}
      </EditableTranscript>
      {editing ? <TeachActions teach={teach} /> : learned && <LearnedNotice teach={teach} />}
    </section>
  );
}

function Disclosure({
  title,
  open,
  onToggle,
  controls,
  action,
  className,
}: {
  title: string;
  open: boolean;
  onToggle: () => void;
  controls: string;
  action?: ReactNode;
  className?: string;
}) {
  return (
    <div className={cn('flex items-center gap-2', className)}>
      <button
        type="button"
        aria-expanded={open}
        aria-controls={controls}
        onClick={onToggle}
        className="flex h-7 flex-1 items-center gap-2 text-left text-[13px] text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring rounded-sm"
      >
        <ChevronRight
          className={cn(
            'size-3.5 shrink-0 text-muted-foreground transition-transform',
            open && 'rotate-90',
          )}
        />
        {title}
      </button>
      {action}
    </div>
  );
}

/**
 * What refinement changed, collapsed to a row of chips; opened, one run of
 * the raw text with the words taken out struck and the words put in marked.
 * With no changes it is just a line saying so.
 */
export function ChangesDisclosure({
  raw,
  refined,
  phrases = [],
}: {
  raw: string;
  refined: string;
  /** Phrases that wrote into `refined`: their changes count as phrases, not rewording. */
  phrases?: FiredPhrase[];
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const panelId = useId();
  const diff = useMemo(() => diffWords(raw, refined), [raw, refined]);
  const summary = summarizeChanges(diff);
  // A change that wrote a phrase's text is that phrase, not words reworded.
  const written = new Set(phrases.map((phrase) => phraseKey(phrase.written)));
  let phraseCount = 0;
  for (const hunk of diff.hunks) {
    if (!hunk.added || !written.has(phraseKey(hunk.added))) continue;
    const times = hunk.count ?? 1;
    phraseCount += times;
    if (hunk.removed) summary.reworded -= times;
    else summary.added -= countWords(hunk.added) * times;
  }
  const chips = (
    [
      ['phrase', phraseCount],
      ['removed', summary.removed],
      ['added', summary.added],
      ['reworded', summary.reworded],
      ['case', summary.case],
      ['punctuation', summary.punctuation],
    ] as const
  )
    .filter(([, count]) => count > 0)
    .map(([kind, count]) => t(`captures.changes.${kind}`, { count }));

  // Nothing to open when refinement left the text as it was.
  if (!chips.length) {
    return (
      <p className="m-0 flex h-7 items-center py-2.5 pl-[22px] text-[13px] text-muted-foreground box-content">
        {t('captures.changes.none')}
      </p>
    );
  }

  return (
    <div className="flex flex-col gap-1.5 py-2.5">
      <Disclosure
        title={t('captures.changes.title')}
        open={open}
        onToggle={() => setOpen((o) => !o)}
        controls={panelId}
      />
      <div className="flex flex-wrap gap-1 pl-[22px]">
        {chips.map((chip) => (
          <span
            key={chip}
            className="inline-flex h-[18px] shrink-0 items-center whitespace-nowrap rounded-full bg-accent/10 px-1.5 font-mono text-[10px] text-accent"
          >
            {chip}
          </span>
        ))}
      </div>
      {open && (
        <div
          id={panelId}
          className="ml-[22px] mt-1 rounded-lg border border-border bg-card px-3.5 py-3"
        >
          <p className="m-0 whitespace-pre-wrap break-words font-mono text-[13px] leading-[1.8] text-foreground/80">
            {diff.merged.map((segment, i) => (
              // Segments have no identity beyond their position in the text.
              // biome-ignore lint/suspicious/noArrayIndexKey: position is the identity
              <span key={i} className={MARK_CLASS[segment.kind] || undefined}>
                {segment.text}
              </span>
            ))}
          </p>
        </div>
      )}
    </div>
  );
}

/** The raw transcript, as Whisper heard it, collapsed under the changes. */
export function HeardDisclosure({ raw }: { raw: string }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const panelId = useId();

  return (
    <div className="flex flex-col gap-1.5 border-t border-border py-2.5">
      <Disclosure
        title={t('captures.transcript.heard')}
        open={open}
        onToggle={() => setOpen((o) => !o)}
        controls={panelId}
        action={
          raw && (
            <CopyButton
              text={raw}
              label={t('captures.panel.copyRaw')}
              className="text-muted-foreground"
            />
          )
        }
      />
      {open && (
        <p
          id={panelId}
          className="m-0 pl-[22px] whitespace-pre-wrap break-words font-mono text-[13px] leading-[1.7] text-foreground/70"
        >
          {raw || t('captures.snippetEmpty')}
        </p>
      )}
    </div>
  );
}
