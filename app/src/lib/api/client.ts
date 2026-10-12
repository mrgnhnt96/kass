import type { LanguageCode } from '@/lib/constants/languages';
import { serverUrl } from '@/stores/serverStore';
import type {
  ActiveTasksResponse,
  CaptureAppFilter,
  CaptureAppsResponse,
  CaptureCreateResponse,
  CaptureFeedbackCreate,
  CaptureFeedbackResponse,
  CaptureListResponse,
  CaptureReadinessResponse,
  CaptureRefineRequest,
  CaptureResponse,
  CaptureSettings,
  CaptureSettingsUpdate,
  CaptureSource,
  CorrectionLearningStatus,
  CorrectionNotesStatus,
  DictionaryEntry,
  DictionaryEntryCreate,
  DictionaryEntryUpdate,
  DictionaryListResponse,
  HealthResponse,
  HistoryRetentionDays,
  ModelDownloadRequest,
  ModelStatusListResponse,
  MovedCorrections,
  PersonalExample,
  ResolvedDictionaryResponse,
  RetentionPreview,
  RetentionStatus,
  StyledApp,
  TeachDictated,
  TeachFinishResult,
  TeachKind,
  TeachSession,
  UsagePeriod,
  UsageStatsResponse,
  WhisperModelSize,
  WritingStyle,
  WritingStyleStatus,
  WritingStylesResponse,
  WritingStyleUpdate,
} from './types';

/** `?style=<id>` for the per-style writing style endpoints; none is the default style. */
function styleQuery(style?: string | null): string {
  return style ? `?style=${encodeURIComponent(style)}` : '';
}

function formatErrorDetail(detail: unknown, fallback: string): string {
  if (typeof detail === 'string') return detail;
  if (Array.isArray(detail)) {
    return detail
      .map((e: Record<string, unknown>) => e.msg || e.message || JSON.stringify(e))
      .join('; ');
  }
  if (detail && typeof detail === 'object') {
    const obj = detail as Record<string, unknown>;
    if (typeof obj.message === 'string') return obj.message;
    return JSON.stringify(detail);
  }
  return fallback;
}

/** A failed request; `status` is the HTTP status (409 for a duplicate, say). */
export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

class ApiClient {
  private getBaseUrl(): string {
    return serverUrl();
  }

  private async request<T>(endpoint: string, options?: RequestInit): Promise<T> {
    const url = `${this.getBaseUrl()}${endpoint}`;
    const response = await fetch(url, {
      ...options,
      headers: {
        'Content-Type': 'application/json',
        ...options?.headers,
      },
    });

    if (!response.ok) {
      const error = await response.json().catch(() => ({
        detail: response.statusText,
      }));
      throw new ApiError(
        formatErrorDetail(error.detail, `HTTP error! status: ${response.status}`),
        response.status,
      );
    }

    return response.json();
  }

  // Health
  async getHealth(): Promise<HealthResponse> {
    return this.request<HealthResponse>('/health');
  }

  async getInternal(): Promise<{ internal: boolean }> {
    return this.request('/internal');
  }

  async reportCaptureOutput(
    captureId: string,
    body: CaptureFeedbackCreate,
  ): Promise<CaptureFeedbackResponse> {
    return this.request(`/captures/${captureId}/feedback`, {
      method: 'POST',
      body: JSON.stringify(body),
    });
  }

  /** Withdraws one report and everything it taught. */
  async withdrawCaptureReport(captureId: string, reportId: string): Promise<void> {
    const response = await fetch(
      `${this.getBaseUrl()}/captures/${encodeURIComponent(captureId)}/feedback/${encodeURIComponent(reportId)}`,
      { method: 'DELETE' },
    );
    if (!response.ok) {
      throw new Error(`HTTP error! status: ${response.status}`);
    }
  }

  async listCaptureFeedback(captureId: string): Promise<CaptureFeedbackResponse[]> {
    return this.request(`/captures/${captureId}/feedback`);
  }

  async exportCaptureFeedback(): Promise<CaptureFeedbackResponse[]> {
    return this.request('/capture/feedback/export');
  }

  async correctionLearningStatus(): Promise<CorrectionLearningStatus> {
    return this.request('/capture/learning');
  }

  async runCorrectionLearning(): Promise<CorrectionLearningStatus> {
    return this.request('/capture/learning/run', { method: 'POST' });
  }

  async pauseLearningForRecording(): Promise<void> {
    await this.request('/capture/learning/activity', { method: 'POST' });
  }

  async cancelModelLearning(): Promise<void> {
    await this.request('/capture/learning/cancel', { method: 'POST' });
  }

  async rollbackCorrectionLearning(): Promise<CorrectionLearningStatus> {
    return this.request('/capture/learning/rollback', { method: 'POST' });
  }

  async downloadVoiceSounds(): Promise<void> {
    await this.request('/capture/learning/voice/sounds', { method: 'POST' });
  }

  // Captures
  async listCaptures(
    limit = 50,
    offset = 0,
    app: CaptureAppFilter = { kind: 'all' },
  ): Promise<CaptureListResponse> {
    const params = new URLSearchParams({ limit: String(limit), offset: String(offset) });
    if (app.kind === 'app') params.set('app_bundle_id', app.bundleId);
    if (app.kind === 'unknown') params.set('unknown_app', 'true');
    return this.request<CaptureListResponse>(`/captures?${params}`);
  }

  async listCaptureApps(): Promise<CaptureAppsResponse> {
    return this.request<CaptureAppsResponse>('/captures/apps');
  }

  async getCaptureStats(
    period: UsagePeriod,
    app: CaptureAppFilter = { kind: 'all' },
  ): Promise<UsageStatsResponse> {
    const params = new URLSearchParams({ period });
    if (app.kind === 'app') params.set('app_bundle_id', app.bundleId);
    if (app.kind === 'unknown') params.set('unknown_app', 'true');
    return this.request<UsageStatsResponse>(`/captures/stats?${params}`);
  }

  async getCapture(captureId: string): Promise<CaptureResponse> {
    return this.request<CaptureResponse>(`/captures/${captureId}`);
  }

  async createCapture(
    file: File,
    options?: {
      source?: CaptureSource;
      language?: LanguageCode;
      sttModel?: WhisperModelSize;
    },
  ): Promise<CaptureCreateResponse> {
    const formData = new FormData();
    formData.append('file', file);
    formData.append('source', options?.source ?? 'file');
    if (options?.language) formData.append('language', options.language);
    if (options?.sttModel) formData.append('stt_model', options.sttModel);

    const url = `${this.getBaseUrl()}/captures`;
    const response = await fetch(url, { method: 'POST', body: formData });
    if (!response.ok) {
      const error = await response.json().catch(() => ({
        detail: response.statusText,
      }));
      throw new Error(formatErrorDetail(error.detail, `HTTP error! status: ${response.status}`));
    }
    return response.json();
  }

  async deleteCapture(captureId: string): Promise<{ message: string }> {
    return this.request<{ message: string }>(`/captures/${captureId}`, {
      method: 'DELETE',
    });
  }

  async refineCapture(captureId: string, body: CaptureRefineRequest): Promise<CaptureResponse> {
    return this.request<CaptureResponse>(`/captures/${captureId}/refine`, {
      method: 'POST',
      body: JSON.stringify(body),
    });
  }

  getCaptureAudioUrl(captureId: string): string {
    return `${this.getBaseUrl()}/captures/${captureId}/audio`;
  }

  // Writing styles and the apps assigned to them
  async listWritingStyles(): Promise<WritingStylesResponse> {
    return this.request<WritingStylesResponse>('/writing-styles');
  }

  async createWritingStyle(name: string): Promise<WritingStyle> {
    return this.request<WritingStyle>('/writing-styles', {
      method: 'POST',
      body: JSON.stringify({ name }),
    });
  }

  async updateWritingStyle(styleId: string, patch: WritingStyleUpdate): Promise<WritingStyle> {
    return this.request<WritingStyle>(`/writing-styles/${encodeURIComponent(styleId)}`, {
      method: 'PATCH',
      body: JSON.stringify(patch),
    });
  }

  async deleteWritingStyle(styleId: string): Promise<void> {
    const response = await fetch(
      `${this.getBaseUrl()}/writing-styles/${encodeURIComponent(styleId)}`,
      { method: 'DELETE' },
    );
    if (!response.ok) {
      const error = await response.json().catch(() => ({ detail: response.statusText }));
      throw new Error(formatErrorDetail(error.detail, `HTTP error! status: ${response.status}`));
    }
  }

  async assignAppStyle(
    bundleId: string,
    styleId: string,
    appName?: string | null,
    corrections: MovedCorrections = 'bring',
  ): Promise<WritingStylesResponse> {
    return this.request<WritingStylesResponse>(
      `/writing-styles/apps/${encodeURIComponent(bundleId)}`,
      {
        method: 'PUT',
        body: JSON.stringify({ style_id: styleId, app_name: appName ?? null, corrections }),
      },
    );
  }

  async confirmApps(apps: Pick<StyledApp, 'bundle_id' | 'name'>[]): Promise<WritingStylesResponse> {
    return this.request<WritingStylesResponse>('/writing-styles/apps/confirm', {
      method: 'POST',
      body: JSON.stringify({
        apps: apps.map((app) => ({ bundle_id: app.bundle_id, app_name: app.name ?? null })),
      }),
    });
  }

  // Writing style: everything below is per style; none is the default style.
  async getWritingStyle(style?: string | null): Promise<WritingStyleStatus> {
    return this.request<WritingStyleStatus>(`/writing-style${styleQuery(style)}`);
  }

  async resetWritingStyle(style?: string | null): Promise<WritingStyleStatus> {
    return this.request<WritingStyleStatus>(`/writing-style${styleQuery(style)}`, {
      method: 'DELETE',
    });
  }

  async listPersonalExamples(style?: string | null): Promise<PersonalExample[]> {
    return this.request<PersonalExample[]>(`/writing-style/examples${styleQuery(style)}`);
  }

  async getCorrectionNotes(style?: string | null): Promise<CorrectionNotesStatus> {
    return this.request<CorrectionNotesStatus>(`/writing-style/notes${styleQuery(style)}`);
  }

  async removeCorrectionNote(noteId: string, style?: string | null): Promise<void> {
    const response = await fetch(
      `${this.getBaseUrl()}/writing-style/notes/${encodeURIComponent(noteId)}${styleQuery(style)}`,
      { method: 'DELETE' },
    );
    if (!response.ok) {
      throw new Error(`HTTP error! status: ${response.status}`);
    }
  }

  async removePersonalExample(exampleId: string): Promise<void> {
    const response = await fetch(
      `${this.getBaseUrl()}/writing-style/examples/${encodeURIComponent(exampleId)}`,
      { method: 'DELETE' },
    );
    if (!response.ok) {
      throw new Error(`HTTP error! status: ${response.status}`);
    }
  }

  // Teaching a style by replying to conversations (docs/plans/TEACH_BY_REPLYING.md)
  async startTeach(style?: string | null): Promise<TeachSession> {
    return this.request<TeachSession>(`/writing-style/teach${styleQuery(style)}`, {
      method: 'POST',
    });
  }

  async addTeachConversation(sessionId: string, kind: TeachKind): Promise<TeachSession> {
    return this.request<TeachSession>(`/writing-style/teach/${sessionId}/conversations`, {
      method: 'POST',
      body: JSON.stringify({ kind }),
    });
  }

  async newTeachTheme(sessionId: string, conversationId: string): Promise<TeachSession> {
    return this.request<TeachSession>(
      `/writing-style/teach/${sessionId}/conversations/${conversationId}/new-theme`,
      { method: 'POST' },
    );
  }

  async wrapTeachConversation(sessionId: string, conversationId: string): Promise<TeachSession> {
    return this.request<TeachSession>(
      `/writing-style/teach/${sessionId}/conversations/${conversationId}/wrap-up`,
      { method: 'POST' },
    );
  }

  /** Send a reply; the other side's next message comes back with the session. */
  async sendTeachReply(
    sessionId: string,
    conversationId: string,
    written: string,
  ): Promise<TeachSession> {
    return this.request<TeachSession>(
      `/writing-style/teach/${sessionId}/conversations/${conversationId}/replies`,
      { method: 'POST', body: JSON.stringify({ written }) },
    );
  }

  /** The cleanup of what was dictated in Kass's window this turn. */
  async teachDictated(sessionId: string, conversationId: string): Promise<TeachDictated> {
    return this.request<TeachDictated>(
      `/writing-style/teach/${sessionId}/conversations/${conversationId}/dictated`,
    );
  }

  /** Earlier dictation is no longer part of this conversation's reply. */
  async restartTeachTurn(sessionId: string, conversationId: string): Promise<void> {
    const response = await fetch(
      `${this.getBaseUrl()}/writing-style/teach/${sessionId}/conversations/${conversationId}/restart-turn`,
      { method: 'POST' },
    );
    if (!response.ok) throw new ApiError(`HTTP error! status: ${response.status}`, response.status);
  }

  async finishTeach(sessionId: string): Promise<TeachFinishResult> {
    return this.request<TeachFinishResult>(`/writing-style/teach/${sessionId}/finish`, {
      method: 'POST',
    });
  }

  async discardTeach(sessionId: string): Promise<void> {
    const response = await fetch(`${this.getBaseUrl()}/writing-style/teach/${sessionId}`, {
      method: 'DELETE',
    });
    if (!response.ok && response.status !== 404) {
      throw new Error(`HTTP error! status: ${response.status}`);
    }
  }

  // Dictionary: words dictation should get right, each in one or more places
  async listDictionary(): Promise<DictionaryListResponse> {
    return this.request<DictionaryListResponse>('/dictionary');
  }

  async createDictionaryEntry(body: DictionaryEntryCreate): Promise<DictionaryEntry> {
    return this.request<DictionaryEntry>('/dictionary', {
      method: 'POST',
      body: JSON.stringify(body),
    });
  }

  async updateDictionaryEntry(id: string, patch: DictionaryEntryUpdate): Promise<DictionaryEntry> {
    return this.request<DictionaryEntry>(`/dictionary/${encodeURIComponent(id)}`, {
      method: 'PATCH',
      body: JSON.stringify(patch),
    });
  }

  async deleteDictionaryEntry(id: string): Promise<{ deleted: true }> {
    return this.request<{ deleted: true }>(`/dictionary/${encodeURIComponent(id)}`, {
      method: 'DELETE',
    });
  }

  /** The entries that apply in an app: its own, its style's and everywhere's. */
  async resolveDictionary(bundleId: string): Promise<ResolvedDictionaryResponse> {
    return this.request<ResolvedDictionaryResponse>(
      `/dictionary/resolved?bundle_id=${encodeURIComponent(bundleId)}`,
    );
  }

  // Settings
  async getCaptureSettings(): Promise<CaptureSettings> {
    return this.request<CaptureSettings>('/settings/captures');
  }

  async getCaptureReadiness(): Promise<CaptureReadinessResponse> {
    return this.request<CaptureReadinessResponse>('/capture/readiness');
  }

  async updateCaptureSettings(patch: CaptureSettingsUpdate): Promise<CaptureSettings> {
    return this.request<CaptureSettings>('/settings/captures', {
      method: 'PUT',
      body: JSON.stringify(patch),
    });
  }

  async getRetentionStatus(): Promise<RetentionStatus> {
    return this.request<RetentionStatus>('/settings/captures/retention-status');
  }

  async getRetentionPreview(days: HistoryRetentionDays): Promise<RetentionPreview> {
    return this.request<RetentionPreview>(`/settings/captures/retention-preview?days=${days}`);
  }

  // Model Management
  async getModelStatus(): Promise<ModelStatusListResponse> {
    return this.request<ModelStatusListResponse>('/models/status');
  }

  async getModelsCacheDir(): Promise<{ path: string }> {
    return this.request<{ path: string }>('/models/cache-dir');
  }

  async migrateModels(
    destination: string,
  ): Promise<{ source: string; destination: string; moved: number; errors: string[] }> {
    return this.request('/models/migrate', {
      method: 'POST',
      body: JSON.stringify({ destination }),
    });
  }

  getMigrationProgressUrl(): string {
    return `${this.getBaseUrl()}/models/migrate/progress`;
  }

  async triggerModelDownload(modelName: string): Promise<{ message: string }> {
    console.log(
      '[API] triggerModelDownload called for:',
      modelName,
      'at',
      new Date().toISOString(),
    );
    const result = await this.request<{ message: string }>('/models/download', {
      method: 'POST',
      body: JSON.stringify({ model_name: modelName } as ModelDownloadRequest),
    });
    console.log('[API] triggerModelDownload response:', result);
    return result;
  }

  async deleteModel(modelName: string): Promise<{ message: string }> {
    return this.request<{ message: string }>(`/models/${modelName}`, {
      method: 'DELETE',
    });
  }

  async unloadModel(modelName: string): Promise<{ message: string }> {
    return this.request<{ message: string }>(`/models/${modelName}/unload`, {
      method: 'POST',
    });
  }

  /** Free MB on the volume models download to. */
  async getModelsDiskSpace(): Promise<{ free_mb: number }> {
    return this.request<{ free_mb: number }>('/models/disk-space');
  }

  /** Rewrite `selection` by a spoken or typed instruction (Command Mode without a recording). */
  async runCommand(selection: string, instruction: string): Promise<CaptureResponse> {
    return this.request<CaptureResponse>('/commands/run', {
      method: 'POST',
      body: JSON.stringify({ selection, instruction }),
    });
  }

  async cancelDownload(modelName: string): Promise<{ message: string }> {
    return this.request<{ message: string }>('/models/download/cancel', {
      method: 'POST',
      body: JSON.stringify({ model_name: modelName } as ModelDownloadRequest),
    });
  }

  // Task Management
  async getActiveTasks(): Promise<ActiveTasksResponse> {
    return this.request<ActiveTasksResponse>('/tasks/active');
  }

  async clearAllTasks(): Promise<{ message: string }> {
    return this.request<{ message: string }>('/tasks/clear', { method: 'POST' });
  }
}

export const apiClient = new ApiClient();
