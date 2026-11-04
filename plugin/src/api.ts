// The backend verbs and the JSON shapes `steam-maintenance plugin` prints.

import { callable } from '@decky/api';

export type Phase =
  | 'evaluating'
  | 'downloading'
  | 'building'
  | 'installing'
  | 'cleaning'
  | 'done'
  | 'up-to-date'
  | 'failed';

export interface Counter {
  done: number;
  expected: number;
}

export interface Status {
  phase: Phase;
  percent: number | null;
  detail: string;
  builds: Counter;
  copies: Counter;
  error: string | null;
  log_tail: string[];
  cancellable: boolean;
  started_at: number;
  updated_at: number;
}

export interface Battery {
  percent: number;
  charging: boolean;
}

export interface Snapshot {
  installed: string | null;
  unit_active: boolean;
  status: Status | null;
  battery: Battery | null;
  battery_blocked: boolean;
  battery_gate: number;
  restart_pending: boolean;
  game_running: boolean;
  deferrals: number;
  max_deferrals: number;
  next_due: number;
}

export type OfferKind = 'update' | 'restart' | 'preparing' | 'offline' | 'current';

export interface Offer {
  kind: OfferKind;
  installed: string | null;
  available: string | null;
}

export interface Prompt {
  offer: Offer;
  forced: boolean;
  show_later: boolean;
  warn_on_later: boolean;
  requested_at: number;
}

export interface LaterReply {
  accepted: boolean;
  warn: boolean;
}

export interface HeartbeatReply {
  accepted: boolean;
  reason: string | null;
}

export interface PromptEvent {
  event: 'prompt';
  prompt: Prompt;
}

export interface StatusEvent {
  event: 'status';
  status: Status | null;
  unit_active: boolean;
}

export const getState = callable<[], Snapshot>('state');
export const probe = callable<[], Offer>('probe');
export const later = callable<[], LaterReply>('later');
export const snooze = callable<[], {}>('snooze');
export const heartbeat = callable<[], HeartbeatReply>('heartbeat');
export const start = callable<[rev: string | null], {}>('start');
export const stop = callable<[], {}>('stop');
export const reboot = callable<[], {}>('reboot');

export const errorText = (err: unknown): string =>
  err instanceof Error ? err.message : String(err);
