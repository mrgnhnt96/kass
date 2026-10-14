// API Types matching backend Pydantic models

export type WhisperModelSize = 'base' | 'small' | 'medium' | 'large' | 'turbo';

export type Qwen3ModelSize = '0.6B' | '1.7B' | '4B';

/** ``command``: a Command Mode rewrite of selected text (docs/plans/COMMAND_MODE.md). */
export type CaptureSource = 'dictation' | 'recording' | 'file' | 'command';

/** A saved Command Mode instruction, run by saying its name or from ⌘K. */
export interface Transform {
  id: string;
  name: string;
  instruction: string;
}

/**
 * Snapshot of the accessibility-focused UI element at chord-start. Emitted
 * from Rust as part of the ``dictate:start`` payload so the frontend can
 * pass it back to ``paste_final_text`` once the final text is ready.
 */
export interface FocusSnapshot {
  pid: number;
  bundle_id: string | null;
  app_name: string | null;
  role: string | null;
}

export type PunctuationStyle = 'standard' | 'casual' | 'learned';

/** Days of capture history to keep; 0 keeps it forever. */
export type HistoryRetentionDays = 0 | 7 | 30 | 90 | 365;

/** Stable codes for learned punctuation habits; the app words them. */
export type WritingStyleHabit =
  | 'boundary_period'
  | 'boundary_comma'
  | 'boundary_none'
  | 'lowercase_start'
  | 'drop_intro_comma'
  | 'drop_conjunction_comma'
  | 'drop_final_period';

export interface WritingStyleStatus {
  ready: boolean;
  runs: number;
  last_run_at: string | null;
  example_count: number;
  habits: WritingStyleHabit[];
}

export interface PersonalExample {
  id: string;
  source: 'correction' | 'calibration';
  said: string;
  meant: string;
  created_at: string | null;
  /** The app a correction was made in. */
  app_bundle_id?: string | null;
  app_name?: string | null;
}

/** A named writing style that apps are assigned to (docs/plans/PER_APP_STYLE.md). */
export interface WritingStyle {
  id: string;
  name: string;
  position: number;
  /** The style of every app the user hasn't assigned. */
  is_default: boolean;
  punctuation_style: PunctuationStyle;
  preserve_technical: boolean;
  /** How the user says they write in this style's apps; cleanup follows it. */
  description: string;
}

/**
 * An app and the style its dictation uses; `confirmed` once the user chose
 * it. `corrections` counts its captures whose corrections teach that style;
 * `suggested_style_id` is what most apps of its App Store category use.
 */
export interface StyledApp {
  bundle_id: string;
  name?: string | null;
  style_id: string;
  confirmed: boolean;
  count: number;
  corrections: number;
  suggested_style_id?: string | null;
}

export interface WritingStylesResponse {
  styles: WritingStyle[];
  apps: StyledApp[];
  max_styles: number;
  /** Memory one style's cached prompt takes in the cleanup model, estimated. */
  cache_mb_per_style?: number | null;
}

/** What happens to an app's corrections when it moves to another style. */
export type MovedCorrections = 'bring' | 'leave';

export type WritingStyleUpdate = Partial<
  Pick<WritingStyle, 'name' | 'punctuation_style' | 'preserve_technical' | 'description'>
> & { is_default?: true };

/** Where a dictionary entry applies. Most specific wins: app, then style, then everywhere. */
export type DictionaryScope = 'global' | 'style' | 'app';

/** One place an entry applies: everywhere, a writing style (`scope_id` its id) or an app (its bundle id). */
export interface DictionaryPlace {
  scope: DictionaryScope;
  scope_id: string | null;
  app_name: string | null;
}

export interface DictionaryPlaceInput {
  scope: DictionaryScope;
  scope_id?: string | null;
  app_name?: string | null;
}

/**
 * A word dictation should get right, in one or more places. A spelling has
 * only `written` (helps speech-to-text hear it and fixes its capitals); a
 * replacement also has `spoken`, what the user says to get `written`.
 */
export interface DictionaryEntry {
  id: string;
  written: string;
  spoken: string | null;
  places: DictionaryPlace[];
  created_at: string;
  /** Off: only fixed where spelled exactly, never swapped in for a word that sounds like it. */
  match_sound: boolean;
  source: DictionarySource;
  /** A phrase: `written` is text inserted exactly, line breaks and all, where `spoken` is said. */
  phrase: boolean;
}

/** Who added an entry: the user, or a word they spelled aloud to fix it. Editing makes it the user's. */
export type DictionarySource = 'user' | 'spoken_fix' | 'correction';

export interface DictionaryListResponse {
  /** Newest first. */
  entries: DictionaryEntry[];
}

export interface DictionaryEntryCreate {
  written: string;
  spoken?: string | null;
  /** At least one; with `global`, the server keeps only that. */
  places: DictionaryPlaceInput[];
  match_sound?: boolean;
  /** Needs `spoken`. */
  phrase?: boolean;
}

export interface DictionaryEntryUpdate {
  written?: string;
  spoken?: string | null;
  places?: DictionaryPlaceInput[];
  match_sound?: boolean;
}

/** One place of an entry that applies in an app; `overridden` when a more specific entry wins. */
export interface ResolvedDictionaryEntry {
  id: string;
  written: string;
  spoken: string | null;
  scope: DictionaryScope;
  scope_id: string | null;
  app_name: string | null;
  created_at: string;
  match_sound: boolean;
  phrase: boolean;
  overridden: boolean;
}

export interface ResolvedDictionaryResponse {
  /** Most specific first: the app's own, its style's, then everywhere's. */
  entries: ResolvedDictionaryEntry[];
  /** Terms given to speech-to-text as a hint. */
  prompt_terms: string[];
  /** Terms that did not fit in the speech-to-text prompt. */
  dropped_terms: string[];
}

export interface CorrectionNote {
  id: string;
  text: string;
}

export interface CorrectionNotesStatus {
  notes: CorrectionNote[];
  /** Examples that left the prompt and have not been summarized yet. */
  pending: number;
  last_run: string | null;
  outcome: string;
}

/** The kinds of conversation teaching uses (docs/plans/TEACH_BY_REPLYING.md). */
export type TeachKind =
  | 'coding_agent'
  | 'design_feedback'
  | 'notes'
  | 'writeup'
  | 'team_chat'
  | 'issue_comment'
  | 'email'
  | 'text_message';

/** The facts for the user's next reply, and sometimes a trick to try. */
export interface TeachNote {
  facts: string | null;
  answers: { ask: string; answer: string }[];
  trick: 'change_of_mind' | 'long' | null;
  example: string | null;
}

/** Something a reply showed about how the user writes; the app words `code`. */
export interface TeachChip {
  code: string;
  value: string | null;
}

export interface TeachConversation {
  id: string;
  kind: TeachKind;
  persona: string;
  relation: string;
  title: string | null;
  messages: { from_you: boolean; text: string }[];
  note: TeachNote | null;
  reply_count: number;
  chips: TeachChip[];
  wrapped: boolean;
}

export interface TeachSession {
  session_id: string;
  style_id: string;
  /** Replies to aim for; finishing works after one. */
  target: number;
  replies: number;
  /** The kinds that match the style's apps. */
  suggested_kinds: TeachKind[];
  conversations: TeachConversation[];
}

export interface TeachDictated {
  text: string | null;
}

export interface TeachFinishResult {
  status: WritingStyleStatus;
  /** One of the user's dictations cleaned up before and after; null without one. */
  before: string | null;
  after: string | null;
  replies: number;
  /** Replies that were dictated, and so teach cleanup. */
  dictated: number;
}

export interface RefinementFlags {
  smart_cleanup: boolean;
  self_correction: boolean;
  preserve_technical: boolean;
  punctuation_style?: PunctuationStyle;
  capitalize_first?: boolean;
  /** The writing style whose habits, examples and rules cleanup used. */
  style?: string | null;
}

/** Why a capture's cleanup is flagged for the user to check. */
export interface RefinementReview {
  /** review: cleanup kept; reject: the transcript was used instead. */
  outcome: 'review' | 'reject';
  added: string[];
  missing: string[];
  reasons: ('answered' | 'dropped' | 'negation' | 'number' | 'person' | 'technical')[];
}

export interface CaptureResponse {
  id: string;
  audio_path: string;
  source: CaptureSource;
  language?: string | null;
  duration_ms?: number | null;
  transcript_raw: string;
  transcript_refined?: string | null;
  stt_model?: string | null;
  llm_model?: string | null;
  refinement_flags?: RefinementFlags | null;
  refinement_review?: RefinementReview | null;
  /** The app dictated into, when the capture came from the global shortcut. */
  app_bundle_id?: string | null;
  app_name?: string | null;
  /** Command captures: the text that was selected, the instruction that ran
   *  (a transform's, when one was named) and that transform's name. The
   *  rewrite is ``transcript_refined``; ``transcript_raw`` is what was said. */
  command_selection?: string | null;
  command_instruction?: string | null;
  command_transform?: string | null;
  /** The writing style the capture was cleaned up with. */
  style_id?: string | null;
  /** The recording was deleted once the transcript was saved (a ⌘K command never had one). */
  audio_deleted: boolean;
  /** The Mac that dictated it, for a Kass shared over the network (beta `network`). */
  mac_id?: string | null;
  mac_name?: string | null;
  created_at: string;
}

export interface CaptureListResponse {
  items: CaptureResponse[];
  total: number;
}

/** One app in the Captures app list. */
export interface CaptureAppCount {
  app_bundle_id: string;
  app_name?: string | null;
  count: number;
  last_captured_at?: string | null;
  /** The style the app's dictation uses; the default until the user chooses. */
  style_id?: string | null;
  confirmed: boolean;
  suggested_style_id?: string | null;
}

/**
 * Response of ``GET /captures/apps``. ``unknown_count`` is the captures with
 * no app recorded: uploads, and dictation from before apps were saved.
 */
export interface CaptureAppsResponse {
  total: number;
  unknown_count: number;
  apps: CaptureAppCount[];
}

/** A period of ``GET /captures/stats``. */
export type UsagePeriod = 'today' | '7d' | '30d' | 'all';

/** Words dictated on one local day ("2026-09-28"). */
export interface UsageDay {
  date: string;
  words: number;
}

/** One period's dictation, for all apps or one. */
export interface UsageTotals {
  words: number;
  captures: number;
  speaking_ms: number;
  /** Median pace of captures of 2 s and longer; null when there are none. */
  pace_wpm: number | null;
  /** Typing time the words would have taken, less the time spent speaking them. */
  time_saved_ms: number;
  /** Captures with a saved correction. */
  fixed_captures: number;
  weekdays_dictated: number;
  weekdays_in_period: number;
  weekend_days: UsageDay[];
  hours_dictated: number;
}

/**
 * One point of the words chart, at a local hour ("2026-09-28T14:00"), day or
 * week start. ``words`` is null for an hour still to come today.
 */
export interface UsagePoint {
  start: string;
  words: number | null;
  previous_start: string | null;
  previous_words: number | null;
}

/** One app's words in the period; a null bundle id is no app recorded. */
export interface UsageApp {
  app_bundle_id: string | null;
  words: number;
  captures: number;
}

export interface UsageLengths {
  bin_edges_s: number[];
  counts: number[];
  p25_ms: number | null;
  median_ms: number | null;
  p75_ms: number | null;
}

/**
 * Response of ``GET /captures/stats``, in the Mac's local days. ``current``
 * and everything after it follow the app filter; ``all_apps`` and ``apps``
 * are always every app's. ``heatmap`` is words by weekday (Monday first) and
 * hour.
 */
export interface UsageStatsResponse {
  period: UsagePeriod;
  bucket: 'hour' | 'day' | 'week';
  start: string;
  end: string;
  previous_start: string | null;
  previous_end: string | null;
  typing_wpm: number;
  current: UsageTotals;
  previous: UsageTotals | null;
  all_apps: UsageTotals;
  series: UsagePoint[];
  apps: UsageApp[];
  heatmap: number[][];
  lengths: UsageLengths;
}

/** Which captures the list shows: every app's, one app's, or those with no app. */
export type CaptureAppFilter =
  | { kind: 'all' }
  | { kind: 'app'; bundleId: string }
  | { kind: 'unknown' };

/**
 * Response of ``POST /captures``. Adds ``auto_refine`` and ``allow_auto_paste``
 * — the server's current settings captured at request time — so the client
 * can decide whether to chain a refine call and whether to fire the
 * synthetic-paste pipeline without relying on its own (possibly stale) copy
 * of capture_settings.
 */
export interface CaptureCreateResponse extends CaptureResponse {
  auto_refine: boolean;
  allow_auto_paste: boolean;
}

export interface CaptureRefineRequest {
  flags?: RefinementFlags;
  model_size?: Qwen3ModelSize;
}

export interface CaptureSettings {
  stt_model: WhisperModelSize;
  language: string;
  auto_refine: boolean;
  llm_model: Qwen3ModelSize;
  smart_cleanup: boolean;
  self_correction: boolean;
  preserve_technical: boolean;
  punctuation_style: PunctuationStyle;
  allow_auto_paste: boolean;
  /** Type cleaned text into the app while cleanup is still writing it. */
  live_text: boolean;
  /** Fix the last dictation by voice ("fix that, Morgan not Megan"). A beta feature. */
  voice_edits: boolean;
  /** Write "!" and drawn-out words ("wayyy") from how something was said. */
  expressive: boolean;
  /** Chime when dictation starts, stops or fails. */
  sound_cues: boolean;
  /** Chime volume, 0 to 1. */
  sound_cue_volume: number;
  /** Configured audio input deviceId (null or empty string means system default microphone). */
  input_device_id: string | null;
  /** Whether the global keyboard hotkey is armed. Off by default — turning
   *  this on triggers the macOS Input Monitoring TCC prompt. */
  hotkey_enabled: boolean;
  /** keytap key names. Defaults are platform-specific right-hand modifiers. */
  chord_push_to_talk_keys: string[];
  /** keytap key names. Toggle adds Space to the platform-specific PTT chord. */
  chord_toggle_to_talk_keys: string[];
  /** keytap key names for Command Mode; empty turns it off. */
  chord_command_keys: string[];
  command_transforms: Transform[];
  /** Days of capture history to keep; older captures are deleted, what they taught is kept. */
  history_retention_days: HistoryRetentionDays;
  /** Set by saving `history_retention_days`; nothing is deleted until then. */
  history_retention_confirmed: boolean;
  /** First-run onboarding was finished or closed (docs/plans/ONBOARDING.md). */
  onboarding_completed: boolean;
  /** Delete each recording once its transcript is saved. */
  discard_audio: boolean;
  /** Send anonymous daily usage counts (backend/services/usage_report.py). */
  share_usage: boolean;
  /** Start from the cleanup and voice models Kass ships (backend/services/shared_adapters.py). */
  shared_adapters: boolean;
}

export interface RetentionStatus {
  days: HistoryRetentionDays;
  confirmed: boolean;
  /** Captures the current window would delete once confirmed. */
  expiring: number;
}

export interface RetentionPreview {
  days: HistoryRetentionDays;
  /** Captures that keeping `days` would delete now. */
  expiring: number;
}

export type CaptureSettingsUpdate = Partial<CaptureSettings>;

/**
 * One row in the dictation readiness checklist. ``model_name`` is the
 * canonical id understood by ``POST /models/download`` so the UI can wire a
 * one-click "Download" button without a second lookup.
 */
export interface ModelReadiness {
  ready: boolean;
  model_name: string;
  display_name: string;
  size: string;
  size_mb?: number | null;
}

/** Backend half of the dictation readiness check. The frontend combines this
 *  with TCC permission state into the full checklist used by useDictationReadiness. */
export interface CaptureReadinessResponse {
  stt: ModelReadiness;
  llm: ModelReadiness;
}

export interface HealthResponse {
  status: string;
  model_loaded: boolean;
  model_downloaded?: boolean;
  model_size?: string;
  gpu_available: boolean;
  gpu_type?: string;
  backend_type?: string;
  version?: string;
  /** Server process start, Unix seconds. */
  started_at?: number;
  pid?: number;
  /** Peak resident memory of the server process. */
  peak_memory_mb?: number;
}

export interface ModelProgress {
  model_name: string;
  current: number;
  total: number;
  progress: number;
  filename?: string;
  status: 'downloading' | 'extracting' | 'complete' | 'error';
  timestamp: string;
  error?: string;
}

export interface ModelStatus {
  model_name: string;
  display_name: string;
  hf_repo_id?: string; // HuggingFace repository ID
  downloaded: boolean;
  downloading: boolean; // True if download is in progress
  size_mb?: number;
  loaded: boolean;
}

export interface HuggingFaceModelInfo {
  id: string;
  author: string;
  lastModified: string;
  pipeline_tag?: string;
  library_name?: string;
  downloads: number;
  likes: number;
  tags: string[];
  cardData?: {
    license?: string;
    language?: string[];
    pipeline_tag?: string;
  };
}

export interface ModelStatusListResponse {
  models: ModelStatus[];
}

export interface ModelDownloadRequest {
  model_name: string;
}

export interface ActiveDownloadTask {
  model_name: string;
  status: string;
  started_at: string;
  error?: string;
  progress?: number; // 0-100 percentage
  current?: number; // bytes downloaded
  total?: number; // total bytes
  filename?: string; // current file being downloaded
}

export interface ActiveTasksResponse {
  downloads: ActiveDownloadTask[];
}

/** How a report was made; a redictation teaches correction learning only. */
export type CaptureFeedbackSource = 'manual' | 'voice_fix' | 'redictation';

export interface CaptureFeedbackCreate {
  target: 'raw' | 'refined';
  expected_text: string;
  notes: string;
  snapshot: CaptureResponse;
  source?: CaptureFeedbackSource;
  /** A report of the same capture and target this one amends; it is replaced. */
  replaces?: string | null;
}

export interface CaptureFeedbackResponse extends CaptureFeedbackCreate {
  id: string;
  capture_id: string;
  source: CaptureFeedbackSource;
  created_at: string;
}

/** Mistakes per group of test recordings (voice_training/gate.py). */
export interface VoiceErrors {
  clean: number;
  noisy: number;
  correction: number;
}

export interface VoiceTrainingStatus {
  active: string | null;
  active_since: string | null;
  can_undo: boolean;
  min_train: number;
  min_test: number;
  bank: {
    takes: number;
    train: number;
    test: number;
    minutes: number;
    room_minutes: number;
  };
  sounds: {
    state: 'missing' | 'downloading' | 'ready' | 'failed';
    fraction: number;
    error: string | null;
  };
  metrics: {
    passed: boolean;
    reasons: string[];
    takes: number;
    base?: VoiceErrors;
    production?: VoiceErrors;
    candidate?: VoiceErrors;
  } | null;
  training: { updates: number; seconds: number; train_takes: number } | null;
}

export interface CorrectionLearningStatus {
  model?: {
    phase: string;
    revision: number;
    running: boolean;
    active_adapter: string | null;
    speech_model: string | null;
    can_rollback: boolean;
    counts: {
      train?: number;
      validation?: number;
      test?: number;
      audio_test?: number;
      speech_test?: number;
    };
    last_run: string | null;
    error: string | null;
    metrics: {
      adapter?: {
        passed: boolean;
        reasons: string[];
        baseline_errors: number;
        candidate_errors: number;
      };
    } | null;
    voice?: VoiceTrainingStatus;
  };
  evaluated_report_ids: string[];
  revision: number;
  active_rules: number;
  last_run: string | null;
  outcome: 'waiting' | 'updated' | 'no_change' | 'rolled_back';
  can_rollback: boolean;
  metrics: {
    examples: number;
    candidates: number;
    accepted: number;
    withdrawn: number;
    median_rule_ms: number;
    latency_passed: boolean;
  } | null;
}
