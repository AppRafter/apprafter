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
import { IpcError, uiErrorOf } from './api';
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

/** The warnings and notices one view keeps (Rust's MESSAGE_CAP); older ones are counted. */
export const MESSAGE_CAP = 1024;

/**
 * The events a subscription holds while its command is answering. More than that and the held
 * events are not trusted: the subscription ends and the view follows the replay again.
 */
export const WAITING_CAP = 1024;

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
  | { readonly kind: 'dropped'; readonly bytes: number; readonly text: string }
  | { readonly kind: 'messages_dropped'; readonly count: number; readonly text: string };

export type OpEnd =
  | { readonly state: 'finished' | 'cancelled'; readonly outcome: Outcome<JsonValue> }
  | { readonly state: 'failed'; readonly error: UiError };

export interface OpView {
  readonly opId: OpId;
  /** From op_list; null for a plan, or an operation not listed yet. */
  readonly summary: OpSummary | null;
  readonly stage: Stage | null;
  readonly progress: Progress | null;
  /**
   * Output, warnings and notices in arrival order, after a `dropped` line when output went and a
   * `messages_dropped` line when messages did.
   */
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
  /** `live`: its events can apply. Until then they wait; once `ended`, they go. */
  status: 'opening' | 'live' | 'ended';
  waiting: OpEvent[];
  /** More than WAITING_CAP events waited: they are dropped and the view follows a new replay. */
  overflowed: boolean;
}

interface Entry {
  readonly opId: OpId;
  view: OpView;
  followers: number;
  /** The subscription whose events the view shows. */
  current: Subscription | null;
  /** Executes answering: meanwhile the current subscription's events wait (see execute). */
  executing: number;
  /** UTF-8 bytes of the output lines in the view. */
  outputBytes: number;
  /** Warning and notice lines in the view. */
  messages: number;
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

/** Every followed or listed operation; re-renders on any change (the operations sheet). */
export function useOperations(): ReadonlyMap<OpId, OpView> {
  return useSyncExternalStore(watchOperations, operationsSnapshot);
}

/** One operation; re-renders only when that operation's view changes. */
export function useOperation(opId: OpId): OpView | undefined {
  return useSyncExternalStore(watchOperations, () => snapshot.get(opId));
}

function entryFor(opId: OpId): Entry {
  let entry = entries.get(opId);
  if (entry === undefined) {
    entry = {
      opId,
      view: { opId, summary: null, ...NOTHING_LIVE, attachError: null },
      followers: 0,
      current: null,
      executing: 0,
      outputBytes: 0,
      messages: 0,
    };
    entries.set(opId, entry);
  }
  return entry;
}

/** Nothing keeps an entry without followers and without a summary. */
function prune(entry: Entry) {
  if (entry.followers === 0 && entry.view.summary === null && entries.get(entry.opId) === entry) {
    entries.delete(entry.opId);
  }
}

function forgetLive(entry: Entry) {
  entry.view = { ...entry.view, ...NOTHING_LIVE, attachError: null };
  entry.outputBytes = 0;
  entry.messages = 0;
}

const isLocked = (e: unknown) =>
  e instanceof IpcError && e.error.code === DESKTOP_ERROR_CODES.LOCKED;
/**
 * A refusal after which the plan waits in Rust for another try under the same id: a busy prompt
 * asked nothing, and a failed gesture (a wrong password, the back-off) lets the owner try again.
 */
const leavesThePlanWaiting = (e: unknown) =>
  e instanceof IpcError &&
  (e.error.code === DESKTOP_ERROR_CODES.AUTH_BUSY ||
    e.error.code === DESKTOP_ERROR_CODES.AUTH_FAILED);

/** A channel for `entry`: its events reach that entry object, whatever the map holds now. */
function openChannel(entry: Entry): Subscription {
  const sub: Subscription = {
    channel: new Channel<OpEvent>((event) => receive(entry, sub, event)),
    id: null,
    status: 'opening',
    waiting: [],
    overflowed: false,
  };
  return sub;
}

function receive(entry: Entry, sub: Subscription, event: OpEvent) {
  if (sub.status === 'ended' || sub.overflowed) return;
  if (sub.status === 'live' && entry.current !== sub) {
    // Replaced without being ended: it must not keep sending to a view that ignores it.
    end(entry, sub, true);
    return;
  }
  if (sub.status === 'live' && entry.executing === 0) {
    apply(entry, [event]);
    publish();
    return;
  }
  if (sub.waiting.length >= WAITING_CAP) {
    sub.overflowed = true;
    sub.waiting = [];
    return;
  }
  sub.waiting.push(event);
}

async function unsubscribe(opId: OpId, id: SubscriptionId) {
  try {
    await api.opUnsubscribe(opId, id);
  } catch (e) {
    // While locked the gate refuses it, and the lock has ended the subscription anyway.
    if (!isLocked(e)) console.error(`op_unsubscribe for operation ${opId} failed:`, e);
  }
}

/**
 * Stop applying `sub`; `tellRust` ends it there too, once its id is known (an answer that
 * arrives for an ended subscription is unsubscribed then). Rust treats an unknown subscription
 * as nothing to end, and while locked refuses the call, which is ignored.
 */
function end(entry: Entry, sub: Subscription, tellRust: boolean) {
  if (sub.status === 'ended') return;
  sub.status = 'ended';
  sub.waiting = [];
  if (tellRust && sub.id !== null) void unsubscribe(entry.opId, sub.id);
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

/** `sub` cannot be trusted any more (its backlog overflowed): end it and follow again. */
function refollow(entry: Entry, sub: Subscription) {
  end(entry, sub, true);
  if (entry.current === sub) entry.current = null;
  if (entry.followers > 0 && entry.current === null) void follow(entry);
}

async function follow(entry: Entry) {
  const sub = openChannel(entry);
  entry.current = sub;
  let answer: Subscribed;
  try {
    answer = await api.opSubscribe(entry.opId, sub.channel);
  } catch (e) {
    const abandoned = entry.current !== sub;
    end(entry, sub, false);
    if (abandoned) return;
    entry.current = null;
    // Refused because locked: reattachAll follows it again after the unlock.
    if (!isLocked(e)) {
      entry.view = { ...entry.view, attachError: uiErrorOf(e) };
      publish();
    }
    return;
  }
  sub.id = answer.subscription;
  if (sub.status === 'ended' || entry.current !== sub) {
    // Released, or cleared by a lock, while the answer was on its way.
    sub.status = 'ended';
    void unsubscribe(entry.opId, answer.subscription);
    return;
  }
  if (sub.overflowed) {
    refollow(entry, sub);
    return;
  }
  goLive(entry, sub, answer.replay);
}

/** Follow `opId` until the returned release is called (once; later calls do nothing). */
export function attach(opId: OpId): () => void {
  const entry = entryFor(opId);
  entry.followers += 1;
  if (entry.current === null) void follow(entry);
  publish();
  return releaser(entry);
}

function releaser(entry: Entry): () => void {
  let released = false;
  return () => {
    if (released) return;
    released = true;
    entry.followers -= 1;
    if (entry.followers > 0) return;
    if (entry.current !== null) end(entry, entry.current, true);
    entry.current = null;
    forgetLive(entry);
    prune(entry);
    publish();
  };
}

/**
 * Run the plan `opId` and follow it; resolves with the release once Rust has started it. The
 * execute counts as a follower from the start, so nothing drops the entry while it answers.
 * `password`: the confirm dialog's own field, where the OS cannot prompt; Rust checks it in
 * place of the gesture. It is sent once and kept nowhere.
 *
 * The rejection is the answer, thrown as it came. The `Failed` that Rust may also send on this
 * call's channel is dropped, so the error shows once. A subscription the store already held for
 * the plan is held still while execute answers: when it starts the operation that subscription
 * ends (both carry the same events from the start); a busy prompt or a failed gesture (a wrong
 * password, the back-off) leaves the plan waiting for another try with the same `opId`, and the
 * subscription resumes; any other refusal ends the plan, and the subscription with it — its own
 * `Failed` would repeat the answer.
 */
export async function execute(opId: OpId, password?: string): Promise<() => void> {
  const entry = entryFor(opId);
  entry.followers += 1;
  const release = releaser(entry);
  const sub = openChannel(entry);
  const started = epoch;
  entry.executing += 1;
  let id: SubscriptionId;
  try {
    id = await api.opExecute(opId, sub.channel, password);
  } catch (e) {
    entry.executing -= 1;
    end(entry, sub, false);
    const previous = entry.current;
    if (previous !== null && leavesThePlanWaiting(e)) {
      resume(entry, previous);
    } else if (previous !== null) {
      end(entry, previous, true);
      entry.current = null;
      entry.view = { ...entry.view, live: false };
    }
    release();
    publish();
    throw e;
  }
  entry.executing -= 1;
  sub.id = id;
  if (epoch !== started) {
    // A lock came in between: Rust dropped this subscription (an unlock drops it too, so this
    // one is ended there in any case); reattachAll follows the operation from its replay.
    end(entry, sub, true);
    publish();
    return release;
  }
  const previous = entry.current;
  if (previous !== null) end(entry, previous, true);
  if (sub.overflowed) {
    refollow(entry, sub);
    return release;
  }
  goLive(entry, sub, []);
  return release;
}

/** A subscription held while an execute answered goes on: what it received meanwhile applies. */
function resume(entry: Entry, sub: Subscription) {
  if (sub.overflowed) {
    refollow(entry, sub);
    return;
  }
  const held = sub.waiting;
  sub.waiting = [];
  apply(entry, held);
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
  prune(entry);
  publish();
}

/** Read op_list: the listed operations' summaries; an unlisted one nobody follows goes. */
export async function refreshList(): Promise<void> {
  const list = await api.opList();
  const listed = new Set(list.map((s) => s.opId));
  for (const entry of [...entries.values()]) {
    if (listed.has(entry.opId)) continue;
    entry.view = { ...entry.view, summary: null };
    prune(entry);
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
 * The lock changed. Rust ended every subscription it held; one made after an unlock whose answer
 * reached JS before its `lock-changed` is ended there now. The live state and the summaries go;
 * the followers stay counted for reattachAll.
 */
export function clearLive(): void {
  epoch += 1;
  for (const entry of [...entries.values()]) {
    if (entry.current !== null) end(entry, entry.current, true);
    entry.current = null;
    forgetLive(entry);
    entry.view = { ...entry.view, summary: null };
    prune(entry);
  }
  publish();
}

/** After an unlock: follow again, from the replay, every operation still followed. */
export function reattachAll(): void {
  for (const entry of entries.values()) {
    if (entry.followers > 0 && entry.current === null) void follow(entry);
  }
}

/** Forget everything without telling Rust; for tests. */
export function resetOperations(): void {
  epoch += 1;
  for (const entry of entries.values()) {
    if (entry.current !== null) end(entry, entry.current, false);
  }
  entries.clear();
  publish();
}

const droppedLine = (bytes: number): OpLine => ({
  kind: 'dropped',
  bytes,
  text: `Earlier output was dropped (${bytes} bytes).`,
});

const messagesDroppedLine = (count: number): OpLine => ({
  kind: 'messages_dropped',
  count,
  text: count === 1 ? '1 earlier message was dropped' : `${count} earlier messages were dropped`,
});

/** Count `bytes` more of dropped output in the leading `dropped` line. */
function addDropped(lines: OpLine[], bytes: number) {
  if (bytes === 0) return;
  const first = lines[0];
  if (first?.kind === 'dropped') lines[0] = droppedLine(first.bytes + bytes);
  else lines.unshift(droppedLine(bytes));
}

/** Count `count` more dropped messages in the `messages_dropped` line, after `dropped`. */
function addDroppedMessages(lines: OpLine[], count: number) {
  if (count === 0) return;
  const at = lines[0]?.kind === 'dropped' ? 1 : 0;
  const line = lines[at];
  if (line?.kind === 'messages_dropped') lines[at] = messagesDroppedLine(line.count + count);
  else lines.splice(at, 0, messagesDroppedLine(count));
}

/** Past a cap, the oldest output lines and the oldest messages go, each counted in its line. */
function trim(entry: Entry, lines: OpLine[]) {
  let bytes = 0;
  for (let i = 0; i < lines.length && entry.outputBytes > OUTPUT_CAP; ) {
    const line = lines[i];
    if (line?.kind === 'stdout' || line?.kind === 'stderr') {
      const size = utf8Bytes(line.text);
      entry.outputBytes -= size;
      bytes += size;
      lines.splice(i, 1);
    } else {
      i++;
    }
  }
  addDropped(lines, bytes);
  let messages = 0;
  for (let i = 0; i < lines.length && entry.messages > MESSAGE_CAP; ) {
    const line = lines[i];
    if (line?.kind === 'warning' || line?.kind === 'notice') {
      entry.messages -= 1;
      messages += 1;
      lines.splice(i, 1);
    } else {
      i++;
    }
  }
  addDroppedMessages(lines, messages);
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
        entry.messages += 1;
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
