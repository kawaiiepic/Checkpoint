import { addEventListener, removeEventListener, callable, definePlugin, toaster } from '@decky/api';
import { ButtonItem, PanelSection, PanelSectionRow, staticClasses } from '@decky/ui';
import { useEffect, useState } from 'react';
import { FaCloud, FaGamepad } from 'react-icons/fa';

type Status = {
  running: boolean;
  game: string | null;
  save_roots: string[];
  last_update: string | null;
  last_game: string | null;
  watched_files: number;
};
type Change = { game: string; when: string; changed_count: number };
const getStatus = callable<[], Status>('get_status');

function Content() {
  const [status, setStatus] = useState<Status | null>(null);
  const [error, setError] = useState('');
  const refresh = () => getStatus().then((result) => { setStatus(result); setError(''); }).catch((e) => setError(String(e)));

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), 5000);
    return () => clearInterval(timer);
  }, []);

  return <>
    <PanelSection title="Eglantine">
      <PanelSectionRow>
        <div>{status?.running ? `Playing: ${status.game ?? 'Unknown game'}` : 'No Eglantine game detected'}</div>
      </PanelSectionRow>
      <PanelSectionRow>
        <div>{status ? `Monitoring ${status.watched_files} save files` : 'Checking...'}</div>
      </PanelSectionRow>
      <PanelSectionRow>
        <div>{status?.last_update ? `Last local update: ${status.last_game} (${status.last_update})` : 'No save updates detected yet'}</div>
      </PanelSectionRow>
      <PanelSectionRow>
        <div style={{ fontSize: 'small', opacity: 0.7 }}>
          {status?.save_roots?.length ? `Save roots: ${status.save_roots.join(', ')}` : 'No Eglantine save directory found. See README.'}
        </div>
      </PanelSectionRow>
      <PanelSectionRow>
        <ButtonItem layout="below" onClick={() => void refresh()}>Refresh detection</ButtonItem>
      </PanelSectionRow>
      {error && <PanelSectionRow><div>{error}</div></PanelSectionRow>}
    </PanelSection>
    <PanelSection title="Prototype limitations">
      <PanelSectionRow>
        <div>Read-only local save notifications. No syncing or Steam library row injection yet.</div>
      </PanelSectionRow>
    </PanelSection>
  </>;
}

export default definePlugin(() => {
  const listener = addEventListener<[Change]>('save_updated', (change) => {
    toaster.toast({
      title: 'Updated local save detected',
      body: `${change.game} · ${change.changed_count} file(s) changed. No cloud sync performed.`,
      icon: <FaCloud />,
    });
  });
  return {
    name: 'Checkpoint',
    titleView: <div className={staticClasses.Title}>Checkpoint</div>,
    content: <Content />,
    icon: <FaGamepad />,
    onDismount() { removeEventListener('save_updated', listener); },
  };
});
