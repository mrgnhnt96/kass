import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useCallback, useEffect, useRef, useState } from 'react';
import type { CaptureResponse } from '@/lib/api/types';
import type { NativeDictationEvent } from '@/lib/hooks/nativeDictationEvent';

export type ChordAction = 'push_to_talk' | 'toggle_to_talk' | 'command';

/** Run `handler` for a Tauri event while mounted. */
function useTauriEvent<T>(name: string, handler: (payload: T) => void) {
  const latest = useRef(handler);
  latest.current = handler;
  useEffect(() => {
    let disposed = false;
    let release: (() => void) | null = null;
    listen<T>(name, ({ payload }) => latest.current(payload))
      .then((unlisten) => {
        if (disposed) unlisten();
        else release = unlisten;
      })
      .catch((err) => console.warn(`[onboarding] ${name} listener failed:`, err));
    return () => {
      disposed = true;
      release?.();
    };
  }, [name]);
}

/**
 * The chord being held right now, from the hotkey monitor's `chord:down` and
 * `chord:up`. `practice` stops chords from recording while it's on, so a
 * step can have the user try the keys before the models are ready.
 */
export function useHeldChord(practice: boolean) {
  const [held, setHeld] = useState<ChordAction | null>(null);
  const [pressedOnce, setPressedOnce] = useState(false);
  useTauriEvent<{ action: ChordAction }>('chord:down', ({ action }) => {
    setHeld(action);
    setPressedOnce(true);
  });
  useTauriEvent<{ action: ChordAction }>('chord:up', () => setHeld(null));

  useEffect(() => {
    invoke('set_chord_practice', { enabled: practice }).catch((err) =>
      console.warn('[onboarding] set_chord_practice failed:', err),
    );
    if (!practice) return;
    return () => {
      invoke('set_chord_practice', { enabled: false }).catch(() => {});
    };
  }, [practice]);

  return { held, pressedOnce };
}

export type MicPermission = 'granted' | 'denied' | 'restricted' | 'undetermined';

/**
 * The microphone's permission and, once it's allowed, a live level (dBFS)
 * from a preview that records nothing.
 */
export function useMicPreview(deviceId: string | null) {
  const [permission, setPermission] = useState<MicPermission | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [db, setDb] = useState(-100);
  const [error, setError] = useState<string | null>(null);

  const recheck = useCallback(() => {
    invoke<MicPermission>('microphone_permission')
      .then(setPermission)
      .catch(() => setPermission('granted'));
  }, []);
  useEffect(() => recheck(), [recheck]);

  useTauriEvent<{ db: number }>('mic:level', ({ db }) => setDb(db));
  useTauriEvent<{ message: string }>('mic:error', ({ message }) => setError(message));

  useEffect(() => {
    if (!previewing) return;
    setError(null);
    invoke('mic_preview_start', { deviceId })
      .then(recheck)
      .catch((err) => {
        setError(String(err));
        recheck();
      });
    return () => {
      invoke('mic_preview_stop').catch(() => {});
    };
  }, [previewing, deviceId, recheck]);

  return { permission, db, error, previewing, start: () => setPreviewing(true), recheck };
}

export type LoginItemStatus = 'enabled' | 'disabled' | 'requires_approval' | 'unavailable';

/** Launch at login, as macOS reports it. */
export function useLaunchAtLogin() {
  const [status, setStatus] = useState<LoginItemStatus | null>(null);
  useEffect(() => {
    invoke<LoginItemStatus>('launch_at_login_status')
      .then(setStatus)
      .catch(() => setStatus('unavailable'));
  }, []);
  const set = useCallback((enabled: boolean) => {
    invoke<LoginItemStatus>('set_launch_at_login', { enabled })
      .then(setStatus)
      .catch((err) => console.warn('[onboarding] set_launch_at_login failed:', err));
  }, []);
  return { status, set };
}

export type TakePhase = 'idle' | 'listening' | 'working';

/**
 * Dictation takes while a spoken step is open: whether one is listening, a
 * Dictate button that starts and stops one, and each finished capture.
 * Shortcut takes also type into the focused field on their own.
 */
export function useTakes(
  onCapture: (capture: CaptureResponse) => void,
  /** A take already recording when the step opened (its start event came before). */
  recording = false,
) {
  const [phase, setPhase] = useState<TakePhase>(recording ? 'listening' : 'idle');
  const [error, setError] = useState<string | null>(null);

  useTauriEvent<NativeDictationEvent>('dictation:state', (event) => {
    switch (event.state) {
      case 'preparing':
      case 'recording':
        setError(null);
        setPhase('listening');
        break;
      case 'transcribing':
      case 'refining':
        setPhase('working');
        break;
      case 'error':
        setError(event.message);
        setPhase('idle');
        break;
      default:
        setPhase('idle');
    }
  });
  useTauriEvent<{ capture: CaptureResponse }>('capture:created', ({ capture }) =>
    onCapture(capture),
  );

  const toggle = useCallback(() => {
    const command = phase === 'listening' ? 'dictation_stop' : 'dictation_start';
    invoke(command).catch((err) => console.warn(`[onboarding] ${command} failed:`, err));
  }, [phase]);

  return { phase, error, toggle };
}
