import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useCallback, useEffect, useRef, useState } from 'react';
import type { NativeDictationEvent } from '@/lib/hooks/nativeDictationEvent';
import { usePlatform } from '@/platform/PlatformContext';

/**
 * Where a reply's dictation is: idle, listening (and how the take started,
 * which decides how the user finishes it), or being cleaned up.
 */
export type TeachDictation =
  | { phase: 'idle' }
  | { phase: 'listening'; how: 'button' | 'shortcut' }
  | { phase: 'cleaning' };

/**
 * Follow dictation takes while a reply is open. A shortcut take types its
 * text into the focused reply box by itself (`useInAppDictationInsert`); a
 * take started from the Dictate button lands in Captures, so `onButtonDone`
 * fetches its cleanup for the box.
 */
export function useTeachDictation(onButtonDone: () => void) {
  const platform = usePlatform();
  const [state, setState] = useState<TeachDictation>({ phase: 'idle' });
  const buttonTake = useRef<number | null>(null);
  // Set from the click until the take's id is known; its first event can beat the reply.
  const buttonPending = useRef(false);
  const onDone = useRef(onButtonDone);
  onDone.current = onButtonDone;

  useEffect(() => {
    if (!platform.metadata.isTauri) return;
    let disposed = false;
    let release: (() => void) | null = null;
    listen<NativeDictationEvent>('dictation:state', ({ payload }) => {
      if (buttonPending.current && buttonTake.current === null && payload.state === 'preparing') {
        buttonTake.current = payload.take;
      }
      const fromButton = payload.take === buttonTake.current;
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
            onDone.current();
          }
          break;
        case 'cancelled':
        case 'error':
          setState({ phase: 'idle' });
          if (fromButton) buttonTake.current = null;
          break;
      }
    })
      .then((unlisten) => {
        if (disposed) unlisten();
        else release = unlisten;
      })
      .catch((err) => console.warn('[teach] dictation listener failed:', err));
    return () => {
      disposed = true;
      release?.();
    };
  }, [platform.metadata.isTauri]);

  const start = useCallback(() => {
    buttonPending.current = true;
    invoke<number | null>('dictation_start')
      .then((take) => {
        if (take !== null) buttonTake.current = take;
      })
      .catch((err) => console.warn('[teach] dictation_start failed:', err))
      .finally(() => {
        buttonPending.current = false;
      });
  }, []);

  const stop = useCallback(() => {
    invoke('dictation_stop').catch((err) => console.warn('[teach] dictation_stop failed:', err));
  }, []);

  return { state, start, stop, available: platform.metadata.isTauri };
}
