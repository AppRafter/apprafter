// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A confirmed plan and a read, each followed to its end on the operations store: the events
// reach whoever follows the op (the operations sheet, a dialog), and the caller awaits the end.
// A cancelled end is one thing however the core said it (an `Ok(Outcome::Cancelled)` or an
// `Err(Cancelled)`, which Rust ends alike): OperationFailed with `OP_CANCELLED`. A follow Rust
// refuses ends the wait with that refusal, so no caller waits forever. A confirmed plan's end
// that arrives when the screen that ran it is gone (a lock, a closed tab) is kept for the app
// (ipc/away.ts) instead of being discarded unseen.
import { IpcError, uiErrorOf } from './api';
import { keepEndedAway } from './away';
import { CORE_ERROR_CODES } from './generated/core-errors';
import { DESKTOP_ERROR_CODES } from './generated/errors';
import type { OpId } from './generated/OpId';
import type { JsonValue } from './generated/serde_json/JsonValue';
import type { UiError } from './generated/UiError';
import {
  attach,
  discard,
  execute,
  type OpEnd,
  operationsSnapshot,
  watchOperations,
} from './operations';

/** The operation ran and failed, or was cancelled: what to show where the action started. */
export class OperationFailed extends Error {
  readonly error: UiError;
  constructor(error: UiError) {
    super(error.message);
    this.name = 'OperationFailed';
    this.error = error;
  }
}

const CANCELLED: UiError = {
  code: CORE_ERROR_CODES.OP_CANCELLED,
  message: 'The operation was cancelled.',
  help: null,
  causes: [],
  fields: {},
};

const reportDiscard = (opId: OpId) => (e: unknown) =>
  console.error(`op_discard ${opId} failed:`, e);

/**
 * Resolves with `opId`'s end once the store has it; rejects with OperationFailed when Rust
 * refused to follow it (the store's attachError), so no caller waits forever.
 */
export function endOf(opId: OpId): Promise<OpEnd> {
  return new Promise((resolve, reject) => {
    let off = () => {};
    const check = () => {
      const view = operationsSnapshot().get(opId);
      if (view?.end != null) {
        off();
        resolve(view.end);
      } else if (view?.attachError != null) {
        off();
        reject(new OperationFailed(view.attachError));
      }
    };
    off = watchOperations(check);
    check();
  });
}

/** A completed operation's result; OperationFailed for a failed or cancelled one. */
export function resultOf(end: OpEnd): JsonValue {
  if (end.state === 'failed') throw new OperationFailed(end.error);
  if (end.outcome.status !== 'completed') throw new OperationFailed(CANCELLED);
  return end.outcome.result;
}

/** The screen that shows a plan's end, and what the end is about when it shows elsewhere. */
export interface PlanOwner {
  /** The plan's title ("Add target lab"). */
  readonly title: string;
  /** Whether the screen is still there to show the end. */
  readonly shown: () => boolean;
}

/** An end in a line, for when it shows away from its screen. */
export function awayText(title: string, end: OpEnd): { text: string; failed: boolean } {
  if (end.state === 'failed') return awayLine(title, end.error);
  if (end.outcome.status !== 'completed') return awayLine(title, CANCELLED);
  return awayLine(title, null);
}

/** A run's end in a line: done (`null`), cancelled, or failed with its error. */
export function awayLine(title: string, error: UiError | null): { text: string; failed: boolean } {
  if (error === null) return { text: `${title}: done.`, failed: false };
  if (error.code === CORE_ERROR_CODES.OP_CANCELLED) {
    return { text: `${title} was cancelled.`, failed: true };
  }
  return { text: `${title} failed: ${error.message}`, failed: true };
}

export interface Started {
  /** The operation's end; the op is released and discarded once it is here. */
  readonly ended: Promise<OpEnd>;
}

/**
 * Run the confirmed plan; resolves once Rust started it. Rejects as op_execute does: some
 * refusals (a wrong password, a busy prompt) leave the plan waiting for another try.
 */
export async function startPlan(
  opId: OpId,
  password?: string,
  owner?: PlanOwner,
): Promise<Started> {
  const release = await execute(opId, password);
  const gone = () => owner !== undefined && !owner.shown();
  const ended = endOf(opId).then(
    (end) => {
      release();
      // Its screen went: the end waits for the app to show it, and is discarded only then.
      if (gone() && owner !== undefined) keepEndedAway({ opId, ...awayText(owner.title, end) });
      else discard(opId).catch(reportDiscard(opId));
      return end;
    },
    (reason: unknown) => {
      release();
      if (gone() && owner !== undefined) {
        const text = `${owner.title} failed: ${uiErrorOf(reason).message}`;
        keepEndedAway({ opId, text, failed: true });
      } else discard(opId).catch(reportDiscard(opId));
      throw reason;
    },
  );
  return { ended };
}

/**
 * Start a read with `start` (an op_start_* call), follow it to its end, return its result.
 * `onStarted` gets the op id as soon as Rust answers (useRead cancels by it).
 */
export async function runRead<T>(
  start: () => Promise<OpId>,
  onStarted?: (opId: OpId) => void,
): Promise<T> {
  const opId = await start();
  onStarted?.(opId);
  const release = attach(opId);
  try {
    return resultOf(await endOf(opId)) as T; // the Rust command's report type
  } finally {
    release();
    discard(opId).catch(reportDiscard(opId));
  }
}

/**
 * Run the confirmed plan to its end: its result, or OperationFailed (failed or cancelled). A
 * refused op_execute (a wrong password) rejects as startPlan does, the plan kept.
 */
export async function runPlan<T>(opId: OpId, password?: string, owner?: PlanOwner): Promise<T> {
  return resultOf(await (await startPlan(opId, password, owner)).ended) as T;
}

/** Whether `reason` is the end of a cancelled operation (resultOf's CANCELLED). */
export const isCancelled = (reason: unknown): boolean =>
  reason instanceof OperationFailed && reason.error.code === CORE_ERROR_CODES.OP_CANCELLED;

/** What to show for a failed runRead / runPlan, or for a refused IPC call. */
export const failureOf = (reason: unknown): UiError =>
  reason instanceof OperationFailed ? reason.error : uiErrorOf(reason);

/** Logs a failure, unless it is the lock gate's refusal (a lock that landed meanwhile). */
export function reportUnlessLocked(what: string): (error: unknown) => void {
  return (error) => {
    if (error instanceof IpcError && error.error.code === DESKTOP_ERROR_CODES.LOCKED) return;
    console.error(`${what} failed:`, error);
  };
}
