// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A confirmed plan and a read, each followed to its end on the operations store: the events
// reach whoever follows the op (the operations sheet, a dialog), and the caller awaits the end.
// A cancelled end is one thing however the core said it (an `Ok(Outcome::Cancelled)` or an
// `Err(Cancelled)`, which Rust ends alike): OperationFailed with `OP_CANCELLED`. A follow Rust
// refuses ends the wait with that refusal, so no caller waits forever. A confirmed plan's end
// that arrives when the screen that ran it is gone (a lock, a closed tab) is kept for the app
// (ipc/away.ts) instead of being discarded unseen, and so is a confirm the lock stopped before it
// began (D.4a F18).
import { IpcError, lockStatus, uiErrorOf } from './api';
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
  type OpLine,
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

/** An end in a line, for when it shows away from its screen (ipc/away.ts). */
export interface AwayLine {
  readonly text: string;
  readonly failed: boolean;
  /** A failure's code (`shownCode`), said under the line. */
  readonly code?: string;
}

/** An end in a line, for when it shows away from its screen. */
export function awayText(title: string, end: OpEnd): AwayLine {
  if (end.state === 'failed') return awayLine(title, end.error);
  if (end.outcome.status !== 'completed') return awayLine(title, CANCELLED);
  return awayLine(title, null);
}

/**
 * The code a failure says under its line, as the ErrorPanel says it (WI-483): its own, or none
 * for a cancel, which its line says in words, and for an error that has none.
 */
export function shownCode(error: UiError): { readonly code?: string } {
  return error.code === null || error.code === CORE_ERROR_CODES.OP_CANCELLED
    ? {}
    : { code: error.code };
}

/** A run's end in a line: done (`null`), cancelled, or failed with its error and its code. */
export function awayLine(title: string, error: UiError | null): AwayLine {
  if (error === null) return { text: `${title}: done.`, failed: false };
  if (error.code === CORE_ERROR_CODES.OP_CANCELLED) {
    return { text: `${title} was cancelled.`, failed: true };
  }
  return { text: `${title} failed: ${error.message}`, failed: true, ...shownCode(error) };
}

/**
 * Where a screen hears that a plan was refused, or that its run failed or was cancelled.
 * `change`, the plan's title, comes only with a run's end (WI-483): the screen's failure panel
 * then keeps it for after the unlock if a lock comes while it shows (state/shownFailure.ts). A
 * refusal before anything ran has none: nothing changed.
 */
export type OnFailed = (error: UiError, change?: string) => void;

/** The line for a confirmed plan the lock stopped before it began (D.4a F18). */
export const notStartedText = (title: string): string =>
  `${title} was not started: AppRafter locked before it began.`;

/** What op_execute answers when a lock may have stopped the plan before it began. */
const STOPPED_BY_A_LOCK: ReadonlySet<string> = new Set([
  DESKTOP_ERROR_CODES.LOCKED,
  DESKTOP_ERROR_CODES.AUTH_CANCELLED,
  DESKTOP_ERROR_CODES.PLAN_NOT_FOUND,
]);

/**
 * D.4a F18, item 4 (critic M-4): a confirmed plan the lock stopped before it began is kept for
 * the app, which says so after the unlock — never silent. That is an OS prompt the lock closed
 * (`auth_cancelled`), a typed secret the lock found wrong or a confirm that reached Rust locked
 * (`locked`), and a confirm whose plan the lock dropped on its way (`plan_not_found`). The last
 * two of those codes are also a cancel the person clicked and a plan that expired, with the app
 * unlocked: Rust's lock says which, asked here, since its `lock-changed` may reach the page after
 * this answer — and the screen may still be shown, about to go. Any other refusal is the
 * screen's to show.
 */
async function keepIfTheLockStoppedIt(owner: PlanOwner, reason: unknown): Promise<void> {
  if (!(reason instanceof IpcError)) return;
  const { code } = reason.error;
  if (code === null || !STOPPED_BY_A_LOCK.has(code)) return;
  if (code !== DESKTOP_ERROR_CODES.LOCKED) {
    const state = await lockStatus().catch(() => null);
    if (state?.locked !== true) return;
  }
  keepEndedAway({ opId: null, text: notStartedText(owner.title), failed: true });
}

export interface Started {
  /** The operation's end; the op is released and discarded once it is here. */
  readonly ended: Promise<OpEnd>;
}

/**
 * Run the confirmed plan; resolves once Rust started it. Rejects as op_execute does: some
 * refusals (a wrong password, a busy prompt) leave the plan waiting for another try. One the
 * lock stopped before it began is kept for the app first (`keepIfTheLockStoppedIt`).
 */
export async function startPlan(
  opId: OpId,
  password?: string,
  owner?: PlanOwner,
): Promise<Started> {
  let release: () => void;
  try {
    release = await execute(opId, password);
  } catch (reason) {
    if (owner !== undefined) await keepIfTheLockStoppedIt(owner, reason);
    throw reason;
  }
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
        keepEndedAway({ opId, ...awayLine(owner.title, failureOf(reason)) });
      } else discard(opId).catch(reportDiscard(opId));
      throw reason;
    },
  );
  return { ended };
}

/** A warning or notice an operation reported while it ran (core Event::Warning / Notice). */
export interface OpNote {
  readonly kind: 'warning' | 'notice';
  readonly text: string;
}

/** The warnings and notices among an operation's lines, in order. */
export function notesIn(lines: readonly OpLine[]): OpNote[] {
  return lines.flatMap((line) =>
    line.kind === 'warning' || line.kind === 'notice' ? [{ kind: line.kind, text: line.text }] : [],
  );
}

/** The warnings and notices the store kept of `opId`, in order. */
function notesOf(opId: OpId): OpNote[] {
  return notesIn(operationsSnapshot().get(opId)?.lines ?? []);
}

/**
 * Start a read with `start` (an op_start_* call), follow it to its end, return its result.
 * `onStarted` gets the op id as soon as Rust answers (useRead cancels by it). `onNotes` gets the
 * warnings and notices the read reported, however it ended, before the op is discarded with them
 * (review #5: doctor's failed sweep of decrypted kubeconfig copies is one).
 */
export async function runRead<T>(
  start: () => Promise<OpId>,
  onStarted?: (opId: OpId) => void,
  onNotes?: (notes: readonly OpNote[]) => void,
): Promise<T> {
  const opId = await start();
  onStarted?.(opId);
  const release = attach(opId);
  try {
    return resultOf(await endOf(opId)) as T; // the Rust command's report type
  } finally {
    const notes = notesOf(opId);
    release();
    discard(opId).catch(reportDiscard(opId));
    if (notes.length > 0) onNotes?.(notes);
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
