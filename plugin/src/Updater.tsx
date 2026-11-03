// The update view: one component for the Quick Access panel and the weekly
// prompt. It reads the shared store, keeps the worker's heartbeat while it is
// on screen, and turns button presses into backend verbs.

import { useQuickAccessVisible } from '@decky/api';
import {
  errorText,
  getState,
  heartbeat,
  later,
  probe,
  reboot,
  shortRev,
  snooze,
  start,
  stop,
  type Offer,
  type Phase,
  type Status,
} from './api';
import { DFL, React } from './globals';
import { applySnapshot, liveStatus, store, useStore } from './store';

const { useEffect, useState, Fragment } = React;

/** Probe results older than this are refreshed when the panel opens. */
const OFFER_MAX_AGE_MS = 5 * 60 * 1000;
const HEARTBEAT_MS = 2000;
const SNAPSHOT_MS = 10000;
const SUBJECT_LINES = 6;
const LOG_LINES = 6;

export interface UpdaterProps {
  mode: 'panel' | 'modal';
  /** A prompt's offer; the panel probes for its own. */
  offer?: Offer;
  showLater?: boolean;
  onLater?: () => void;
  /** Close the modal; the panel has nothing to close. */
  onClose?: () => void;
}

interface ButtonSpec {
  id: string;
  label: string;
  onClick: () => void;
  disabled?: boolean;
}

interface Section {
  title: string;
  lines: React.ReactNode[];
  progress?: { percent: number | null; detail: string };
  buttons: ButtonSpec[];
  spinner?: boolean;
}

const PHASE_TITLE: Record<Phase, string> = {
  evaluating: 'Preparing',
  downloading: 'Downloading',
  building: 'Building',
  installing: 'Installing',
  cleaning: 'Cleaning up',
  done: 'Restarting',
  'up-to-date': 'Up to date',
  failed: 'Update failed',
};

const IN_PROGRESS: Phase[] = ['evaluating', 'downloading', 'building', 'installing', 'cleaning'];

/** Phases during which the worker may want the heartbeat. */
const heartbeatWanted = (phase: Phase) => IN_PROGRESS.includes(phase) || phase === 'done';

const cancellableNow = (s: Status) =>
  s.cancellable && (s.phase === 'evaluating' || s.phase === 'downloading' || s.phase === 'building');

export async function refreshSnapshot() {
  try {
    applySnapshot(await getState());
  } catch (err) {
    console.error('[System Update] state:', err);
  }
}

export async function runProbe() {
  if (store.get().probing) return;
  store.set({ probing: true });
  try {
    store.set({ offer: await probe(), offerAt: Date.now() });
  } catch (err) {
    console.error('[System Update] probe:', err);
    store.set({ offer: { kind: 'offline', installed: null, available: null, subjects: [] }, offerAt: Date.now() });
  } finally {
    store.set({ probing: false });
  }
}

export function Updater(props: UpdaterProps) {
  const s = useStore();
  const qamVisible = useQuickAccessVisible();
  const visible = props.mode === 'modal' || qamVisible;
  const live = liveStatus(s);
  const offer = props.offer ?? s.offer;
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Count as on screen while visible, so toasts stay quiet.
  useEffect(() => {
    if (!visible) return;
    store.set({ viewers: store.get().viewers + 1 });
    return () => store.set({ viewers: store.get().viewers - 1 });
  }, [visible]);

  // Fresh facts on open, then every few seconds while on screen.
  useEffect(() => {
    if (!visible) return;
    refreshSnapshot();
    const id = setInterval(refreshSnapshot, SNAPSHOT_MS);
    return () => clearInterval(id);
  }, [visible]);

  // The panel probes for an update when nothing is running and the last look is old.
  useEffect(() => {
    if (!visible || props.offer || live) return;
    if (!s.offer || Date.now() - s.offerAt > OFFER_MAX_AGE_MS) runProbe();
  }, [visible, live !== null]);

  // The worker reboots only while an update view is on screen and no game runs.
  const livePhase = live?.phase ?? null;
  useEffect(() => {
    if (!visible || !livePhase || !heartbeatWanted(livePhase)) return;
    const tick = async () => {
      try {
        const reply = await heartbeat();
        store.set({ heartbeatRefused: reply.accepted ? null : reply.reason ?? 'refused' });
      } catch (err) {
        store.set({ heartbeatRefused: errorText(err) });
      }
    };
    tick();
    const id = setInterval(tick, HEARTBEAT_MS);
    return () => clearInterval(id);
  }, [visible, livePhase]);

  useEffect(() => {
    if (live) setStarting(false);
  }, [live !== null]);

  const onUpdate = async () => {
    if (!offer) return;
    setError(null);
    setStarting(true);
    try {
      await start(offer.available);
    } catch (err) {
      setStarting(false);
      setError(`Couldn't start the update: ${errorText(err)}`);
    }
  };

  const onRestart = async () => {
    setError(null);
    try {
      await reboot();
    } catch (err) {
      setError(`Couldn't restart: ${errorText(err)}`);
    }
  };

  const onCancel = () => {
    if (!live) return;
    const startedAt = live.started_at;
    DFL.showModal(
      <DFL.ConfirmModal
        strTitle="Cancel the update?"
        strDescription="Nothing has been changed yet. The next prompt waits a day."
        strOKButtonText="Cancel update"
        strCancelButtonText="Keep updating"
        bDestructiveWarning
        onOK={async () => {
          store.set({ cancelled: startedAt });
          try {
            await stop();
            await snooze();
          } catch (err) {
            setError(`Couldn't cancel: ${errorText(err)}`);
          }
        }}
      />,
    );
  };

  const onDismiss = () => {
    if (live) store.set({ dismissed: live.started_at });
    props.onClose?.();
  };

  const section = describe({
    live,
    offer,
    starting,
    probing: s.probing,
    batteryBlocked: s.snapshot?.battery_blocked ?? false,
    batteryGate: s.snapshot?.battery_gate ?? 0,
    unitActive: s.unitActive,
    heartbeatRefused: s.heartbeatRefused,
    cancelled: live !== null && live.started_at === s.cancelled,
    showLater: props.showLater ?? false,
    onUpdate,
    onRestart,
    onCancel,
    onLater: props.onLater ?? (() => undefined),
    onClose: onDismiss,
    onCheck: runProbe,
  });
  if (error) section.lines.push(<Line key="error" text={error} />);

  return props.mode === 'panel' ? <PanelLayout section={section} /> : <ModalLayout section={section} />;
}

interface Facts {
  live: Status | null;
  offer: Offer | null;
  starting: boolean;
  probing: boolean;
  batteryBlocked: boolean;
  batteryGate: number;
  unitActive: boolean;
  heartbeatRefused: string | null;
  cancelled: boolean;
  showLater: boolean;
  onUpdate: () => void;
  onRestart: () => void;
  onCancel: () => void;
  onLater: () => void;
  onClose: () => void;
  onCheck: () => void;
}

/** The screen for the facts at hand. Pure, so the layouts stay dumb. */
export function describe(f: Facts): Section {
  const close: ButtonSpec = { id: 'close', label: 'Close', onClick: f.onClose };
  const restart: ButtonSpec = { id: 'restart', label: 'Restart now', onClick: f.onRestart };
  const laterButton: ButtonSpec[] = f.showLater ? [{ id: 'later', label: 'Later', onClick: f.onLater }] : [];

  if (f.live) {
    const s = f.live;
    if (IN_PROGRESS.includes(s.phase)) {
      const lines: React.ReactNode[] = [];
      if (s.copies.expected > 0 || s.builds.expected > 0) {
        lines.push(<Line key="counts" text={counts(s)} muted />);
      }
      return {
        title: PHASE_TITLE[s.phase],
        lines,
        progress: { percent: s.percent, detail: s.detail },
        buttons: cancellableNow(s) ? [{ id: 'cancel', label: 'Cancel', onClick: f.onCancel }] : [],
      };
    }
    if (s.phase === 'done') {
      if (f.unitActive && !f.heartbeatRefused) {
        return { title: 'Restarting', lines: [], progress: { percent: null, detail: 'Finishing the update' }, buttons: [] };
      }
      const why = f.heartbeatRefused ? `Restart when ${f.heartbeatRefused.replace(/^a /, 'the ')} is done.` : 'Restart to finish it.';
      return {
        title: 'Update installed',
        lines: [<Line key="why" text={why} />],
        buttons: [restart, close],
      };
    }
    if (s.phase === 'up-to-date') {
      return { title: 'Up to date', lines: [<Line key="l" text="This machine already runs the latest build." />], buttons: [close] };
    }
    if (f.cancelled) {
      return { title: 'Update cancelled', lines: [<Line key="l" text="Nothing was changed." />], buttons: [close] };
    }
    const lines: React.ReactNode[] = [<Line key="error" text={s.error ?? 'nixos-rebuild failed.'} />];
    if (s.log_tail.length) lines.push(<Log key="log" lines={s.log_tail.slice(-LOG_LINES)} />);
    return { title: 'Update failed', lines, buttons: [close] };
  }

  if (f.starting) {
    return { title: 'Starting', lines: [], progress: { percent: null, detail: 'Starting the update' }, buttons: [] };
  }

  if (f.probing || !f.offer) {
    return { title: 'Checking for updates', lines: [], spinner: true, buttons: [] };
  }

  const o = f.offer;
  const check: ButtonSpec = { id: 'check', label: 'Check again', onClick: f.onCheck };
  switch (o.kind) {
    case 'update': {
      const lines: React.ReactNode[] = [
        <Line key="revs" text={`${shortRev(o.installed)} → ${shortRev(o.available)}`} muted />,
        ...o.subjects.slice(0, SUBJECT_LINES).map((subject, i) => <Line key={`s${i}`} text={subject} />),
      ];
      if (f.batteryBlocked) {
        lines.push(<Line key="battery" text={`Plug in or charge above ${f.batteryGate}% to update.`} />);
      }
      return {
        title: 'Update available',
        lines,
        buttons: [{ id: 'update', label: 'Update', onClick: f.onUpdate, disabled: f.batteryBlocked }, ...laterButton],
      };
    }
    case 'restart':
      return {
        title: 'Restart pending',
        lines: [<Line key="l" text="A newer build is installed and waits for a restart." />],
        buttons: [restart, ...laterButton],
      };
    case 'preparing':
      return {
        title: 'Update on its way',
        lines: [<Line key="l" text={`${shortRev(o.available)} is still being built for this machine.`} />],
        buttons: [check],
      };
    case 'offline':
      return {
        title: 'Offline',
        lines: [<Line key="l" text="Couldn't reach GitHub to look for updates." />],
        buttons: [check],
      };
    case 'current':
    default:
      return {
        title: 'Up to date',
        lines: [<Line key="l" text={`Installed ${shortRev(o.installed)}`} muted />],
        buttons: [check],
      };
  }
}

function counts(s: Status): string {
  const parts: string[] = [];
  if (s.copies.expected > 0) parts.push(`${s.copies.done} of ${s.copies.expected} downloads`);
  if (s.builds.expected > 0) parts.push(`${s.builds.done} of ${s.builds.expected} builds`);
  return parts.join(' · ');
}

function Line({ text, muted }: { text: string; muted?: boolean }) {
  return <div style={{ fontSize: '13px', opacity: muted ? 0.7 : 1, padding: '2px 0', wordBreak: 'break-word' }}>{text}</div>;
}

function Log({ lines }: { lines: string[] }) {
  return (
    <div style={{ fontFamily: 'monospace', fontSize: '11px', opacity: 0.8, whiteSpace: 'pre-wrap', wordBreak: 'break-all', padding: '4px 0' }}>
      {lines.join('\n')}
    </div>
  );
}

function Progress({ percent, detail }: { percent: number | null; detail: string }) {
  return (
    <DFL.ProgressBarWithInfo
      layout="below"
      bottomSeparator="none"
      nProgress={percent ?? 0}
      indeterminate={percent === null}
      sOperationText={detail}
    />
  );
}

function PanelLayout({ section }: { section: Section }) {
  return (
    <Fragment>
      <DFL.PanelSection title={section.title} spinner={section.spinner}>
        {section.lines.length > 0 && (
          <DFL.PanelSectionRow>
            <DFL.Field childrenLayout="below" bottomSeparator="none">
              {section.lines}
            </DFL.Field>
          </DFL.PanelSectionRow>
        )}
        {section.progress && (
          <DFL.PanelSectionRow>
            <Progress {...section.progress} />
          </DFL.PanelSectionRow>
        )}
        {section.buttons.map((b) => (
          <DFL.PanelSectionRow key={b.id}>
            <DFL.ButtonItem layout="below" onClick={b.onClick} disabled={b.disabled}>
              {b.label}
            </DFL.ButtonItem>
          </DFL.PanelSectionRow>
        ))}
      </DFL.PanelSection>
    </Fragment>
  );
}

function ModalLayout({ section }: { section: Section }) {
  return (
    <Fragment>
      <DFL.DialogHeader>{section.title}</DFL.DialogHeader>
      <DFL.DialogBody>
        {section.spinner && <DFL.Spinner style={{ width: '32px', height: '32px' }} />}
        {section.lines.length > 0 && <div style={{ fontSize: '15px' }}>{section.lines}</div>}
        {section.progress && <Progress {...section.progress} />}
      </DFL.DialogBody>
      {section.buttons.length > 0 && (
        <DFL.DialogFooter>
          <DFL.Focusable style={{ display: 'flex', gap: '12px', justifyContent: 'flex-end' }}>
            {section.buttons.map((b) => (
              <DFL.DialogButton key={b.id} onClick={b.onClick} disabled={b.disabled} style={{ width: 'auto', minWidth: '140px' }}>
                {b.label}
              </DFL.DialogButton>
            ))}
          </DFL.Focusable>
        </DFL.DialogFooter>
      )}
    </Fragment>
  );
}
