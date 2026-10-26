import { invoke } from '@tauri-apps/api/core';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Toggle } from '@/components/ui/toggle';
import { useToast } from '@/components/ui/use-toast';
import { useBetaFeature } from '@/lib/betaFeatures';
import { answersAsKass, normalizeServerAddress } from '@/lib/serverAddress';
import { usePlatform } from '@/platform/PlatformContext';
import { useServerStore } from '@/stores/serverStore';
import { SettingRow, SettingSection } from './SettingRow';

/**
 * Beta `network`: share this Mac's Kass with other Macs on the network, or
 * use another Mac's instead of this one's. Anyone on the network can reach a
 * shared Kass; there's no sign-in.
 */
export function NetworkSection() {
  const { t } = useTranslation();
  const beta = useBetaFeature('network');
  const remote = useServerStore((state) => state.remoteServerUrl);
  if (!beta) return null;
  return (
    <SettingSection
      title={t('settings.general.network.title')}
      description={t('settings.general.network.description')}
    >
      {!remote && <ShareOnNetworkRow />}
      <UseAnotherMacRow />
    </SettingSection>
  );
}

/** Run this Mac's server for other Macs too: restarts it to listen for them. */
function ShareOnNetworkRow() {
  const { t } = useTranslation();
  const { toast } = useToast();
  const platform = usePlatform();
  const share = useServerStore((state) => state.shareOnNetwork);
  const setShare = useServerStore((state) => state.setShareOnNetwork);
  const [saving, setSaving] = useState(false);
  const [address, setAddress] = useState<string | null>(null);

  useEffect(() => {
    if (!share) return;
    invoke<string>('network_address')
      .then(setAddress)
      .catch(() => setAddress(null));
  }, [share]);

  const onCheckedChange = async (next: boolean) => {
    setSaving(true);
    setShare(next);
    try {
      await platform.lifecycle.restartServer();
    } catch (error) {
      setShare(!next);
      toast({
        title: t('settings.general.network.share.failed'),
        description: String(error),
        variant: 'destructive',
      });
    } finally {
      setSaving(false);
    }
  };

  return (
    <SettingRow
      title={t('settings.general.network.share.title')}
      description={
        share && address
          ? t('settings.general.network.share.on', { address })
          : t('settings.general.network.share.description')
      }
      htmlFor="shareOnNetwork"
      action={
        <Toggle
          id="shareOnNetwork"
          checked={share}
          disabled={saving}
          onCheckedChange={onCheckedChange}
        />
      }
    />
  );
}

/** Point this Mac at another Mac's server, then restart onto it. */
function UseAnotherMacRow() {
  const { t } = useTranslation();
  const { toast } = useToast();
  const platform = usePlatform();
  const remote = useServerStore((state) => state.remoteServerUrl);
  const setRemote = useServerStore((state) => state.setRemoteServerUrl);
  const [draft, setDraft] = useState('');
  const [busy, setBusy] = useState(false);

  const restartOnto = async (url: string | null) => {
    const previous = remote;
    setRemote(url);
    try {
      await platform.lifecycle.restartApp();
    } catch (error) {
      setRemote(previous);
      setBusy(false);
      toast({
        title: t('settings.general.restart.failed'),
        description: String(error),
        variant: 'destructive',
      });
    }
  };

  const connect = async () => {
    const url = normalizeServerAddress(draft);
    if (!url) {
      toast({ title: t('settings.general.network.remote.invalid'), variant: 'destructive' });
      return;
    }
    setBusy(true);
    if (!(await answersAsKass(url))) {
      setBusy(false);
      toast({
        title: t('settings.general.network.remote.unreachable', { url }),
        description: t('settings.general.network.remote.unreachableHint'),
        variant: 'destructive',
      });
      return;
    }
    await restartOnto(url);
  };

  if (remote) {
    return (
      <SettingRow
        title={t('settings.general.network.remote.title')}
        description={t('settings.general.network.remote.using', { url: remote })}
        action={
          <Button
            variant="outline"
            size="sm"
            disabled={busy}
            onClick={() => {
              setBusy(true);
              void restartOnto(null);
            }}
          >
            {t('settings.general.network.remote.stop')}
          </Button>
        }
      />
    );
  }

  return (
    <SettingRow
      title={t('settings.general.network.remote.title')}
      description={t('settings.general.network.remote.description')}
    >
      <form
        className="mt-3 flex gap-2"
        onSubmit={(event) => {
          event.preventDefault();
          void connect();
        }}
      >
        <Input
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          placeholder="studio.local"
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          disabled={busy}
        />
        <Button type="submit" variant="outline" size="sm" disabled={busy || !draft.trim()}>
          {t(
            busy
              ? 'settings.general.network.remote.connecting'
              : 'settings.general.network.remote.connect',
          )}
        </Button>
      </form>
    </SettingRow>
  );
}
