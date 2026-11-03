// Plugin entry: the Quick Access panel, the weekly prompt the check hands over,
// and the toasts for runs that finish while nothing is watching.

import { addEventListener, definePlugin, removeEventListener, toaster } from '@decky/api';
import { errorText, getState, later, start, type Prompt, type PromptEvent, type StatusEvent } from './api';
import { DFL, React } from './globals';
import { applySnapshot, liveStatus, store } from './store';
import { Updater } from './Updater';

const TITLE = 'System Update';
/** Decky's own tab in the Quick Access Menu. */
const DECKY_TAB = (DFL as any).QuickAccessTab?.Decky ?? 999;

function openPanel() {
  DFL.Navigation.OpenQuickAccessMenu(DECKY_TAB);
}

function toast(title: string, body: string, onClick?: () => void) {
  toaster.toast({ title, body, onClick, duration: 8000 });
}

function Icon() {
  return (
    <svg viewBox="0 0 24 24" width="1em" height="1em" fill="currentColor">
      <path d="M12 2a10 10 0 1 1-7.07 2.93l1.41 1.41A8 8 0 1 0 12 4V7l4-4-4-4v3z" transform="translate(0 2)" />
      <path d="M11 9h2v4.17l1.59-1.58L16 13l-4 4-4-4 1.41-1.41L11 13.17z" />
    </svg>
  );
}

function PromptModal({ prompt, closeModal }: { prompt: Prompt; closeModal?: () => void }) {
  const onLater = async () => {
    if (prompt.show_later) {
      try {
        const reply = await later();
        if (reply.accepted && reply.warn) {
          DFL.showModal(
            <DFL.ConfirmModal
              strTitle="Last deferral"
              strDescription="Next week's update will install on its own while Steam is idle. Restart from the Quick Access Menu when it finishes."
              bAlertDialog
            />,
          );
        }
      } catch (err) {
        console.error('[System Update] later:', err);
      }
    }
    closeModal?.();
  };
  return (
    <DFL.ModalRoot closeModal={closeModal} onCancel={onLater} bAllowFullSize>
      <Updater mode="modal" offer={prompt.offer} showLater={prompt.show_later} onLater={onLater} onClose={closeModal} />
    </DFL.ModalRoot>
  );
}

let toastedFor: number | null = null;

/** Tell the user about a run that ended while no update view was on screen. */
function toastIfUnseen(ev: StatusEvent) {
  const s = store.get();
  const status = liveStatus(s);
  if (!status || s.viewers > 0 || toastedFor === status.started_at) return;
  // The worker writes this detail when it gave up waiting for the heartbeat.
  if (status.phase === 'done' && !ev.unit_active && status.detail === 'Restart pending') {
    toastedFor = status.started_at;
    toast('Update installed', 'Open the Quick Access Menu to restart.', openPanel);
  } else if (status.phase === 'failed' && status.started_at !== s.cancelled) {
    toastedFor = status.started_at;
    toast('Update failed', status.error ?? 'See the Quick Access Menu.', openPanel);
  }
}

function onStatus(ev: StatusEvent) {
  const prev = store.get();
  store.set({
    status: ev.status,
    unitActive: ev.unit_active,
    liveStart: ev.unit_active && ev.status ? ev.status.started_at : prev.liveStart,
  });
  toastIfUnseen(ev);
}

async function onPrompt(ev: PromptEvent) {
  const prompt = ev.prompt;
  store.set({ offer: prompt.offer, offerAt: Date.now() });
  if (prompt.forced && prompt.offer.kind === 'update') {
    try {
      await start(prompt.offer.available);
      toast('Installing the system update', 'It was put off twice. Restart from the Quick Access Menu when it finishes.', openPanel);
    } catch (err) {
      toast(TITLE, `Couldn't start the update: ${errorText(err)}`, openPanel);
    }
    return;
  }
  DFL.showModal(<PromptModal prompt={prompt} />, undefined, { strTitle: TITLE, bHideMainWindowForPopouts: false });
}

export default definePlugin(() => {
  const statusListener = addEventListener<[StatusEvent]>('status', onStatus);
  const promptListener = addEventListener<[PromptEvent]>('prompt', onPrompt);
  getState().then(applySnapshot).catch((err) => console.error('[System Update] state:', err));

  return {
    name: TITLE,
    titleView: <div className={DFL.staticClasses.Title}>{TITLE}</div>,
    content: <Updater mode="panel" />,
    icon: <Icon />,
    onDismount() {
      removeEventListener('status', statusListener);
      removeEventListener('prompt', promptListener);
    },
  };
});

