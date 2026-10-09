// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The system's LocalAuthentication: the only code in this crate that calls the framework.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use apprafter_core::CancellationToken;
use block2::RcBlock;
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::Bool;
use objc2_foundation::{NSError, NSString};
use objc2_local_authentication::{LAContext, LAErrorDomain, LAPolicy};

use super::{wait_for, Asked, LaError, LocalAuthentication, Pending, TIMING};

/// Touch ID or an Apple Watch where the Mac has them, the account password otherwise.
const POLICY: LAPolicy = LAPolicy::DeviceOwnerAuthentication;

pub struct SystemLocalAuthentication;

impl LocalAuthentication for SystemLocalAuthentication {
    fn can_evaluate(&self) -> Result<(), LaError> {
        autoreleasepool(|_| can_evaluate(&fresh_context()))
    }

    fn evaluate(&self, reason: &str, cancel: &CancellationToken) -> Asked {
        // The framework raises NSInvalidArgumentException for an empty reason, which aborts
        // the process: never ask with one.
        if reason.trim().is_empty() {
            return Asked::NotStarted;
        }
        autoreleasepool(|_| {
            let context = fresh_context();
            if let Err(error) = can_evaluate(&context) {
                return Asked::CannotEvaluate(error);
            }
            let (sender, replies) = mpsc::channel();
            let reply = RcBlock::new(move |success: Bool, error: *mut NSError| {
                let reply = if success.as_bool() {
                    Ok(())
                } else {
                    // SAFETY: the framework passes nil or an NSError that is valid for the
                    // duration of the reply block; it is only read here, not kept.
                    Err(unsafe { error.as_ref() }.and_then(la_code))
                };
                // The waiter may have given up and gone: nobody needs the reply then.
                let _ = sender.send(reply);
            });
            let reason = NSString::from_str(reason);
            // SAFETY: `POLICY` is a policy the framework defines; `reason` is a valid, non-empty
            // NSString (checked above), which the framework copies; `reply` has the signature
            // the method declares (`void (^)(BOOL success, NSError *error)`) and is sendable: it
            // captures only a channel sender, which is `Send + Sync`, and the framework retains
            // it until it has replied. `context` is kept alive below until the reply arrives or
            // the wait gives up, as the method asks.
            unsafe { context.evaluatePolicy_localizedReason_reply(POLICY, &reason, &reply) };
            wait_for(
                &Evaluation {
                    context: &context,
                    replies,
                },
                cancel,
                TIMING,
            )
        })
    }
}

/// A new context for each request: a used one can pass the policy again without asking.
fn fresh_context() -> Retained<LAContext> {
    // SAFETY: `+new` takes no arguments, and LAContext needs no initialisation beyond it.
    unsafe { LAContext::new() }
}

fn can_evaluate(context: &LAContext) -> Result<(), LaError> {
    // SAFETY: `POLICY` is a policy the framework defines, and objc2 manages the error
    // out-parameter. Never called inside the reply block, where Apple says it can deadlock.
    unsafe { context.canEvaluatePolicy_error(POLICY) }.map_err(|error| la_code(&error))
}

/// The code of an LAError, or `None` for an error of another domain.
fn la_code(error: &NSError) -> Option<isize> {
    // SAFETY: `LAErrorDomain` is an immutable NSString constant the framework exports, valid
    // for the life of the process.
    let domain: &NSString = unsafe { LAErrorDomain };
    error.domain().isEqualToString(domain).then(|| error.code())
}

/// An evaluation in progress on `context`, whose reply block sends into `replies`.
struct Evaluation<'a> {
    context: &'a LAContext,
    replies: Receiver<Result<(), LaError>>,
}

impl Pending for Evaluation<'_> {
    fn wait(&self, timeout: Duration) -> Option<Result<(), LaError>> {
        match self.replies.recv_timeout(timeout) {
            Ok(reply) => Some(reply),
            Err(RecvTimeoutError::Timeout) => None,
            // The reply block was released without being called: no reply will come.
            Err(RecvTimeoutError::Disconnected) => Some(Err(None)),
        }
    }

    fn invalidate(&self) {
        // SAFETY: `context` is a live LAContext on the thread that created it; invalidating
        // one that is evaluating, or already invalid, is documented and has no other effect.
        unsafe { self.context.invalidate() };
    }
}
