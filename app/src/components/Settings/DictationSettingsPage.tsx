import { invoke } from '@tauri-apps/api/core';
import { type ReactNode, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AccessibilityNotice } from '@/components/AccessibilityGate/AccessibilityGate';
import { DictationReadinessChecklist } from '@/components/CapturesTab/DictationReadinessChecklist';
import { ChordPicker } from '@/components/ChordPicker/ChordPicker';
import { InputMonitoringNotice } from '@/components/InputMonitoringGate/InputMonitoringGate';
import { SettingRow, SettingSection } from '@/components/ServerTab/SettingRow';
import { Button } from '@/components/ui/button';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { Toggle } from '@/components/ui/toggle';
import { useToast } from '@/components/ui/use-toast';
import { useAudioInputDevices } from '@/lib/hooks/useAudioInputDevices';
import { useDictationReadiness } from '@/lib/hooks/useDictationReadiness';
import { inputDevicePickerValue, useNativeInputDevices } from '@/lib/hooks/useNativeInputDevices';
import { useCaptureSettings } from '@/lib/hooks/useSettings';
import { defaultChordKeys } from '@/lib/utils/keyCodes';
import { usePlatform } from '@/platform/PlatformContext';
import { ChordKeys } from './ChordKeys';
import { HudPreview } from './HudPreview';

type ChordMode = 'push' | 'toggle';

/** Dictation settings: shortcut, microphone, HUD, paste. */
export function DictationSettingsPage() {
  const { t } = useTranslation();
  const { settings, update } = useCaptureSettings();
  const readiness = useDictationReadiness();
  const hotkeyEnabled = settings?.hotkey_enabled ?? false;
  const allowAutoPaste = settings?.allow_auto_paste ?? true;
  const voiceEdits = settings?.voice_edits ?? true;
  const pushToTalkKeys = settings?.chord_push_to_talk_keys ?? defaultChordKeys('push');
  const toggleToTalkKeys = settings?.chord_toggle_to_talk_keys ?? defaultChordKeys('toggle');
  const [chordEditor, setChordEditor] = useState<ChordMode | null>(null);

  return (
    <>
      {/* Same checklist the Captures empty state uses, so a missing model or
          permission can't hide behind a green toggle. Gone once all is ready. */}
      {!readiness.allReady && (
        <section className="mb-7 rounded-lg border border-warning/30 bg-warning/[0.06] p-4">
          <h2 className="mb-3 text-sm font-medium">{t('captures.readiness.title')}</h2>
          <DictationReadinessChecklist readiness={readiness} compact />
        </section>
      )}

      <SettingSection title={t('settings.captures.dictation.sectionShortcut')}>
        <div className="py-3.5">
          <GlobalShortcutRow enabled={hotkeyEnabled} />
          <InputMonitoringNotice enabled={hotkeyEnabled} />
        </div>

        <SettingRow
          title={t('settings.captures.dictation.pushToTalk.title')}
          description={t('settings.captures.dictation.pushToTalk.description')}
          action={
            <ChordAction
              keys={pushToTalkKeys}
              disabled={!hotkeyEnabled}
              label={t('settings.captures.dictation.pushToTalk.changeLabel')}
              onChange={() => setChordEditor('push')}
            />
          }
        />

        <SettingRow
          title={t('settings.captures.dictation.toggle.title')}
          description={t('settings.captures.dictation.toggle.description')}
          action={
            <ChordAction
              keys={toggleToTalkKeys}
              disabled={!hotkeyEnabled}
              label={t('settings.captures.dictation.toggle.changeLabel')}
              onChange={() => setChordEditor('toggle')}
            />
          }
        />
      </SettingSection>

      <ChordPicker
        open={chordEditor === 'push'}
        title={t('settings.captures.dictation.chordPicker.pttTitle')}
        description={t('settings.captures.dictation.chordPicker.pttDescription')}
        initialKeys={pushToTalkKeys}
        onCancel={() => setChordEditor(null)}
        onSave={(keys) => {
          update({ chord_push_to_talk_keys: keys });
          setChordEditor(null);
        }}
      />

      <ChordPicker
        open={chordEditor === 'toggle'}
        title={t('settings.captures.dictation.chordPicker.toggleTitle')}
        description={t('settings.captures.dictation.chordPicker.toggleDescription')}
        initialKeys={toggleToTalkKeys}
        onCancel={() => setChordEditor(null)}
        onSave={(keys) => {
          update({ chord_toggle_to_talk_keys: keys });
          setChordEditor(null);
        }}
      />

      <SettingSection title={t('settings.captures.dictation.sectionInput')}>
        <MicrophoneRow />
        <SettingRow
          title={t('settings.captures.dictation.preview.title')}
          description={t('settings.captures.dictation.preview.description')}
          action={<HudPreview enabled={hotkeyEnabled} />}
        />
        <SoundCueRows />
      </SettingSection>

      <SettingSection title={t('settings.captures.dictation.sectionOutput')}>
        <div className="py-3.5">
          <RowLayout
            htmlFor="autoPaste"
            title={t('settings.captures.dictation.autoPaste.title')}
            description={t('settings.captures.dictation.autoPaste.description')}
          >
            <Toggle
              id="autoPaste"
              checked={allowAutoPaste}
              onCheckedChange={(v) => update({ allow_auto_paste: v })}
              disabled={!hotkeyEnabled}
            />
          </RowLayout>
          <AccessibilityNotice />
        </div>
        <SettingRow
          title={t('settings.captures.dictation.voiceEdits.title')}
          description={t('settings.captures.dictation.voiceEdits.description')}
          htmlFor="voiceEdits"
          action={
            <Toggle
              id="voiceEdits"
              checked={voiceEdits}
              onCheckedChange={(v) => update({ voice_edits: v })}
              disabled={!hotkeyEnabled}
            />
          }
        />
      </SettingSection>
    </>
  );
}

/**
 * The master switch for the global shortcut. Turning it on while a model is
 * missing says so here, since otherwise the chord would just do nothing.
 */
function GlobalShortcutRow({ enabled }: { enabled: boolean }) {
  const { t } = useTranslation();
  const { toast } = useToast();
  const { update } = useCaptureSettings();
  const readiness = useDictationReadiness();

  const onCheckedChange = (v: boolean) => {
    update({ hotkey_enabled: v });
    // The Input Monitoring notice covers the permission, but a missing model
    // would otherwise be invisible from this page: useChordSync gates on it.
    if (!v) return;
    const missingModels = readiness.missing.filter((g) => g === 'stt' || g === 'llm');
    if (missingModels.length === 0) return;
    const names = [
      missingModels.includes('stt') ? readiness.stt?.display_name : null,
      missingModels.includes('llm') ? readiness.llm?.display_name : null,
    ]
      .filter(Boolean)
      .join(' and ');
    toast({
      title: t('captures.toast.shortcutNotArmed'),
      description: t('captures.toast.shortcutNotArmedDescription', {
        names,
        count: missingModels.length,
      }),
    });
  };

  return (
    <RowLayout
      htmlFor="hotkeyEnabled"
      title={t('settings.captures.dictation.globalShortcut.title')}
      description={t('settings.captures.dictation.globalShortcut.description')}
    >
      <Toggle id="hotkeyEnabled" checked={enabled} onCheckedChange={onCheckedChange} />
    </RowLayout>
  );
}

/**
 * SettingRow's label-and-control line without its padding, for rows that
 * carry a permission notice underneath inside the same divider.
 */
function RowLayout({
  htmlFor,
  title,
  description,
  children,
}: {
  htmlFor: string;
  title: string;
  description: string;
  children: ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-8">
      <div className="min-w-0">
        <label htmlFor={htmlFor} className="cursor-pointer select-none text-sm leading-none">
          {title}
        </label>
        <p className="mt-1 text-xs leading-relaxed text-muted-foreground">{description}</p>
      </div>
      <div className="shrink-0">{children}</div>
    </div>
  );
}

function ChordAction({
  keys,
  disabled,
  label,
  onChange,
}: {
  keys: string[];
  disabled: boolean;
  label: string;
  onChange: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div className="flex items-center gap-1.5">
      <ChordKeys keys={keys} />
      <Button
        variant="outline"
        className="ml-1.5 h-[30px] px-2.5 text-xs"
        disabled={disabled}
        aria-label={label}
        onClick={onChange}
      >
        {t('settings.captures.dictation.pushToTalk.change')}
      </Button>
    </div>
  );
}

/**
 * The dictation chimes, played natively by Rust (see useSoundCueSync). The
 * volume is saved and previewed when the slider is let go, not while it moves.
 */
function SoundCueRows() {
  const { t } = useTranslation();
  const platform = usePlatform();
  const { settings, update } = useCaptureSettings();
  const enabled = settings?.sound_cues ?? true;
  const saved = settings?.sound_cue_volume ?? 0.5;
  const [volume, setVolume] = useState(saved);
  useEffect(() => setVolume(saved), [saved]);

  if (!platform.metadata.isTauri) return null;

  const commit = () => {
    if (volume === saved) return;
    update({ sound_cue_volume: volume });
    invoke('preview_sound_cue', { volume }).catch((err) =>
      console.warn('[sound-cues] preview failed:', err),
    );
  };

  return (
    <>
      <SettingRow
        htmlFor="soundCues"
        title={t('settings.captures.dictation.soundCues.title')}
        description={t('settings.captures.dictation.soundCues.description')}
        action={
          <Toggle
            id="soundCues"
            checked={enabled}
            onCheckedChange={(v) => update({ sound_cues: v })}
          />
        }
      />
      <SettingRow
        htmlFor="soundCueVolume"
        title={t('settings.captures.dictation.soundCueVolume.title')}
        description={t('settings.captures.dictation.soundCueVolume.description')}
        action={
          <input
            id="soundCueVolume"
            type="range"
            min={0}
            max={1}
            step={0.05}
            value={volume}
            disabled={!enabled}
            onChange={(e) => setVolume(Number(e.target.value))}
            onPointerUp={commit}
            onKeyUp={commit}
            className="w-[240px] cursor-pointer accent-accent disabled:cursor-not-allowed disabled:opacity-50"
          />
        }
      />
    </>
  );
}

/**
 * The desktop app records natively, so it lists native devices and saves
 * their stable ids; the web build keeps browser device ids.
 */
function MicrophoneRow() {
  const { t } = useTranslation();
  const platform = usePlatform();
  const { settings, update } = useCaptureSettings();
  const isTauri = platform.metadata.isTauri;
  const inputDeviceId = settings?.input_device_id ?? null;
  const { devices: browserInputDevices } = useAudioInputDevices();
  const { devices: nativeInputDevices } = useNativeInputDevices(isTauri);
  const inputDevices = isTauri ? nativeInputDevices : browserInputDevices;
  const inputDeviceValue = isTauri
    ? inputDevicePickerValue(inputDeviceId, inputDevices)
    : (inputDeviceId ?? 'default');

  return (
    <SettingRow
      title={t('settings.captures.dictation.inputDevice.title')}
      description={t('settings.captures.dictation.inputDevice.description')}
      action={
        <Select
          value={inputDeviceValue}
          onValueChange={(v) => update({ input_device_id: v === 'default' ? null : v })}
        >
          <SelectTrigger
            className="h-8 w-[240px]"
            aria-label={t('settings.captures.dictation.inputDevice.title')}
          >
            <SelectValue placeholder={t('settings.captures.dictation.inputDevice.default')} />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="default">
              {t('settings.captures.dictation.inputDevice.default')}
            </SelectItem>
            {inputDevices.map((d) => (
              <SelectItem key={d.deviceId} value={d.deviceId}>
                {d.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      }
    />
  );
}
