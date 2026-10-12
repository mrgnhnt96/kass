import { invoke } from '@tauri-apps/api/core';
import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Toggle } from '@/components/ui/toggle';
import { useToast } from '@/components/ui/use-toast';
import { type UpdateChannel, useUpdateChannel, useUpdateChannelStore } from '@/lib/betaFeatures';
import { useUpdateCheck } from '@/lib/hooks/useUpdateCheck';
import { usePlatform } from '@/platform/PlatformContext';
import { remoteServerUrl, sharesOnNetwork } from '@/stores/serverStore';
import { SettingRow } from './SettingRow';

/**
 * Opt in to beta releases and beta features (tauri src-tauri/src/updater.rs,
 * lib/betaFeatures.ts). Turning it off keeps an installed beta until a
 * public release is newer.
 */
export function BetaUpdatesRow() {
  const { t } = useTranslation();
  const { toast } = useToast();
  const platform = usePlatform();
  const { version } = useUpdateCheck();
  const channel = useUpdateChannel();
  const setChannel = useUpdateChannelStore((state) => state.setChannel);
  const [saving, setSaving] = useState(false);

  const onCheckedChange = async (beta: boolean) => {
    setSaving(true);
    // Beta `network` stops with the beta: back onto this Mac's own server.
    const wasRemote = remoteServerUrl() !== null;
    const wasSharing = sharesOnNetwork();
    try {
      setChannel(
        await invoke<UpdateChannel>('set_update_channel', { channel: beta ? 'beta' : 'stable' }),
      );
      if (!beta && wasRemote) await platform.lifecycle.restartApp();
      else if (!beta && wasSharing) await platform.lifecycle.restartServer();
    } catch (error) {
      toast({
        title: t('settings.general.betaUpdates.failed'),
        description: String(error),
        variant: 'destructive',
      });
    } finally {
      setSaving(false);
    }
  };

  const onBetaVersion = version.includes('-beta.');
  return (
    <SettingRow
      title={t('settings.general.betaUpdates.title')}
      description={
        channel === 'stable' && onBetaVersion
          ? t('settings.general.betaUpdates.leaving', { version })
          : t('settings.general.betaUpdates.description')
      }
      htmlFor="betaUpdates"
      action={
        <Toggle
          id="betaUpdates"
          checked={channel === 'beta'}
          disabled={saving || channel === null}
          onCheckedChange={onCheckedChange}
        />
      }
    />
  );
}
