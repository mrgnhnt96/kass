import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useCallback, useEffect, useRef, useState } from 'react';
import type { StyleChange } from '@/components/CapturePill/StyleChip';
import type { CapturePillState } from '@/lib/hooks/useCaptureRecordingSession';
import { useCaptureSettings } from '@/lib/hooks/useSettings';
import { useServerUrl } from '@/stores/serverStore';

/**
 * Pill state for one native take, emitted by Rust (`dictation/mod.rs`).
 * ``take`` increases per recording so a take that is still finishing can't
 * overwrite the pill of a newer one.
 */
export type NativeDictationEvent =
  | { take: number; state: 'preparing' | 'recording' | 'refining' | 'done' | 'cancelled' }
  | { take: number; state: 'transcribing'; elapsed_ms: number }
  | { take: number; state: 'error'; message: string; visible_ms: number };

/** Microphone loudness for the HUD's level bars, about 20 times a second. */
export type NativeDictationLevel = { take: number; db: number };

/** The writing style the user asked for by name ("use formal mode"). */
export type NativeDictationStyle = { take: number; from: string | null; to: string };

export interface NativeDictationSession {
  pillState: CapturePillState;
  pillElapsedMs: number;
  /** Input loudness in dBFS while recording; `null` before the first reading. */
  inputDb?: number | null;
  errorMessage: string | null;
  /** A style asked for by name, for the chip above the pill until it plays out. */
  styleChange?: StyleChange | null;
  finishStyleChange?: () => void;
  isRecording: boolean;
  stopRecording: () => void;
  dismissError: () => void;
}

const REST_FADE_MS = 900;
const ELAPSED_TICK_MS = 250;

/**
 * Dictation in the desktop app. Rust owns the microphone, the streaming
 * socket and paste delivery; this hook only mirrors the take's state onto
 * the pill and tells Rust where the server is and which microphone to use.
 * The webview never opens browser audio for dictation.
 */
/** How long a microphone may stay silent before the pill says it's opening. */
export const SLOW_MICROPHONE_MS = 300;

export function useNativeDictationSession(): NativeDictationSession {
  const { settings } = useCaptureSettings();
  const inputDeviceId = settings?.input_device_id ?? null;
  // Until settings load, say nothing about the microphone so Rust keeps the
  // one it remembered from the last launch.
  const deviceKnown = settings !== undefined;
  const liveText = settings?.live_text ?? false;
  // Unknown until settings load, so Rust keeps its default. Rust also needs
  // the beta channel on (docs/plans/VOICE_EDITS.md).
  const voiceEdits = settings?.voice_edits;
  // Another Mac's server (beta `network`) is only known once the channel is.
  const serverUrl = useServerUrl();

  const [pillState, setPillState] = useState<CapturePillState>('hidden');
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [inputDb, setInputDb] = useState<number | null>(null);
  const [styleChange, setStyleChange] = useState<StyleChange | null>(null);
  const styleChangesRef = useRef(0);
  const [frozenElapsedMs, setFrozenElapsedMs] = useState(0);
  const [liveElapsedMs, setLiveElapsedMs] = useState(0);
  const startedAtRef = useRef<number | null>(null);
  const currentTakeRef = useRef(0);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const slowMicrophoneRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const clearTimer = useCallback(() => {
    if (timerRef.current !== null) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    if (slowMicrophoneRef.current !== null) {
      clearTimeout(slowMicrophoneRef.current);
      slowMicrophoneRef.current = null;
    }
  }, []);

  useEffect(() => {
    invoke('dictation_configure', {
      serverUrl,
      origin: window.location.origin,
      inputDeviceId,
      deviceKnown,
      liveText,
      voiceEdits,
    }).catch((err) => console.warn('[dictate] dictation_configure failed:', err));
  }, [serverUrl, inputDeviceId, deviceKnown, liveText, voiceEdits]);

  useEffect(() => {
    const apply = (event: NativeDictationEvent) => {
      if (event.state === 'preparing') {
        if (event.take < currentTakeRef.current) return;
        currentTakeRef.current = event.take;
      } else if (event.take !== currentTakeRef.current) {
        return;
      }
      clearTimer();
      switch (event.state) {
        case 'preparing':
          startedAtRef.current = Date.now();
          setLiveElapsedMs(0);
          setFrozenElapsedMs(0);
          setErrorMessage(null);
          setInputDb(null);
          setStyleChange(null);
          // Most microphones deliver sound within ~0.1 s, so show recording
          // right away; only a device still silent after SLOW_MICROPHONE_MS
          // (e.g. a Bluetooth headset switching to call mode) says it's opening.
          setPillState('recording');
          slowMicrophoneRef.current = setTimeout(() => {
            slowMicrophoneRef.current = null;
            setPillState('preparing');
          }, SLOW_MICROPHONE_MS);
          break;
        case 'recording':
          setPillState('recording');
          break;
        case 'transcribing':
          startedAtRef.current = null;
          setFrozenElapsedMs(event.elapsed_ms);
          setPillState('transcribing');
          break;
        case 'refining':
          setPillState('refining');
          break;
        case 'done':
          startedAtRef.current = null;
          setPillState('rest');
          timerRef.current = setTimeout(() => {
            timerRef.current = null;
            setPillState('hidden');
          }, REST_FADE_MS);
          break;
        case 'cancelled':
          // Escape: gone at once, without the resting fade.
          startedAtRef.current = null;
          setPillState('hidden');
          break;
        case 'error':
          startedAtRef.current = null;
          setErrorMessage(event.message || 'Something went wrong');
          setPillState('error');
          timerRef.current = setTimeout(() => {
            timerRef.current = null;
            setPillState('hidden');
            setErrorMessage(null);
          }, event.visible_ms);
          break;
      }
    };

    let disposed = false;
    const releases: (() => void)[] = [];
    const subscribe = <T>(name: string, handle: (payload: T) => void) =>
      listen<T>(name, ({ payload }) => handle(payload))
        .then((unlisten) => {
          if (disposed) unlisten();
          else releases.push(unlisten);
        })
        .catch((err) => console.warn(`[dictate] ${name} listener failed:`, err));
    subscribe<NativeDictationEvent>('dictation:state', apply);
    subscribe<NativeDictationLevel>('dictation:level', (level) => {
      if (level.take === currentTakeRef.current) setInputDb(level.db);
    });
    subscribe<NativeDictationStyle>('dictation:style', ({ take, from, to }) => {
      if (take !== currentTakeRef.current) return;
      // The window grows up from the pill first, so the chip has room to rise into.
      invoke('dictate_chip_space', { show: true })
        .catch((err) => console.warn('[dictate] dictate_chip_space failed:', err))
        .finally(() => {
          if (take === currentTakeRef.current) {
            styleChangesRef.current += 1;
            setStyleChange({ key: styleChangesRef.current, from, to });
          }
        });
    });
    return () => {
      disposed = true;
      for (const release of releases) release();
      clearTimer();
    };
  }, [clearTimer]);

  const isRecording = pillState === 'preparing' || pillState === 'recording';

  useEffect(() => {
    if (!isRecording) return;
    const interval = setInterval(() => {
      if (startedAtRef.current !== null) setLiveElapsedMs(Date.now() - startedAtRef.current);
    }, ELAPSED_TICK_MS);
    return () => clearInterval(interval);
  }, [isRecording]);

  const stopRecording = useCallback(() => {
    invoke('dictation_stop').catch((err) => console.warn('[dictate] dictation_stop failed:', err));
  }, []);

  const finishStyleChange = useCallback(() => {
    setStyleChange(null);
    // The room above the pill would otherwise take clicks meant for what's under it.
    invoke('dictate_chip_space', { show: false }).catch((err) =>
      console.warn('[dictate] dictate_chip_space failed:', err),
    );
  }, []);

  const dismissError = useCallback(() => {
    clearTimer();
    setPillState('hidden');
    setErrorMessage(null);
  }, [clearTimer]);

  return {
    pillState,
    pillElapsedMs: pillState === 'recording' ? liveElapsedMs : frozenElapsedMs,
    errorMessage,
    styleChange,
    finishStyleChange,
    inputDb: isRecording ? inputDb : null,
    isRecording,
    stopRecording,
    dismissError,
  };
}
