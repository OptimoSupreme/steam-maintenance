// One shared picture of the updater, fed by the backend's events and read by
// the panel, the prompt modal and the toasts.

import type { Offer, Snapshot, Status } from './api';
import { React } from './globals';

export interface State {
  snapshot: Snapshot | null;
  status: Status | null;
  unitActive: boolean;
  /** `started_at` of a run this plugin instance saw the worker perform; older status files are stale. */
  liveStart: number | null;
  /** `started_at` of a finished run the user has closed. */
  dismissed: number | null;
  /** `started_at` of a run the user cancelled, so its Failed reads as cancelled. */
  cancelled: number | null;
  offer: Offer | null;
  /** When `offer` was probed, in ms. */
  offerAt: number;
  probing: boolean;
  /** Why the last heartbeat was refused, or null. */
  heartbeatRefused: string | null;
  /** Update views currently on screen. */
  viewers: number;
}

type Listener = () => void;

let state: State = {
  snapshot: null,
  status: null,
  unitActive: false,
  liveStart: null,
  dismissed: null,
  cancelled: null,
  offer: null,
  offerAt: 0,
  probing: false,
  heartbeatRefused: null,
  viewers: 0,
};

const listeners = new Set<Listener>();

export const store = {
  get: (): State => state,
  set(patch: Partial<State>) {
    state = { ...state, ...patch };
    listeners.forEach((l) => l());
  },
  subscribe(listener: Listener): () => void {
    listeners.add(listener);
    return () => listeners.delete(listener);
  },
};

export function useStore(): State {
  const [, rerender] = React.useState(0);
  React.useEffect(() => store.subscribe(() => rerender((n) => n + 1)), []);
  return state;
}

/** The status worth showing: a running unit's, or the finished result of a run we watched. */
export function liveStatus(s: State): Status | null {
  const { status, unitActive, liveStart, dismissed } = s;
  if (!status) return null;
  if (!unitActive && status.started_at !== liveStart) return null;
  if (status.started_at === dismissed) return null;
  return status;
}

/** Apply a `state` snapshot from the backend. */
export function applySnapshot(snap: Snapshot) {
  const prev = store.get();
  store.set({
    snapshot: snap,
    status: snap.status,
    unitActive: snap.unit_active,
    liveStart: snap.unit_active && snap.status ? snap.status.started_at : prev.liveStart,
  });
}
