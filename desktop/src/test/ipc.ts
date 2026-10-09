// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The Rust side as the D.3 flows meet it, for bun tests. Every call is recorded. `answer` overrides
// any command, built-ins included (a value, or a function of the arguments; return
// Promise.reject(uiError) to refuse) — it is checked first, so `answer('op_subscribe', …)` is how a
// test makes Rust refuse to follow. Otherwise: a command given with `read` answers a fresh
// operation id, whose op_subscribe replays the events given (none: it keeps running); one given
// with `plan` answers the PlanView, and op_execute of it sends its events on the execute's
// channel. The rest answer null (op_list: []).
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
  /** An operation the test knows by id (op_subscribe replays its events). */
  operation(opId: OpId, events: readonly OpEvent[]): void;
  of(cmd: string): Call[];
}

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
  let next = 100;
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
      next += 1;
      events.set(next, read);
      return next;
    }
    const plan = plans.get(cmd)?.shift();
    if (plan !== undefined) {
      next += 1;
      events.set(next, plan.events);
      return { ...plan.view, opId: next };
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
    operation: (opId, list) => {
      events.set(opId, list);
    },
    of: (cmd) => calls.filter((c) => c.cmd === cmd),
  };
}
