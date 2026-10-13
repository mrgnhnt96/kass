import { withMacParams } from '@/lib/macs';
import type { CaptureCreateResponse } from './types';

export interface StreamingCaptureFinal {
  type: 'final';
  capture: CaptureCreateResponse;
  refinement_complete: true;
  refinement_error?: string;
  degraded_reason?: string;
}

const MAX_AHEAD_OF_CLOCK_S = 1;

/** One socket owns one take. Failure before finish can safely use batch upload. */
export class CaptureStream {
  private socket: WebSocket;
  private pending: ArrayBuffer[] = [];
  private pendingBytes = 0;
  private sequence = 0;
  private sampleOffset = 0;
  private ready = false;
  private failed = false;
  private cancelled = false;
  private terminalError?: string;
  private finished = false;
  private sessionId?: string;
  private finishPromise?: Promise<StreamingCaptureFinal | null>;
  private resolveReady!: (ready: boolean) => void;
  private resolveFinal!: (result: StreamingCaptureFinal | null) => void;
  private readyResult = new Promise<boolean>((resolve) => {
    this.resolveReady = resolve;
  });
  private finalResult = new Promise<StreamingCaptureFinal | null>((resolve) => {
    this.resolveFinal = resolve;
  });
  private handshakeTimer: ReturnType<typeof setTimeout>;
  private startedAt = performance.now();

  constructor(
    private baseUrl: string,
    private sampleRate: number,
    private onFallback?: () => void,
  ) {
    const url = new URL(`${baseUrl.replace(/\/$/, '')}/captures/stream`);
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
    this.socket = new WebSocket(withMacParams(url));
    this.handshakeTimer = setTimeout(() => this.fail(), 5000);
    this.socket.onopen = () =>
      this.socket.send(
        JSON.stringify({
          type: 'start',
          protocol_version: 1,
          sample_rate: sampleRate,
          channels: 1,
          encoding: 'pcm_s16le',
          source: 'dictation',
        }),
      );
    this.socket.onmessage = ({ data }) => {
      try {
        const event = JSON.parse(data);
        if (event.type === 'ready' && !this.ready && typeof event.session_id === 'string') {
          clearTimeout(this.handshakeTimer);
          this.sessionId = event.session_id;
          this.ready = true;
          for (const frame of this.pending) this.socket.send(frame);
          this.pending = [];
          this.pendingBytes = 0;
          this.resolveReady(true);
        } else if (
          event.type === 'final' &&
          event.refinement_complete === true &&
          event.capture?.id === this.sessionId &&
          typeof event.capture?.transcript_raw === 'string'
        ) {
          this.resolveFinal(event as StreamingCaptureFinal);
          this.socket.close();
        } else if (event.type === 'error') {
          if (this.finished) this.terminalError = event.message || 'Streaming finalization failed';
          this.fail();
        } else if (event.degraded_reason) this.onFallback?.();
      } catch {
        this.fail();
      }
    };
    this.socket.onerror = () => this.fail();
    this.socket.onclose = () => this.fail();
  }

  append(pcm: ArrayBuffer) {
    if (this.failed || this.finished) return;
    const frame = new ArrayBuffer(8 + pcm.byteLength);
    const view = new DataView(frame);
    view.setUint32(0, this.sequence++, true);
    view.setUint32(4, this.sampleOffset, true);
    this.sampleOffset += pcm.byteLength / 2;
    // Speech can't arrive faster than it is spoken. A freshly connected audio
    // graph in WebKit can replay minutes of empty audio at once, burying the
    // take; the complete recording is intact, so let it take over.
    const elapsed = (performance.now() - this.startedAt) / 1000;
    if (this.sampleOffset > (elapsed + MAX_AHEAD_OF_CLOCK_S) * this.sampleRate) {
      this.fail();
      return;
    }
    new Uint8Array(frame, 8).set(new Uint8Array(pcm));
    if (this.pendingBytes + this.socket.bufferedAmount + frame.byteLength > 1024 * 1024) {
      this.fail();
      return;
    }
    if (this.ready && this.socket.readyState === WebSocket.OPEN) {
      try {
        this.socket.send(frame);
      } catch {
        this.fail();
      }
    } else {
      this.pending.push(frame);
      this.pendingBytes += frame.byteLength;
    }
  }

  cancel() {
    this.cancelled = true;
    this.fail();
  }

  private fail() {
    if (this.failed) return;
    this.failed = true;
    if (!this.finished) this.onFallback?.();
    clearTimeout(this.handshakeTimer);
    this.pending = [];
    this.pendingBytes = 0;
    this.resolveReady(false);
    this.resolveFinal(null);
    this.socket.close();
  }

  finish(): Promise<StreamingCaptureFinal | null> {
    this.finishPromise ??= this.finishOnce();
    return this.finishPromise;
  }

  private async finishOnce(): Promise<StreamingCaptureFinal | null> {
    if (!(await this.readyResult) || this.failed) return null;
    try {
      this.socket.send(JSON.stringify({ type: 'finish' }));
    } catch {
      this.fail();
      return null;
    }
    this.finished = true;
    const timeout = setTimeout(() => this.fail(), 120_000);
    const result = await this.finalResult;
    clearTimeout(timeout);
    if (this.cancelled) return null;
    if (result) return result;
    if (this.terminalError) throw new Error(this.terminalError);
    // Never retry a batch POST after finish: the capture may already be saved.
    if (this.sessionId) {
      for (let attempt = 0; attempt < 20; attempt++) {
        if (this.cancelled) return null;
        try {
          const response = await fetch(
            `${this.baseUrl.replace(/\/$/, '')}/captures/stream/${this.sessionId}/result`,
            {
              signal: AbortSignal.timeout(5000),
            },
          );
          if (response.ok) {
            const recovered = await response.json();
            if (this.cancelled) return null;
            if (recovered.type === 'error' && recovered.session_id === this.sessionId) {
              this.terminalError = recovered.message || 'Streaming finalization failed';
            }
            if (
              recovered.type === 'final' &&
              recovered.refinement_complete === true &&
              recovered.capture?.id === this.sessionId
            )
              return recovered;
          }
        } catch {
          /* A short disconnect can recover without repeating the capture. */
        }
        if (this.terminalError) throw new Error(this.terminalError);
        await new Promise((resolve) => setTimeout(resolve, 1000));
      }
    }
    throw new Error('Streaming finalization interrupted. Check Captures before recording again.');
  }
}
