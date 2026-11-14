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
  available_saves?: number;
  available_titles?: number;
  game_running?: boolean;
  detection_source?: string | null;
  last_modified_ms?: number | null;
  backup_configured?: boolean;
  backed_up_games?: number;
  backup_error?: string | null;
  title_id?: string | null;
  process_count?: number;
  saves?: { path: string; profile: string; layout: string; file_count: number; last_modified_ms?: number | null }[];
  error?: string | null;
  last_sync?: string | null;
  last_scan?: string | null;
  activity?: { id: number; time: string; level: string; message: string }[];
};
type Change = { game: string; title_id?: string; changed_count: number };
const getStatus = callable<[], Status>('get_status');
const refreshDetection = callable<[], Status>('refresh_detection');
const clearActivity = callable<[], boolean>('clear_activity');

function formatSaveTime(ms?: number | null): string {
  if (ms == null || !Number.isFinite(ms)) return 'Unknown';
  return new Date(ms).toLocaleString();
}

function Content() {
  const [status, setStatus] = useState<Status | null>(null);
  const [error, setError] = useState('');
  const [refreshing, setRefreshing] = useState(false);
  const refresh = () => getStatus().then((result) => { setStatus(result); setError(''); }).catch((e) => setError(String(e)));
  const rescan = async () => {
    setRefreshing(true);
    try {
      const result = await refreshDetection();
      setStatus(result);
      setError(result.error ?? '');
    } catch (e) { setError(String(e)); }
    finally { setRefreshing(false); }
  };

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), 5000);
    return () => clearInterval(timer);
  }, []);

  return <>
    <PanelSection title="Eglantine">
      <PanelSectionRow>
        <div>{status?.game_running ? `Game active: ${status.game ?? 'Unknown game'}` : status?.running ? 'Eglantine running; game state uncertain, sync paused' : 'Eglantine closed; ready to sync'}</div>
      </PanelSectionRow>
      <PanelSectionRow>
        <div>{status ? `${status.available_saves ?? 0} local save director${(status.available_saves ?? 0) === 1 ? 'y' : 'ies'} discovered across ${status.available_titles ?? 0} game${(status.available_titles ?? 0) === 1 ? '' : 's'}` : 'Checking...'}</div>
      </PanelSectionRow>
      {status?.title_id && <PanelSectionRow><div>Title ID: {status.title_id} ({status.detection_source === "process-args" ? "launch arguments" : status.detection_source === "recent-boot-log" ? "boot log hint" : "detected"})</div></PanelSectionRow>}
      {status?.title_id && <PanelSectionRow><div>Monitoring {status.watched_files} files for current title</div></PanelSectionRow>}
      {status?.title_id && <PanelSectionRow><div>Last save modified: {formatSaveTime(status?.last_modified_ms)}</div></PanelSectionRow>}
      {status?.title_id && <PanelSectionRow><div>{status?.saves?.length ? status.saves.map((s) => `${s.layout} · ${s.profile}: ${s.path} (updated ${formatSaveTime(s.last_modified_ms)})`).join(' | ') : 'No save directory found for the detected game'}</div></PanelSectionRow>}
      <PanelSectionRow>
        <div style={{ fontSize: 'small', opacity: 0.7 }}>
          {status?.save_roots?.length ? `Save roots: ${status.save_roots.join(', ')}` : 'No Eglantine save directory found. See README.'}
        </div>
      </PanelSectionRow>
      <PanelSectionRow>
        <ButtonItem layout="below" disabled={refreshing} onClick={() => void rescan()}>
          {refreshing ? 'Scanning Eglantine...' : 'Refresh detection'}
        </ButtonItem>
      </PanelSectionRow>
      {(error || status?.error) && <PanelSectionRow><div>{error || status?.error}</div></PanelSectionRow>}
    </PanelSection>
    <PanelSection title="Backup">
      <PanelSectionRow>
        <div>{status?.backup_error ? `Backup configuration error: ${status.backup_error}` : status?.backup_configured ? `Games backed up: ${status.backed_up_games ?? 0}` : 'Backup destination not configured. Set one global destination with --configure-sync.'}</div>
      </PanelSectionRow>
    </PanelSection>
    <PanelSection title="Activity console">
      <PanelSectionRow>
        <div style={{ width: '100%' }}>
          <div style={{ opacity: 0.75, fontSize: 'small', marginBottom: 8 }}>
            {status?.last_scan ? `Last monitor scan: ${new Date(status.last_scan).toLocaleTimeString()}` : 'Monitor scan pending'} {' · '} {status?.last_sync ? `Last sync check: ${new Date(status.last_sync).toLocaleTimeString()}` : 'No automatic sync check yet'}
          </div>
          <div style={{
            maxHeight: 220, overflowY: 'auto', padding: 10,
            borderRadius: 8, background: '#10151b', color: '#cbd5e1',
            fontFamily: 'monospace', fontSize: 11, lineHeight: 1.55,
            overflowWrap: 'anywhere',
          }}>
            {status?.activity?.length ? status.activity.slice().reverse().map((entry) =>
              <div key={entry.id} style={{ marginBottom: 6, color:
                entry.level === 'error' ? '#fca5a5' :
                entry.level === 'sync' ? '#86efac' :
                entry.level === 'game' ? '#93c5fd' :
                entry.level === 'save' ? '#fcd34d' : '#cbd5e1' }}>
                <span style={{ opacity: 0.65 }}>[{new Date(entry.time).toLocaleTimeString()}]</span> {entry.message}
              </div>
            ) : 'No activity yet'}
          </div>
        </div>
      </PanelSectionRow>
      <PanelSectionRow>
        <ButtonItem layout="below" onClick={() => void clearActivity().then(() => refresh()).catch(e => setError(String(e)))}>
          Clear console
        </ButtonItem>
      </PanelSectionRow>
    </PanelSection>
    <PanelSection title="Prototype limitations">
      <PanelSectionRow>
        <div>Native WebDAV automatically syncs paired saves while Eglantine is closed. Conflicting saves are not overwritten. First-time pairing may require --sync-adopt TITLE_ID. Legacy rsync/rclone destinations remain backup-only.</div>
      </PanelSectionRow>
    </PanelSection>
  </>;
}

export default definePlugin(() => {
  const listener = addEventListener<[Change]>('save_updated', (change) => {
    toaster.toast({
      title: 'Updated local save detected',
      body: `${change.game} · ${change.changed_count} file(s) changed. Sync pending.`,
      icon: <FaCloud />,
    });
  });
  const syncListener = addEventListener<[{ kind: string; title: string; body: string }]>("sync_notification", (event) => {
    toaster.toast({title: event.title, body: event.body, icon: <FaCloud />});
  });
  const gameListener = addEventListener<[{ game: string; title_id: string }]>("game_detected", (event) => {
    toaster.toast({title: "Eglantine game detected", body: `${event.game} [${event.title_id}]`, icon: <FaGamepad />});
  });
  return {
    name: 'Checkpoint',
    titleView: <div className={staticClasses.Title}>Checkpoint</div>,
    content: <Content />,
    icon: <FaGamepad />,
    onDismount() { removeEventListener('save_updated', listener); removeEventListener("game_detected", gameListener); removeEventListener("sync_notification", syncListener); },
  };
});
