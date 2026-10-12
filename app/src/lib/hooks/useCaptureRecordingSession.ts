import { useMutation, useQueryClient } from '@tanstack/react-query';
import { emit as tauriEmit } from '@tauri-apps/api/event';
import { useCallback, useEffect, useRef, useState } from 'react';
import type { PillState } from '@/components/CapturePill/CapturePill';
import { CaptureStream, type StreamingCaptureFinal } from '@/lib/api/captureStream';
import { apiClient } from '@/lib/api/client';
import type { CaptureListResponse, CaptureResponse, CaptureSource } from '@/lib/api/types';
import { prepareStreamingAudio, startStreamingAudio } from '@/lib/audio/streamingAudio';
import { useAudioRecording } from '@/lib/hooks/useAudioRecording';
import { serverUrl } from '@/stores/serverStore';

/**
 * Broadcast to sibling Tauri webviews that the captures list has changed.
 * The main CapturesTab listens, seeds its React Query cache, and focuses the
 * new row, so uploads from the floating dictate window show up live.
 *
 * ``capture:created`` carries the full response so the sibling can seed its
 * cache before the refetch lands — otherwise the selection-guard effect
 * would snap back to ``captures[0]`` in the race window between
 * ``setSelectedId(new)`` and the list actually containing the new row.
 *
 * No-op in web mode — there are no siblings to notify.
 */
function broadcastCreated(capture: CaptureResponse) {
  tauriEmit('capture:created', { capture }).catch(() => {
    /* not running inside Tauri; nothing to sync to */
  });
}

function broadcastUpdated(id: string) {
  tauriEmit('capture:updated', { id }).catch(() => {
    /* not running inside Tauri; nothing to sync to */
  });
}

interface RecordingTake {
  context: unknown;
  stream?: CaptureStream;
  final?: Promise<{ result: StreamingCaptureFinal | null; error?: Error }>;
  /** The microphone has delivered sound, not just the silence it opens with. */
  heard?: boolean;
}

const REST_FADE_MS = 900;
// Long enough to read a full backend stack message and click-to-copy.
const ERROR_PILL_VISIBLE_MS = 6000;
// Short self-explanatory notices (e.g. "Recording too short, canceled") —
// there's nothing to read or copy, so clear out quickly.
const BRIEF_NOTICE_MS = 2000;
// MediaRecorder.start(100) emits its first chunk ~100ms in, but the webm
// container header isn't guaranteed to be finalised that quickly — anything
// under half a second tends to produce a blob neither AudioContext.decode
// nor ffmpeg will accept. Caught client-side and surfaced as a friendly
// "Recording too short, canceled" pill instead of bubbling up a 400.
const MIN_RECORDING_DURATION_S = 0.5;
const SHORT_RECORDING_MESSAGE = 'Recording too short, canceled';
// A microphone opens with silence before it delivers sound, so the pill says
// "recording" only once sound arrives. If none does by then, show it anyway.
const HEARD_FALLBACK_MS = 1500;

export type CapturePillState = PillState | 'hidden';

export interface UseCaptureRecordingSessionOptions {
  /**
   * Fired after a capture row is created on the server. Callers can use this
   * to select the new capture or emit a Tauri event to a sibling window.
   * ``context`` is whatever was passed to ``startRecording`` for this take.
   */
  onCaptureCreated?: (capture: CaptureResponse, context?: unknown) => void;
  /**
   * Fired with the final delivered text — refined if ``auto_refine`` was on
   * for this capture, raw transcript otherwise. Used by the floating
   * dictate window to hand the text off to the Rust auto-paste pipeline.
   *
   * ``allowAutoPaste`` snapshots the setting at chord-start so a refine that
   * lands after the user flips the toggle still uses the value the capture
   * was created under. ``context`` is the value passed to ``startRecording``
   * for this take, so overlapping dictations can't cross their targets.
   */
  onFinalText?: (
    text: string,
    capture: CaptureResponse,
    allowAutoPaste: boolean,
    context?: unknown,
  ) => void | Promise<void>;
}

export interface UseCaptureRecordingSessionResult {
  pillState: CapturePillState;
  pillElapsedMs: number;
  errorMessage: string | null;
  isRecording: boolean;
  isUploading: boolean;
  isRefining: boolean;
  startRecording: (context?: unknown) => void;
  stopRecording: () => void;
  toggleRecording: () => void;
  dismissError: () => void;
  uploadFile: (file: File, source: CaptureSource) => void;
  refine: (captureId: string) => void;
}

/**
 * Owns the full record → transcribe → refine → rest lifecycle behind the
 * capture pill. The pill component and the Dictate/Stop button are the only
 * consumers; everything else (cache seeding, error toasts, settings reads) is
 * internal so the hook can be reused from a floating Tauri window without the
 * containing tab.
 */
export function useCaptureRecordingSession(
  options: UseCaptureRecordingSessionOptions = {},
): UseCaptureRecordingSessionResult {
  const mountedRef = useRef(true);
  useEffect(() => {
    // Prepare the processor without opening the microphone.
    void prepareStreamingAudio().catch(() => {});
  }, []);
  const queryClient = useQueryClient();
  // Every capture setting is resolved server-side. ``stt_model``,
  // ``llm_model`` and refine flags are read from the capture_settings table
  // inside POST /captures and /captures/*/refine, and ``auto_refine`` comes
  // back on the create response so the client decides whether to chain a
  // refine call using a value that can't go stale across sibling webviews.

  const activeTakeRef = useRef<RecordingTake | null>(null);
  const startingRef = useRef(false);
  const [finalizingCount, setFinalizingCount] = useState(0);
  const streamsRef = useRef(new Set<CaptureStream>());
  const [pillState, setPillState] = useState<CapturePillState>('hidden');
  const [frozenElapsedMs, setFrozenElapsedMs] = useState(0);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const restTimerRef = useRef<number | null>(null);
  const errorTimerRef = useRef<number | null>(null);

  // Mutation callbacks close over stale pillState otherwise.
  const pillStateRef = useRef<CapturePillState>('hidden');
  pillStateRef.current = pillState;

  const onCaptureCreatedRef = useRef(options.onCaptureCreated);
  onCaptureCreatedRef.current = options.onCaptureCreated;

  const onFinalTextRef = useRef(options.onFinalText);
  onFinalTextRef.current = options.onFinalText;

  // Per-capture recording context and its ``allow_auto_paste`` snapshot, keyed
  // by capture id so a refine that resolves after another dictation started
  // still delivers to the right target with the setting the capture was created
  // under. Populated on capture-create and consumed once the final text lands.
  const captureDeliveryRef = useRef<
    Map<string, { context: unknown; allowAutoPaste: boolean; take?: RecordingTake }>
  >(new Map());

  const clearRestTimer = useCallback(() => {
    if (restTimerRef.current !== null) {
      window.clearTimeout(restTimerRef.current);
      restTimerRef.current = null;
    }
  }, []);

  const clearErrorTimer = useCallback(() => {
    if (errorTimerRef.current !== null) {
      window.clearTimeout(errorTimerRef.current);
      errorTimerRef.current = null;
    }
  }, []);

  const scheduleHidePill = useCallback(() => {
    clearRestTimer();
    setPillState('rest');
    restTimerRef.current = window.setTimeout(() => {
      setPillState('hidden');
      restTimerRef.current = null;
    }, REST_FADE_MS);
  }, [clearRestTimer]);

  const showError = useCallback(
    (message: string, durationMs: number = ERROR_PILL_VISIBLE_MS) => {
      clearRestTimer();
      clearErrorTimer();
      setErrorMessage(message || 'Something went wrong');
      setPillState('error');
      errorTimerRef.current = window.setTimeout(() => {
        setPillState('hidden');
        setErrorMessage(null);
        errorTimerRef.current = null;
      }, durationMs);
    },
    [clearRestTimer, clearErrorTimer],
  );

  // Tell the speaker to talk only once the microphone is actually hearing.
  const markHeard = useCallback((take: RecordingTake) => {
    if (take.heard) return;
    take.heard = true;
    if (activeTakeRef.current === take)
      setPillState((state) => (state === 'preparing' ? 'recording' : state));
  }, []);

  const dismissError = useCallback(() => {
    clearErrorTimer();
    setPillState('hidden');
    setErrorMessage(null);
  }, [clearErrorTimer]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      clearRestTimer();
      clearErrorTimer();
      for (const stream of streamsRef.current) stream.cancel();
      streamsRef.current.clear();
    };
  }, [clearRestTimer, clearErrorTimer]);

  const deliverText = async (
    text: string | null | undefined,
    capture: CaptureResponse,
    allowAutoPaste: boolean,
    context?: unknown,
    currentTake?: RecordingTake,
  ) => {
    const shouldUpdatePill = () =>
      mountedRef.current && (!currentTake || activeTakeRef.current === currentTake);
    try {
      // Phase one inserts only at completion. Empty output must not replace
      // an existing selection; deletion requires a verified owned range later.
      if (text) await onFinalTextRef.current?.(text, capture, allowAutoPaste, context);
      if (
        shouldUpdatePill() &&
        (currentTake ||
          pillStateRef.current === 'transcribing' ||
          pillStateRef.current === 'refining')
      ) {
        scheduleHidePill();
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (shouldUpdatePill()) showError(`Text saved in Captures. ${message}`);
    }
  };

  const refineMutation = useMutation({
    // Empty body — backend resolves flags and model from capture_settings.
    mutationFn: async (captureId: string) => apiClient.refineCapture(captureId, {}),
    onSuccess: async (data, captureId) => {
      queryClient.invalidateQueries({ queryKey: ['captures'] });
      broadcastUpdated(captureId);
      const delivery = captureDeliveryRef.current.get(captureId);
      captureDeliveryRef.current.delete(captureId);
      const finalText = data.transcript_refined ?? data.transcript_raw;
      await deliverText(
        finalText,
        data,
        delivery?.allowAutoPaste ?? true,
        delivery?.context,
        delivery?.take,
      );
    },
    onError: (err: Error, captureId) => {
      const delivery = captureDeliveryRef.current.get(captureId);
      captureDeliveryRef.current.delete(captureId);
      if (!delivery?.take || activeTakeRef.current === delivery.take)
        showError(err.message || 'Refinement failed');
    },
  });

  const uploadMutation = useMutation({
    mutationFn: async ({
      file,
      source,
    }: {
      file: File;
      source: CaptureSource;
      context?: unknown;
      take?: RecordingTake;
    }) => apiClient.createCapture(file, { source }),
    onSuccess: async (capture, { context, take }) => {
      if (!mountedRef.current) return;
      queryClient.setQueryData<CaptureListResponse>(['captures'], (prev) => {
        if (!prev) return prev;
        if (prev.items.some((c) => c.id === capture.id)) return prev;
        return { ...prev, items: [capture, ...prev.items], total: prev.total + 1 };
      });
      queryClient.invalidateQueries({ queryKey: ['captures'] });
      broadcastCreated(capture);
      onCaptureCreatedRef.current?.(capture, context);
      if (capture.auto_refine) {
        captureDeliveryRef.current.set(capture.id, {
          context,
          allowAutoPaste: capture.allow_auto_paste,
          take,
        });
        if (!take || activeTakeRef.current === take) setPillState('refining');
        refineMutation.mutate(capture.id);
      } else {
        await deliverText(capture.transcript_raw, capture, capture.allow_auto_paste, context, take);
      }
    },
    onError: (err: Error, { take }) => {
      if (!mountedRef.current || (take && activeTakeRef.current !== take)) return;
      // Backend's librosa-audioread fallback returns a 400 with this shape
      // for tiny/corrupt webm blobs that slip past the client guard —
      // translate it to the same friendly message so the user sees one
      // consistent cause, not an opaque decode error.
      const msg = err.message || '';
      if (/could not decode/i.test(msg) || /empty or corrupt/i.test(msg)) {
        showError(SHORT_RECORDING_MESSAGE, BRIEF_NOTICE_MS);
      } else {
        showError(msg || 'Upload failed');
      }
    },
  });

  const {
    isRecording,
    duration,
    startRecording: beginAudioRecording,
    canStartRecording,
    stopRecording,
    error: recordError,
  } = useAudioRecording({
    onRecordingStream: async (stream, context) => {
      const take = context as RecordingTake;
      try {
        const audio = await startStreamingAudio(
          stream,
          (sampleRate, coversStart) => {
            // A stream that missed the first words would lose them, so it only
            // listens for sound and the complete recording is transcribed.
            if (!coversStart) return;
            take.stream = new CaptureStream(serverUrl(), sampleRate);
            streamsRef.current.add(take.stream);
          },
          (frame) => {
            if (!take.heard && new Int16Array(frame).some((sample) => sample !== 0))
              markHeard(take);
            take.stream?.append(frame);
          },
          () => take.stream?.cancel(),
        );
        return {
          stop: async (duration) => {
            try {
              await audio.stop();
            } catch {
              take.stream?.cancel();
            }
            if (take.stream && (duration ?? 0) >= MIN_RECORDING_DURATION_S) {
              setFinalizingCount((count) => count + 1);
              take.final = take.stream.finish().then(
                (result) => ({ result }),
                (error: Error) => ({ result: null, error }),
              );
            } else take.stream?.cancel();
          },
          cancel: () => {
            audio.cancel();
            take.stream?.cancel();
            if (take.stream) streamsRef.current.delete(take.stream);
          },
        };
      } catch (error) {
        take.stream?.cancel();
        if (take.stream) streamsRef.current.delete(take.stream);
        // Without the PCM processor there is no way to hear the microphone.
        markHeard(take);
        throw error;
      }
    },
    onRecordingComplete: async (blob, recordedDuration, recordingContext) => {
      if (!mountedRef.current) return;
      const take = recordingContext as RecordingTake;
      const context = take.context;
      // Trigger-happy tap — MediaRecorder hasn't emitted a usable chunk yet
      // so the blob is empty or unparseable. Surface it as a transient pill
      // so the user sees their recording was recognised and canceled.
      if ((!blob.size && !take.final) || (recordedDuration ?? 0) < MIN_RECORDING_DURATION_S) {
        take.stream?.cancel();
        if (take.stream) streamsRef.current.delete(take.stream);
        if (activeTakeRef.current === take) showError(SHORT_RECORDING_MESSAGE, BRIEF_NOTICE_MS);
        return;
      }
      if (activeTakeRef.current === take) {
        setFrozenElapsedMs(Math.round((recordedDuration ?? 0) * 1000));
        setPillState('transcribing');
      }
      if (take.stream) {
        try {
          const final = take.final ? await take.final : { result: null };
          if (!mountedRef.current) return;
          if (final.error) throw final.error;
          const result = final.result;
          if (result) {
            const capture = result.capture;
            queryClient.setQueryData<CaptureListResponse>(['captures'], (previous) => {
              if (!previous || previous.items.some((item) => item.id === capture.id))
                return previous;
              return {
                ...previous,
                items: [capture, ...previous.items],
                total: previous.total + 1,
              };
            });
            queryClient.invalidateQueries({ queryKey: ['captures'] });
            broadcastCreated(capture);
            onCaptureCreatedRef.current?.(capture, context);
            if (result.refinement_error) {
              if (activeTakeRef.current === take)
                showError(`Text saved in Captures. ${result.refinement_error}`);
            } else {
              await deliverText(
                capture.transcript_refined ?? capture.transcript_raw,
                capture,
                capture.allow_auto_paste,
                context,
                take,
              );
            }
            return;
          }
          // No finish was sent, so the archived audio can safely use batch transcription.
        } catch (error) {
          if (activeTakeRef.current === take)
            showError(error instanceof Error ? error.message : 'Streaming finalization failed');
          return;
        } finally {
          streamsRef.current.delete(take.stream);
          if (take.final && mountedRef.current) setFinalizingCount((count) => count - 1);
        }
      }
      const extension = blob.type.includes('wav')
        ? 'wav'
        : blob.type.includes('webm')
          ? 'webm'
          : 'bin';
      const file = new File([blob], `dictation-${Date.now()}.${extension}`, {
        type: blob.type,
      });
      uploadMutation.mutate({ file, source: 'dictation', context, take });
    },
  });

  useEffect(() => {
    // MediaRecorder has started only after the browser grants mic access; the
    // pill switches to recording when the processor first hears sound.
    const take = activeTakeRef.current;
    if (!isRecording || !take) return;
    if (take.heard) {
      setPillState('recording');
      return;
    }
    const timer = window.setTimeout(() => markHeard(take), HEARD_FALLBACK_MS);
    return () => window.clearTimeout(timer);
  }, [isRecording, markHeard]);

  useEffect(() => {
    if (recordError) {
      showError(recordError);
    }
  }, [recordError, showError]);

  useEffect(() => {
    if (!isRecording) return;
    const interval = window.setInterval(() => {
      void apiClient.pauseLearningForRecording().catch(() => {});
    }, 30_000);
    return () => window.clearInterval(interval);
  }, [isRecording]);

  const startRecording = useCallback(
    (context?: unknown) => {
      if (isRecording || startingRef.current || !canStartRecording()) return;
      startingRef.current = true;
      void apiClient.pauseLearningForRecording().catch(() => {});
      clearRestTimer();
      setFrozenElapsedMs(0);
      clearErrorTimer();
      setErrorMessage(null);
      setPillState('preparing');
      const take: RecordingTake = { context };
      activeTakeRef.current = take;
      void beginAudioRecording(take).finally(() => {
        startingRef.current = false;
      });
    },
    [isRecording, canStartRecording, beginAudioRecording, clearRestTimer, clearErrorTimer],
  );

  const toggleRecording = useCallback(() => {
    if (isRecording) {
      stopRecording();
      return;
    }
    startRecording();
  }, [isRecording, startRecording, stopRecording]);

  const uploadFile = useCallback(
    (file: File, source: CaptureSource) => {
      uploadMutation.mutate({ file, source });
    },
    [uploadMutation],
  );

  const refine = useCallback(
    (captureId: string) => {
      refineMutation.mutate(captureId);
    },
    [refineMutation],
  );

  const pillElapsedMs = pillState === 'recording' ? Math.round(duration * 1000) : frozenElapsedMs;

  return {
    pillState,
    pillElapsedMs,
    errorMessage,
    isRecording,
    isUploading: uploadMutation.isPending || finalizingCount > 0,
    isRefining: refineMutation.isPending,
    startRecording,
    stopRecording,
    toggleRecording,
    dismissError,
    uploadFile,
    refine,
  };
}
