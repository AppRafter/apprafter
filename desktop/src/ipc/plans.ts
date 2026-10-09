// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A confirmed plan and a read, each followed to its end on the operations store: the events
// reach whoever follows the op (the operations sheet, a dialog), and the caller awaits the end.
// A cancelled end is one thing however the core said it (an `Ok(Outcome::Cancelled)` or an
// `Err(Cancelled)`, which Rust ends alike): OperationFailed with `OP_CANCELLED`. A follow Rust
// refuses ends the wait with that refusal, so no caller waits forever.
import { IpcError, uiErrorOf } from './api';
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

export interface Started {
  /** The operation's end; the op is released and discarded once it is here. */
  readonly ended: Promise<OpEnd>;
}

/**
 * Run the confirmed plan; resolves once Rust started it. Rejects as op_execute does: some
 * refusals (a wrong password, a busy prompt) leave the plan waiting for another try.
 */
export async function startPlan(opId: OpId, password?: string): Promise<Started> {
  const release = await execute(opId, password);
  const ended = endOf(opId).finally(() => {
    release();
    discard(opId).catch(reportDiscard(opId));
  });
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
export async function runPlan<T>(opId: OpId, password?: string): Promise<T> {
  return resultOf(await (await startPlan(opId, password)).ended) as T;
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
