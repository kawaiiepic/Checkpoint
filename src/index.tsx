import { addEventListener, removeEventListener, callable, definePlugin, toaster } from '@decky/api';
import { ButtonItem, PanelSection, PanelSectionRow, staticClasses } from '@decky/ui';
import { useEffect, useState } from 'react';
import { FaCloud, FaGamepad } from 'react-icons/fa';

type Status = {
  running: boolean;
  game: string | null;
  save_roots: string[];
  last_update?: string | null;
  last_game?: string | null;
  watched_files: number;
  title_id?: string | null;
  process_count?: number;
  saves?: { path: string; profile: string; layout: string; file_count: number }[];
  error?: string | null;
};
type Change = { game: string; title_id?: string; changed_count: number };
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
      <PanelSectionRow><div>{status?.title_id ? `Title ID: ${status.title_id}` : 'Title ID not detected'}</div></PanelSectionRow>
      <PanelSectionRow><div>{status?.saves?.length ? status.saves.map((s) => `${s.layout} · ${s.profile}: ${s.path}`).join(' | ') : 'No matching save directory found'}</div></PanelSectionRow>
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
      {(error || status?.error) && <PanelSectionRow><div>{error || status?.error}</div></PanelSectionRow>}
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
  const gameListener = addEventListener<[{ game: string; title_id: string }]>("game_detected", (event) => {
    toaster.toast({title: "Eglantine game detected", body: `${event.game} [${event.title_id}]`, icon: <FaGamepad />});
  });
  return {
    name: 'Checkpoint',
    titleView: <div className={staticClasses.Title}>Checkpoint</div>,
    content: <Content />,
    icon: <FaGamepad />,
    onDismount() { removeEventListener('save_updated', listener); removeEventListener("game_detected", gameListener); },
  };
});
