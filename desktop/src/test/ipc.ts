// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The Rust side as the D.3 flows meet it, for bun tests. Every call is recorded. `answer` overrides
// any command, built-ins included (a value, or a function of the arguments; return
// Promise.reject(uiError) to refuse) — it is checked first, so `answer('op_subscribe', …)` is how a
// test makes Rust refuse to follow. Otherwise: a command given with `read` answers a fresh
// operation id, whose op_subscribe replays the events given (none: it keeps running); one given
// with `plan` answers the PlanView, and op_execute of it sends its events on the execute's
// channel. The rest answer null (op_list: []). Operation ids are unique across every harness of
// a test run, as Rust's are in a process: a read a test left waiting never wakes on a later test's
// operation.
import type { Channel, InvokeArgs } from '@tauri-apps/api/core';
import { mockIPC } from '@tauri-apps/api/mocks';
import type { OpEvent } from '../ipc/generated/OpEvent';
import type { OpId } from '../ipc/generated/OpId';
import type { PlanView } from '../ipc/generated/PlanView';
import type { JsonValue } from '../ipc/generated/serde_json/JsonValue';
import type { UiError } from '../ipc/generated/UiError';

export interface Call {
  readonly cmd: string;
  readonly args: Record<string, unknown>;
}
/** A value, or a function of the call's arguments that returns one (or a rejected promise). */
type Answer = unknown;

export interface Harness {
  readonly calls: Call[];
  answer(cmd: string, value: Answer): void;
  read(cmd: string, events: readonly OpEvent[]): void;
  plan(cmd: string, view: Omit<PlanView, 'opId'>, events: readonly OpEvent[]): void;
  /**
   * A new operation whose op_subscribe replays `events`. Its id comes from the run-wide counter,
   * never one a previous test used: there is no way to name an id of one's own (review #12), so
   * a read an earlier test left waiting never wakes on this test's operation.
   */
  newOperation(events: readonly OpEvent[]): OpId;
  of(cmd: string): Call[];
  /** The operation ids `cmd` was answered with by `read` or `plan`, in order. */
  started(cmd: string): OpId[];
}

let nextOpId = 100;

export const completed = (result: unknown): OpEvent => ({
  kind: 'finished',
  outcome: { status: 'completed', result: result as JsonValue },
});
export const cancelled = (): OpEvent => ({
  kind: 'finished',
  outcome: { status: 'cancelled', cleaned: [], left: [] },
});
export const failed = (error: UiError): OpEvent => ({ kind: 'failed', error });
export const stage = (index: number, total: number, title: string): OpEvent => ({
  kind: 'stage',
  index,
  total,
  title,
});
export const uiError = (code: string, message = code, fields: UiError['fields'] = {}): UiError => ({
  code,
  message,
  help: null,
  causes: [],
  fields,
});

interface Internals {
  runCallback(id: number, data: unknown): void;
}

export function installHarness(): Harness {
  const calls: Call[] = [];
  const answers = new Map<string, Answer>();
  const reads = new Map<string, (readonly OpEvent[])[]>();
  const plans = new Map<string, { view: Omit<PlanView, 'opId'>; events: readonly OpEvent[] }[]>();
  const events = new Map<OpId, readonly OpEvent[]>();
  const answered = new Map<string, OpId[]>();
  const assign = (cmd: string): OpId => {
    nextOpId += 1;
    answered.set(cmd, [...(answered.get(cmd) ?? []), nextOpId]);
    return nextOpId;
  };
  mockIPC((cmd, raw?: InvokeArgs) => {
    const args = (raw ?? {}) as Record<string, unknown>;
    calls.push({ cmd, args });
    // First: an answer overrides reads, plans and the built-ins.
    if (answers.has(cmd)) {
      const a = answers.get(cmd);
      return typeof a === 'function' ? (a as (x: Record<string, unknown>) => unknown)(args) : a;
    }
    const read = reads.get(cmd)?.shift();
    if (read !== undefined) {
      const opId = assign(cmd);
      events.set(opId, read);
      return opId;
    }
    const plan = plans.get(cmd)?.shift();
    if (plan !== undefined) {
      const opId = assign(cmd);
      events.set(opId, plan.events);
      return { ...plan.view, opId };
    }
    if (cmd === 'op_subscribe') {
      return { subscription: 1, replay: events.get(args.opId as OpId) ?? [] };
    }
    if (cmd === 'op_execute') {
      const channel = args.onEvent as Channel<OpEvent>;
      const sent = events.get(args.opId as OpId) ?? [];
      const internals = (window as unknown as { __TAURI_INTERNALS__: Internals })
        .__TAURI_INTERNALS__;
      setTimeout(() => {
        sent.forEach((message, index) => {
          internals.runCallback(channel.id, { index, message });
        });
      }, 0);
      return 2;
    }
    return cmd === 'op_list' ? [] : null;
  });
  return {
    calls,
    answer: (cmd, value) => {
      answers.set(cmd, value);
    },
    read: (cmd, list) => {
      reads.set(cmd, [...(reads.get(cmd) ?? []), list]);
    },
    plan: (cmd, view, list) => {
      plans.set(cmd, [...(plans.get(cmd) ?? []), { view, events: list }]);
    },
    newOperation: (list) => {
      nextOpId += 1;
      events.set(nextOpId, list);
      return nextOpId;
    },
    of: (cmd) => calls.filter((c) => c.cmd === cmd),
    started: (cmd) => answered.get(cmd) ?? [],
  };
}
