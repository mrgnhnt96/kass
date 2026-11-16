import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Mic, Square } from 'lucide-react';
import {
  type KeyboardEvent,
  type RefObject,
  useCallback,
  useEffect,
  useRef,
  useState,
} from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Textarea } from '@/components/ui/textarea';
import { apiClient } from '@/lib/api/client';
import { claimInAppDictation, isEditableField } from '@/lib/hooks/useInAppDictationInsert';
import type { NativeDictationEvent } from '@/lib/hooks/nativeDictationEvent';
import { useCaptureSettings } from '@/lib/hooks/useSettings';
import { cn } from '@/lib/utils/cn';
import { defaultChordKeys } from '@/lib/utils/keyCodes';
import { usePlatform } from '@/platform/PlatformContext';
import { ChordKeys } from './ChordKeys';
import { MAX_LENGTH } from './DictionaryControls';

/**
 * Saying a phrase instead of typing it: the "When I say" box fills itself
 * from the mic button, or from the push-to-talk shortcut while no other
 * field is focused (docs/plans/DICTIONARIES.md, "Phrases").
 */

const P = 'dictionary.phrases';
/** Matches the server's limit on what a phrase writes. */
const MAX_PHRASE_LENGTH = 1000;

/** What was said as a phrase to match: one line, lowercase, without the punctuation around it. */
export function spokenPhrase(text: string): string {
  return text
    .split(/\s+/)
    .join(' ')
    .replace(/^[^\p{L}\p{N}]+|[^\p{L}\p{N}]+$/gu, '')
    .toLowerCase()
    .slice(0, MAX_LENGTH);
}

export type PhraseDictationState =
  | { phase: 'idle' }
  | { phase: 'listening'; how: 'button' | 'shortcut' }
  | { phase: 'cleaning' };

/**
 * Follows dictation takes for one "When I say" box. A take from the mic
 * button lands in Captures; a shortcut take is claimed before it would be
 * typed, unless another field has focus, where it is typed as usual. Either
 * way the box gets the take's raw transcript, as heard: cleanup may have
 * rewritten it, or swapped a phrase already in the dictionary.
 */
export function usePhraseDictation(
  say: RefObject<HTMLInputElement>,
  onPhrase: (phrase: string) => void,
) {
  const platform = usePlatform();
  const available = platform.metadata.isTauri;
  const [state, setState] = useState<PhraseDictationState>({ phase: 'idle' });
  const buttonTake = useRef<number | null>(null);
  // Set from the click until the take's id is known; its first event can beat the reply.
  const buttonPending = useRef(false);
  // Shortcut takes this box follows, and the text claimed from each once it arrives.
  const takes = useRef(new Map<number, { text: string | null; done: boolean }>());
  const onPhraseRef = useRef(onPhrase);
  onPhraseRef.current = onPhrase;

  /** The box takes a shortcut take unless another field is where the user is typing. */
  const aimsHere = useCallback(() => {
    const active = document.activeElement;
    return !isEditableField(active) || active === say.current;
  }, [say]);

  /** Fills the box from the newest capture: the take just finished. */
  const fill = useCallback((claimed: string | null) => {
    void apiClient
      .listCaptures(1)
      .then(({ items }) => {
        const capture = items[0];
        const delivered = capture?.transcript_refined || capture?.transcript_raw || '';
        // A shortcut take's capture is the one that delivered its text;
        // otherwise it isn't saved yet, and the text claimed stands in.
        const ours =
          capture && (claimed === null || spokenPhrase(delivered) === spokenPhrase(claimed));
        const phrase = spokenPhrase((ours ? capture.transcript_raw : null) || claimed || '');
        if (phrase) onPhraseRef.current(phrase);
      })
      .catch(() => {
        if (claimed && spokenPhrase(claimed)) onPhraseRef.current(spokenPhrase(claimed));
      });
  }, []);

  const settle = useCallback(
    (take: number) => {
      const followed = takes.current.get(take);
      if (!followed?.done || followed.text === null) return;
      takes.current.delete(take);
      fill(followed.text);
    },
    [fill],
  );

  useEffect(() => {
    if (!available) return;
    return claimInAppDictation((take, text) => {
      if (!aimsHere()) return false;
      // A take that started while another field had focus wasn't followed:
      // its text is all there is to wait for.
      const followed = takes.current.get(take) ?? { text: null, done: true };
      takes.current.set(take, { ...followed, text });
      settle(take);
      return true;
    });
  }, [available, aimsHere, settle]);

  useEffect(() => {
    if (!available) return;
    let disposed = false;
    let release: (() => void) | null = null;
    listen<NativeDictationEvent>('dictation:state', ({ payload }) => {
      const { take } = payload;
      if (buttonPending.current && buttonTake.current === null && payload.state === 'preparing') {
        buttonTake.current = take;
      }
      const fromButton = take === buttonTake.current;
      if (!fromButton && payload.state === 'preparing' && aimsHere()) {
        takes.current.set(take, { text: null, done: false });
      }
      if (!fromButton && !takes.current.has(take)) return;
      switch (payload.state) {
        case 'preparing':
        case 'recording':
          setState({ phase: 'listening', how: fromButton ? 'button' : 'shortcut' });
          break;
        case 'transcribing':
        case 'refining':
          setState({ phase: 'cleaning' });
          break;
        case 'done':
          setState({ phase: 'idle' });
          if (fromButton) {
            buttonTake.current = null;
            fill(null);
          } else {
            const followed = takes.current.get(take);
            if (followed) takes.current.set(take, { ...followed, done: true });
            settle(take);
          }
          break;
        case 'cancelled':
        case 'error':
          setState({ phase: 'idle' });
          if (fromButton) buttonTake.current = null;
          takes.current.delete(take);
          break;
      }
    })
      .then((unlisten) => {
        if (disposed) unlisten();
        else release = unlisten;
      })
      .catch((err) => console.warn('[phrase] dictation listener failed:', err));
    return () => {
      disposed = true;
      release?.();
    };
  }, [available, aimsHere, fill, settle]);

  const start = useCallback(() => {
    buttonPending.current = true;
    invoke<number | null>('dictation_start')
      .then((take) => {
        if (take !== null) buttonTake.current = take;
      })
      .catch((err) => console.warn('[phrase] dictation_start failed:', err))
      .finally(() => {
        buttonPending.current = false;
      });
  }, []);

  const stop = useCallback(() => {
    invoke('dictation_stop').catch((err) => console.warn('[phrase] dictation_stop failed:', err));
  }, []);

  return { state, start, stop, available };
}

export type PhraseDictation = ReturnType<typeof usePhraseDictation>;

/** Level bars while a take is heard; they only show that it is listening. */
function Listening() {
  const { t } = useTranslation();
  return (
    <span className="pointer-events-none absolute inset-0 flex items-center gap-2.5 px-3">
      <span aria-hidden className="flex h-4 items-center gap-0.5">
        {[6, 14, 10, 16, 8].map((height, i) => (
          <span
            // biome-ignore lint/suspicious/noArrayIndexKey: fixed decoration
            key={i}
            className="w-[3px] animate-pulse rounded-sm bg-accent"
            style={{ height, animationDelay: `${i * 120}ms` }}
          />
        ))}
      </span>
      <span className="font-mono text-[11px] text-accent">{t(`${P}.listening`)}</span>
    </span>
  );
}

/**
 * "When I say": the phrase, typed, or said with the mic button beside it.
 * While a take for it is heard, the box shows that it is listening; when the
 * take is in, the phrase it heard replaces what was there.
 */
export function PhraseSayField({
  id,
  value,
  onChange,
  onKeyDown,
  inputRef,
  dictation,
  filled,
  autoFocus,
  size = 'md',
}: {
  id: string;
  value: string;
  onChange: (value: string) => void;
  onKeyDown?: (event: KeyboardEvent<HTMLInputElement>) => void;
  inputRef: RefObject<HTMLInputElement>;
  dictation: PhraseDictation;
  /** Briefly marks the box after a take filled it. */
  filled?: boolean;
  autoFocus?: boolean;
  /** `sm` beside the page's other fields; `md` in a dialog. */
  size?: 'sm' | 'md';
}) {
  const { t } = useTranslation();
  const { state } = dictation;
  const height = size === 'sm' ? 'h-8' : 'h-9';
  const listening = state.phase === 'listening';
  const button = listening && state.how === 'button';
  return (
    <div className="flex gap-1.5">
      <div className="relative min-w-0 flex-1">
        <Input
          id={id}
          ref={inputRef}
          value={value}
          onChange={(event) => onChange(event.target.value)}
          onKeyDown={onKeyDown}
          placeholder={t(`${P}.sayPlaceholder`)}
          maxLength={MAX_LENGTH}
          autoFocus={autoFocus}
          aria-busy={state.phase !== 'idle'}
          className={cn(
            'transition-colors duration-500',
            listening &&
              'border-accent text-transparent ring-2 ring-accent/20 placeholder:text-transparent',
            state.phase === 'cleaning' && 'opacity-60',
            filled && 'bg-accent/10',
            height,
          )}
        />
        {listening && <Listening />}
      </div>
      {dictation.available && (
        <Button
          type="button"
          size="icon"
          variant={button ? 'default' : 'outline'}
          className={cn('shrink-0 [&_svg]:size-3.5', height, size === 'sm' ? 'w-8' : 'w-9')}
          disabled={state.phase === 'cleaning' || (listening && !button)}
          aria-label={t(button ? `${P}.stop` : `${P}.dictate`)}
          title={t(button ? `${P}.stop` : `${P}.dictate`)}
          // Keeps focus where it is: the take goes to the phrase box either way.
          onMouseDown={(event) => event.preventDefault()}
          onClick={() => (button ? dictation.stop() : dictation.start())}
        >
          {button ? <Square className="fill-current" /> : <Mic />}
        </Button>
      )}
    </div>
  );
}

/** "Type the phrase, click the mic, or hold ⌘ ⌥ to say it", with the user's own shortcut. */
export function PhraseHint({
  dictation,
  variant = 'add',
  className,
}: {
  dictation: PhraseDictation;
  /** `another`: in a dialog that already has the phrase, to say it another way. */
  variant?: 'add' | 'another';
  className?: string;
}) {
  const { settings } = useCaptureSettings();
  const keys = settings?.chord_push_to_talk_keys ?? defaultChordKeys('push');
  const { state } = dictation;
  const holding = state.phase === 'listening' && state.how === 'shortcut';
  const chord = (
    <span className="mx-1 inline-flex align-middle">
      <ChordKeys keys={keys} />
    </span>
  );
  let key: string;
  if (!dictation.available) key = `${P}.hintTyped`;
  else if (holding) key = `${P}.hintRelease`;
  else key = variant === 'another' ? `${P}.hintAnother` : `${P}.hint`;
  return (
    <p className={cn('m-0 text-xs leading-[26px] text-muted-foreground', className)}>
      <Trans i18nKey={key} components={{ keys: chord }} />
    </p>
  );
}

/** True for a moment after `value` is set by a take, to mark the box it filled. */
export function useFilled(): [boolean, () => void] {
  const [filled, setFilled] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout>>();
  useEffect(() => () => clearTimeout(timer.current), []);
  const mark = useCallback(() => {
    setFilled(true);
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setFilled(false), 900);
  }, []);
  return [filled, mark];
}

/** "Write exactly": the text a phrase writes. ⏎ is a new line; ⌘⏎ submits, esc cancels. */
export function PhraseTextField({
  id,
  value,
  onChange,
  onSubmit,
  onCancel,
  autoFocus,
  className,
}: {
  id?: string;
  value: string;
  onChange: (value: string) => void;
  onSubmit: () => void;
  onCancel?: () => void;
  autoFocus?: boolean;
  className?: string;
}) {
  const { t } = useTranslation();
  return (
    <Textarea
      id={id}
      value={value}
      onChange={(event) => onChange(event.target.value)}
      onKeyDown={(event) => {
        if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
          event.preventDefault();
          onSubmit();
        } else if (event.key === 'Escape' && onCancel) {
          event.preventDefault();
          onCancel();
        }
      }}
      placeholder={t(`${P}.writePlaceholder`)}
      aria-label={id ? undefined : t(`${P}.write`)}
      maxLength={MAX_PHRASE_LENGTH}
      rows={Math.min(6, Math.max(2, value.split('\n').length))}
      autoFocus={autoFocus}
      className={cn('min-h-0 resize-none px-2.5 py-1.5 leading-normal', className)}
    />
  );
}
