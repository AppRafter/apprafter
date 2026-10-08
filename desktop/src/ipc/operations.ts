// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The operations the webview follows: a module-level store, read through useSyncExternalStore,
// of each operation's summary (op_list) and live state (its events).
//
// One subscription per operation, however many components follow it: Rust sends every
// subscription every event, so two would show each line twice. A new subscription starts from
// Rust's replay, which replaces what the view showed. The command's answer (the replay) and the
// channel's first messages reach JS in either order; Rust registers the sink and takes the
// snapshot atomically, so the channel's messages wait until the replay is applied and nothing is
// lost or repeated.
//
// Every lock transition ends every subscription in Rust, the unlock included: on each
// `lock-changed`, clearLive() forgets them, and after an unlock reattachAll() follows again
// what is still followed, each from its replay.
import { Channel } from '@tauri-apps/api/core';
import { useSyncExternalStore } from 'react';
import * as api from './api';
import { IpcError } from './api';
import { DESKTOP_ERROR_CODES } from './generated/errors';
import type { OpEvent } from './generated/OpEvent';
import type { OpId } from './generated/OpId';
import type { OpSummary } from './generated/OpSummary';
import type { Outcome } from './generated/Outcome';
import type { Subscribed } from './generated/Subscribed';
import type { SubscriptionId } from './generated/SubscriptionId';
import type { JsonValue } from './generated/serde_json/JsonValue';
import type { UiError } from './generated/UiError';

/** The output one view keeps, in UTF-8 bytes (Rust's replay keeps as much); older goes. */
export const OUTPUT_CAP = 1 << 20;

export interface Stage {
  readonly index: number;
  readonly total: number;
  readonly title: string;
}

export interface Progress {
  readonly done: number;
  readonly total: number | null;
  readonly unit: string;
}

export type OpLine =
  | { readonly kind: 'stdout' | 'stderr' | 'warning' | 'notice'; readonly text: string }
  | { readonly kind: 'dropped'; readonly bytes: number; readonly text: string };

export type OpEnd =
  | { readonly state: 'finished' | 'cancelled'; readonly outcome: Outcome<JsonValue> }
  | { readonly state: 'failed'; readonly error: UiError };

export interface OpView {
  readonly opId: OpId;
  /** From op_list; null for a plan, or an operation not listed yet. */
  readonly summary: OpSummary | null;
  readonly stage: Stage | null;
  readonly progress: Progress | null;
  /** Output, warnings and notices in arrival order, after a `dropped` line when output went. */
  readonly lines: readonly OpLine[];
  readonly end: OpEnd | null;
  /** The replay is applied and events arrive as they happen. */
  readonly live: boolean;
  /** Why following failed, when it was not the lock (which reattachAll recovers from). */
  readonly attachError: UiError | null;
}

interface Subscription {
  readonly channel: Channel<OpEvent>;
  /** Rust's id, once the command answered. */
  id: SubscriptionId | null;
  /** `live`: its events apply as they come. Until then they wait; once `ended`, they go. */
  status: 'opening' | 'live' | 'ended';
  waiting: OpEvent[];
}

interface Entry {
  view: OpView;
  followers: number;
  /** The subscription whose events the view shows. */
  current: Subscription | null;
  /** UTF-8 bytes of the output lines in the view. */
  outputBytes: number;
}

const entries = new Map<OpId, Entry>();
const listeners = new Set<() => void>();
let snapshot: ReadonlyMap<OpId, OpView> = new Map();
/** Moves on each clearLive: an answer from before it belongs to a subscription Rust dropped. */
let epoch = 0;

const NOTHING_LIVE = { stage: null, progress: null, lines: [], end: null, live: false } as const;
const encoder = new TextEncoder();
const utf8Bytes = (text: string) => encoder.encode(text).length;

function publish() {
  snapshot = new Map([...entries].map(([opId, entry]) => [opId, entry.view]));
  for (const listener of listeners) listener();
}

export function watchOperations(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function operationsSnapshot(): ReadonlyMap<OpId, OpView> {
  return snapshot;
}

export function useOperations(): ReadonlyMap<OpId, OpView> {
  return useSyncExternalStore(watchOperations, operationsSnapshot);
}

export function useOperation(opId: OpId): OpView | undefined {
  return useOperations().get(opId);
}

function entryFor(opId: OpId): Entry {
  let entry = entries.get(opId);
  if (entry === undefined) {
    entry = {
      view: { opId, summary: null, ...NOTHING_LIVE, attachError: null },
      followers: 0,
      current: null,
      outputBytes: 0,
    };
    entries.set(opId, entry);
  }
  return entry;
}

/** Nothing keeps an entry without followers and without a summary. */
function prune(opId: OpId, entry: Entry) {
  if (entry.followers === 0 && entry.view.summary === null && entries.get(opId) === entry) {
    entries.delete(opId);
  }
}

function forgetLive(entry: Entry) {
  entry.view = { ...entry.view, ...NOTHING_LIVE, attachError: null };
  entry.outputBytes = 0;
}

const isLocked = (e: unknown) =>
  e instanceof IpcError && e.error.code === DESKTOP_ERROR_CODES.LOCKED;

function openChannel(opId: OpId): Subscription {
  const sub: Subscription = {
    channel: new Channel<OpEvent>((event) => {
      if (sub.status === 'ended') return;
      const entry = entries.get(opId);
      if (sub.status === 'live' && entry?.current === sub) {
        apply(entry, [event]);
        publish();
      } else {
        sub.waiting.push(event);
      }
    }),
    id: null,
    status: 'opening',
    waiting: [],
  };
  return sub;
}

async function unsubscribe(opId: OpId, id: SubscriptionId) {
  try {
    await api.opUnsubscribe(opId, id);
  } catch (e) {
    // While locked the gate refuses it, and the lock has ended the subscription anyway.
    if (!isLocked(e)) console.error(`op_unsubscribe for operation ${opId} failed:`, e);
  }
}

/** Stop applying `sub`; `tellRust` ends it there too (once its id is known). */
function end(opId: OpId, sub: Subscription, tellRust: boolean) {
  if (sub.status === 'ended') return;
  sub.status = 'ended';
  sub.waiting = [];
  if (tellRust && sub.id !== null) void unsubscribe(opId, sub.id);
}

/** `sub` becomes the view's source: its replay, then what its channel brought meanwhile. */
function goLive(entry: Entry, sub: Subscription, replay: readonly OpEvent[]) {
  const waiting = sub.waiting;
  sub.waiting = [];
  sub.status = 'live';
  entry.current = sub;
  forgetLive(entry);
  entry.view = { ...entry.view, live: true };
  apply(entry, replay);
  apply(entry, waiting);
  publish();
}

async function follow(opId: OpId, entry: Entry) {
  const sub = openChannel(opId);
  entry.current = sub;
  let answer: Subscribed;
  try {
    answer = await api.opSubscribe(opId, sub.channel);
  } catch (e) {
    const abandoned = entry.current !== sub;
    end(opId, sub, false);
    if (abandoned) return;
    entry.current = null;
    // Refused because locked: reattachAll follows it again after the unlock.
    if (!isLocked(e)) {
      const error: UiError =
        e instanceof IpcError
          ? e.error
          : { code: null, message: String(e), help: null, causes: [], fields: {} };
      entry.view = { ...entry.view, attachError: error };
      publish();
    }
    return;
  }
  sub.id = answer.subscription;
  if (sub.status === 'ended' || entry.current !== sub) {
    // Released, or cleared by a lock, while the answer was on its way.
    sub.status = 'ended';
    void unsubscribe(opId, answer.subscription);
    return;
  }
  goLive(entry, sub, answer.replay);
}

/** Follow `opId` until the returned release is called (once; later calls do nothing). */
export function attach(opId: OpId): () => void {
  const entry = entryFor(opId);
  entry.followers += 1;
  if (entry.current === null) void follow(opId, entry);
  publish();
  return releaser(opId, entry);
}

function releaser(opId: OpId, entry: Entry): () => void {
  let released = false;
  return () => {
    if (released) return;
    released = true;
    entry.followers -= 1;
    if (entry.followers > 0) return;
    if (entry.current !== null) end(opId, entry.current, true);
    entry.current = null;
    forgetLive(entry);
    prune(opId, entry);
    publish();
  };
}

/**
 * Run the plan `opId` and follow it; resolves with the release once Rust has started it.
 *
 * The rejection is the answer, thrown as it came. The `Failed` that Rust may also send on this
 * call's channel carries the same error for pages that followed the plan before; this channel's
 * copy is dropped, so the error shows once. A subscription the store already held for the plan
 * ends once execute answers: both carry the operation's events from its start.
 */
export async function execute(opId: OpId): Promise<() => void> {
  const entry = entryFor(opId);
  const sub = openChannel(opId);
  const started = epoch;
  let id: SubscriptionId;
  try {
    id = await api.opExecute(opId, sub.channel);
  } catch (e) {
    end(opId, sub, false);
    prune(opId, entry);
    publish();
    throw e;
  }
  sub.id = id;
  entry.followers += 1;
  if (epoch !== started) {
    // A lock came in between and Rust dropped this subscription: reattachAll follows it.
    end(opId, sub, false);
    if (!entries.has(opId)) entries.set(opId, entry);
    publish();
    return releaser(opId, entry);
  }
  if (entry.current !== null) end(opId, entry.current, true);
  goLive(entry, sub, []);
  return releaser(opId, entry);
}

/** Cancel an operation, its open prompt or its plan; the operation then ends on its own. */
export function cancel(opId: OpId): Promise<void> {
  return api.opCancel(opId);
}

/** Forget an ended operation whose result was shown, or a closed plan. */
export async function discard(opId: OpId): Promise<void> {
  await api.opDiscard(opId);
  const entry = entries.get(opId);
  if (entry === undefined) return;
  entry.view = { ...entry.view, summary: null };
  prune(opId, entry);
  publish();
}

/** Read op_list: the listed operations' summaries; an unlisted one nobody follows goes. */
export async function refreshList(): Promise<void> {
  const list = await api.opList();
  const listed = new Set(list.map((s) => s.opId));
  for (const [opId, entry] of entries) {
    if (listed.has(opId)) continue;
    entry.view = { ...entry.view, summary: null };
    prune(opId, entry);
  }
  for (const summary of list) {
    const entry = entryFor(summary.opId);
    const end = entry.view.end;
    entry.view = {
      ...entry.view,
      summary: end === null ? summary : { ...summary, state: end.state },
    };
  }
  publish();
}

/**
 * The lock changed: Rust ended every subscription. Forget them, the live state and the
 * summaries; the followers stay counted for reattachAll.
 */
export function clearLive(): void {
  epoch += 1;
  for (const [opId, entry] of entries) {
    if (entry.current !== null) end(opId, entry.current, false);
    entry.current = null;
    forgetLive(entry);
    entry.view = { ...entry.view, summary: null };
    prune(opId, entry);
  }
  publish();
}

/** After an unlock: follow again, from the replay, every operation still followed. */
export function reattachAll(): void {
  for (const [opId, entry] of entries) {
    if (entry.followers > 0 && entry.current === null) void follow(opId, entry);
  }
}

/** Forget everything without telling Rust; for tests. */
export function resetOperations(): void {
  epoch += 1;
  for (const [opId, entry] of entries) {
    if (entry.current !== null) end(opId, entry.current, false);
  }
  entries.clear();
  publish();
}

const droppedLine = (bytes: number): OpLine => ({
  kind: 'dropped',
  bytes,
  text: `Earlier output was dropped (${bytes} bytes).`,
});

/** Count `bytes` more of dropped output in the leading `dropped` line. */
function addDropped(lines: OpLine[], bytes: number) {
  if (bytes === 0) return;
  const first = lines[0];
  if (first?.kind === 'dropped') lines[0] = droppedLine(first.bytes + bytes);
  else lines.unshift(droppedLine(bytes));
}

/** Past the cap, the oldest output lines go, counted in the leading `dropped` line. */
function trim(entry: Entry, lines: OpLine[]) {
  let dropped = 0;
  for (let i = 0; i < lines.length && entry.outputBytes > OUTPUT_CAP; ) {
    const line = lines[i];
    if (line?.kind === 'stdout' || line?.kind === 'stderr') {
      const bytes = utf8Bytes(line.text);
      entry.outputBytes -= bytes;
      dropped += bytes;
      lines.splice(i, 1);
    } else {
      i++;
    }
  }
  addDropped(lines, dropped);
}

function apply(entry: Entry, events: readonly OpEvent[]) {
  if (events.length === 0) return;
  let { stage, progress, end, summary } = entry.view;
  const lines = [...entry.view.lines];
  for (const event of events) {
    switch (event.kind) {
      case 'stage':
        stage = { index: event.index, total: event.total, title: event.title };
        break;
      case 'progress':
        progress = { done: event.done, total: event.total, unit: event.unit };
        break;
      case 'output':
        lines.push({ kind: event.stream, text: event.text });
        entry.outputBytes += utf8Bytes(event.text);
        break;
      case 'output_dropped':
        addDropped(lines, event.bytes);
        break;
      case 'warning':
      case 'notice':
        lines.push({ kind: event.kind, text: event.message });
        break;
      case 'finished':
        end = {
          state: event.outcome.status === 'completed' ? 'finished' : 'cancelled',
          outcome: event.outcome,
        };
        break;
      case 'failed':
        end = { state: 'failed', error: event.error };
        break;
    }
  }
  trim(entry, lines);
  if (summary !== null && end !== null) summary = { ...summary, state: end.state };
  entry.view = { ...entry.view, stage, progress, lines, end, summary };
}
