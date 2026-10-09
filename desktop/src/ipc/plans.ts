// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A confirmed plan and a read, each followed to its end on the operations store: the events
// reach whoever follows the op (the operations sheet, a dialog), and the caller awaits the end.
// A cancelled end is one thing however the core said it (an `Ok(Outcome::Cancelled)` or an
// `Err(Cancelled)`, which Rust ends alike): OperationFailed with `OP_CANCELLED`.
import { CORE_ERROR_CODES } from './generated/core-errors';
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

/** Resolves with `opId`'s end once the store has it. */
export function endOf(opId: OpId): Promise<OpEnd> {
  return new Promise((resolve) => {
    let off = () => {};
    const check = () => {
      const end = operationsSnapshot().get(opId)?.end ?? null;
      if (end === null) return;
      off();
      resolve(end);
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

/** Start a read with `start` (an op_start_* call), follow it to its end, return its result. */
export async function runRead<T>(start: () => Promise<OpId>): Promise<T> {
  const opId = await start();
  const release = attach(opId);
  try {
    return resultOf(await endOf(opId)) as T; // the Rust command's report type
  } finally {
    release();
    discard(opId).catch(reportDiscard(opId));
  }
}
