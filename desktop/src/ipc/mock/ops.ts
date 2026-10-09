// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A stand-in for Rust's OperationManager (desktop/src-tauri/src/ops/manager.rs), as far as a
// page can tell: plans by op id that run once on op_execute (a destructive one after the
// gesture), reads that start at once, events through the page's Channel in order, a replay for a
// late op_subscribe, op_cancel, op_discard, op_list, and Rust's lock hook (`transition`).
import type { Channel, InvokeArgs } from '@tauri-apps/api/core';
import { DESKTOP_ERROR_CODES } from '../generated/errors';
import type { OpEvent } from '../generated/OpEvent';
import type { OpId } from '../generated/OpId';
import type { OpState } from '../generated/OpState';
import type { OpSummary } from '../generated/OpSummary';
import type { PlanView } from '../generated/PlanView';
import type { Subscribed } from '../generated/Subscribed';
import type { SubscriptionId } from '../generated/SubscriptionId';
import type { JsonValue } from '../generated/serde_json/JsonValue';
import type { UiError } from '../generated/UiError';

/** Rust's PLAN_TTL_MS (ops.test.ts holds the two equal): a plan runs within 10 minutes. */
export const MOCK_PLAN_TTL_MS = 10 * 60 * 1000;

export type MockResult = { readonly result: JsonValue } | { readonly error: UiError };

/** What a mock operation does when it runs: its events, then how it ends (asked only then). */
export interface MockRun {
  readonly events?: readonly OpEvent[];
  readonly end: () => MockResult;
}

export interface MockOpsOptions {
  /** How long an operation takes before it reports: dev mode shows it running; tests use 0. */
  readonly delayMs: number;
  /**
   * A destructive plan's gesture: resolves when verified, rejects as op_execute would. Every
   * refusal it makes is one after which Rust keeps the plan (a wrong password, the back-off,
   * the other way to ask), so a rejection leaves the plan waiting for another try.
   */
  readonly gesture: (password: string | undefined) => Promise<void>;
}

/** One mock command's answer; exported for `targets.ts` and D.3e's `flows.ts`, which return `Record<string, Handler>`. */
export type Handler = (args: InvokeArgs | undefined) => unknown;

type OpCommand =
  | 'op_list'
  | 'op_subscribe'
  | 'op_unsubscribe'
  | 'op_cancel'
  | 'op_discard'
  | 'op_execute';

export interface MockOps {
  registerPlan(view: Omit<PlanView, 'opId' | 'expiresAtMs'>, run: MockRun): PlanView;
  startRead(title: string, target: string | null, run: MockRun): OpId;
  /** Rust's lock hook, on every lock and unlock. */
  transition(): void;
  readonly handlers: Readonly<Record<OpCommand, Handler>>;
}

interface Sink {
  readonly id: SubscriptionId;
  readonly channel: Channel<OpEvent>;
  next: number;
}

interface Plan {
  readonly view: PlanView;
  readonly run: MockRun;
  sinks: Sink[];
}

/** A destructive plan while its gesture is asked: out of the plans, so it cannot run twice. */
interface Prompt {
  readonly plan: Plan;
  /** A cancel or a lock came while asking: the plan is refused, whatever the answer. */
  refused: boolean;
}

interface Op {
  summary: OpSummary;
  readonly events: OpEvent[];
  sinks: Sink[];
  ended: boolean;
  /** Started by startRead; a lock cancels it. An executed plan runs on. */
  readonly read: boolean;
}

interface OpArgs {
  readonly opId: OpId;
  readonly onEvent: Channel<OpEvent>;
  readonly subscription: SubscriptionId;
  readonly password?: string;
}

/** The op commands' arguments, as api.ts sends them (camelCase, the Channel object itself). */
const argsOf = (args: InvokeArgs | undefined) => (args ?? {}) as unknown as OpArgs;

const internals = () =>
  (
    window as unknown as {
      __TAURI_INTERNALS__?: { runCallback?: (id: number, data: unknown) => void };
    }
  ).__TAURI_INTERNALS__;

/** Rust's side of a Channel: message `index`, counted per channel from 0. */
function send(sink: Sink, message: OpEvent) {
  // After clearMocks (a test ended) a late timer finds no IPC to send through.
  internals()?.runCallback?.(sink.channel.id, { index: sink.next, message });
  sink.next += 1;
}

function fanOut(sinks: readonly Sink[], event: OpEvent) {
  for (const sink of sinks) send(sink, event);
}

// DesktopError's texts and fields (desktop/src-tauri/src/errors.rs).
const desktopError = (
  code: string,
  message: string,
  help: string | null,
  fields: UiError['fields'] = {},
): UiError => ({ code, message, help, causes: [], fields });

const planNotFound = (opId: OpId) =>
  desktopError(
    DESKTOP_ERROR_CODES.PLAN_NOT_FOUND,
    `operation ${opId} has no pending plan`,
    'It already ran, was discarded, or was dropped when AppRafter locked; plan it again.',
    { opId },
  );

const planExpired = (opId: OpId) =>
  desktopError(
    DESKTOP_ERROR_CODES.PLAN_EXPIRED,
    `the plan for operation ${opId} expired`,
    'A plan is valid for 10 minutes; plan it again.',
    { opId },
  );

const LOCKED = desktopError(
  DESKTOP_ERROR_CODES.LOCKED,
  'AppRafter is locked',
  'Unlock AppRafter to continue.',
);

const AUTH_CANCELLED = desktopError(
  DESKTOP_ERROR_CODES.AUTH_CANCELLED,
  'authentication was cancelled',
  null,
);

const CANCELLED: OpEvent = {
  kind: 'finished',
  outcome: { status: 'cancelled', cleaned: [], left: [] },
};

export function createMockOps(options: MockOpsOptions): MockOps {
  const plans = new Map<OpId, Plan>();
  const prompts = new Map<OpId, Prompt>();
  const ops = new Map<OpId, Op>();
  // One counter for plans and reads, as Rust's: an executed plan keeps its id.
  let nextOpId = 0;
  let nextSubscription = 0;

  const sinkFor = (channel: Channel<OpEvent>): Sink => {
    nextSubscription += 1;
    return { id: nextSubscription, channel, next: 0 };
  };

  /** Keep the event for a later replay and send it to every subscriber. */
  const emit = (op: Op, event: OpEvent) => {
    op.events.push(event);
    fanOut(op.sinks, event);
  };

  /** End `op` once, with `event`: sent, kept, and the subscribers dropped. */
  const finish = (op: Op, event: OpEvent, state: OpState) => {
    if (op.ended) return;
    op.ended = true;
    emit(op, event);
    op.sinks = [];
    op.summary = { ...op.summary, state };
  };

  const launch = (op: Op, run: MockRun) => {
    setTimeout(() => {
      if (op.ended) return;
      for (const event of run.events ?? []) emit(op, event);
      const end = run.end();
      if ('result' in end) {
        const outcome = { status: 'completed', result: end.result } as const;
        finish(op, { kind: 'finished', outcome }, 'finished');
      } else {
        finish(op, { kind: 'failed', error: end.error }, 'failed');
      }
    }, options.delayMs);
  };

  const begin = (
    opId: OpId,
    title: string,
    target: string | null,
    read: boolean,
    sinks: Sink[],
    run: MockRun,
  ) => {
    const op: Op = {
      summary: { opId, title, target, state: 'running', startedAtMs: Date.now() },
      events: [],
      sinks,
      ended: false,
      read,
    };
    ops.set(opId, op);
    launch(op, run);
  };

  /** Ask the gesture of a destructive plan: resolves when it may run, rejects as Rust refuses. */
  const confirmed = async (plan: Plan, sink: Sink, password: string | undefined) => {
    const opId = plan.view.opId;
    const prompt: Prompt = { plan, refused: false };
    prompts.set(opId, prompt);
    try {
      await options.gesture(password);
    } catch (refusal) {
      prompts.delete(opId);
      if (!prompt.refused) {
        // Rust keeps the plan for another try under the same id; this call's sink goes.
        plan.sinks = plan.sinks.filter((s) => s !== sink);
        plans.set(opId, plan);
        throw refusal;
      }
    }
    prompts.delete(opId);
    if (!prompt.refused) return;
    fanOut(plan.sinks, { kind: 'failed', error: AUTH_CANCELLED });
    throw AUTH_CANCELLED;
  };

  const opExecute: Handler = async (args) => {
    const { opId, onEvent, password } = argsOf(args);
    const plan = plans.get(opId);
    if (plan === undefined) throw planNotFound(opId);
    plans.delete(opId);
    // As Rust's op_execute: this call's channel subscribes to the plan first.
    const sink = sinkFor(onEvent);
    plan.sinks.push(sink);
    if (Date.now() > plan.view.expiresAtMs) {
      const expired = planExpired(opId);
      fanOut(plan.sinks, { kind: 'failed', error: expired });
      throw expired;
    }
    if (plan.view.class === 'destructive') await confirmed(plan, sink, password);
    begin(opId, plan.view.title, plan.view.target, false, plan.sinks, plan.run);
    return sink.id;
  };

  const opSubscribe: Handler = (args) => {
    const { opId, onEvent } = argsOf(args);
    const op = ops.get(opId);
    if (op !== undefined) {
      const sink = sinkFor(onEvent);
      if (!op.ended) op.sinks.push(sink);
      return { subscription: sink.id, replay: [...op.events] } satisfies Subscribed;
    }
    const plan = plans.get(opId) ?? prompts.get(opId)?.plan;
    if (plan === undefined) return Promise.reject(planNotFound(opId));
    const sink = sinkFor(onEvent);
    plan.sinks.push(sink);
    return { subscription: sink.id, replay: [] } satisfies Subscribed;
  };

  const opUnsubscribe: Handler = (args) => {
    const { opId, subscription } = argsOf(args);
    const holder = plans.get(opId) ?? prompts.get(opId)?.plan ?? ops.get(opId);
    if (holder !== undefined) holder.sinks = holder.sinks.filter((s) => s.id !== subscription);
    return null;
  };

  const opCancel: Handler = (args) => {
    const { opId } = argsOf(args);
    // A plan the page cancels goes without a word.
    if (plans.delete(opId)) return null;
    const prompt = prompts.get(opId);
    if (prompt !== undefined) {
      prompt.refused = true;
      return null;
    }
    const op = ops.get(opId);
    if (op === undefined) return Promise.reject(planNotFound(opId));
    finish(op, CANCELLED, 'cancelled');
    return null;
  };

  const opDiscard: Handler = (args) => {
    const { opId } = argsOf(args);
    if (plans.delete(opId)) return null;
    // A running operation is left alone: cancel it first.
    if (ops.get(opId)?.ended === true) ops.delete(opId);
    return null;
  };

  const opList: Handler = () =>
    [...ops.values()]
      .map((op) => op.summary)
      .sort((a, b) => b.startedAtMs - a.startedAtMs || b.opId - a.opId);

  return {
    registerPlan(view, run) {
      nextOpId += 1;
      const full: PlanView = {
        ...view,
        opId: nextOpId,
        expiresAtMs: Date.now() + MOCK_PLAN_TTL_MS,
      };
      plans.set(full.opId, { view: full, run, sinks: [] });
      return full;
    },
    startRead(title, target, run) {
      nextOpId += 1;
      begin(nextOpId, title, target, true, [], run);
      return nextOpId;
    },
    transition() {
      // drop_all_plans: the pages that followed a plan hear it went; an open prompt is refused.
      for (const plan of plans.values()) fanOut(plan.sinks, { kind: 'failed', error: LOCKED });
      plans.clear();
      for (const prompt of prompts.values()) prompt.refused = true;
      // drop_all_subscribers: every subscription ends; the page follows again after an unlock.
      for (const prompt of prompts.values()) prompt.plan.sinks = [];
      for (const op of ops.values()) op.sinks = [];
      // cancel_reads: each running read ends cancelled on its own, after its subscribers went,
      // so its end reaches the replay only. Executed plans run on: they were confirmed.
      for (const op of ops.values()) if (op.read) finish(op, CANCELLED, 'cancelled');
    },
    handlers: {
      op_list: opList,
      op_subscribe: opSubscribe,
      op_unsubscribe: opUnsubscribe,
      op_cancel: opCancel,
      op_discard: opDiscard,
      op_execute: opExecute,
    },
  };
}
