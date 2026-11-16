/**
 * Pill state for one native take, emitted by Rust (`dictation/mod.rs`).
 * ``take`` increases per recording so a take that is still finishing can't
 * overwrite the pill of a newer one. The dictate pill itself is native
 * (crates/kass-pill); these screens still listen for the same events.
 */
export type NativeDictationEvent =
  | { take: number; state: 'preparing' | 'recording' | 'refining' | 'done' | 'cancelled' }
  | { take: number; state: 'transcribing'; elapsed_ms: number }
  | { take: number; state: 'error'; message: string; visible_ms: number };
