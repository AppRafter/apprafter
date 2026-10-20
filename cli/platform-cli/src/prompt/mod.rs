// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Asking the person at the controlling terminal (D.4a T61). The core never prompts (ADR 0067
//! §2): what asks for a lock password or a recovery key is here, behind [`Asker`], so the
//! dialogues of `lock on` / `lock off` and the prompt of a locked store's commands are unit-tested
//! with a script ([`Script`]) and run against the terminal ([`Terminal`]).

pub(crate) mod secret;

use apprafter_core::SecretString;

pub(crate) use secret::{PromptError, PROMPT_TIMEOUT};

/// Where the CLI asks the person something.
pub(crate) trait Asker: Send + Sync {
    /// A person can be asked here: the controlling terminal is stdin's (T61).
    fn present(&self) -> bool;
    /// Shows `text` on the terminal itself, never on stderr.
    fn tell(&self, text: &str) -> Result<(), PromptError>;
    /// A secret: nothing typed is shown.
    fn ask(&self, prompt: &str) -> Result<SecretString, PromptError>;
    /// A question whose answer is shown as it is typed (`Use it anyway? [y/N]`).
    fn answer(&self, prompt: &str) -> Result<String, PromptError>;
}

/// `text` with every control character but a newline shown as `?` (critic finding 14): a
/// process name (`prctl(PR_SET_NAME)` sets it freely), a target directory or a path can carry
/// ESC / CSI bytes that would rewrite the prompt's lines — the very lines that let a person
/// tell their own command from an agent's.
pub(crate) fn printable(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() && c != '\n' { '?' } else { c })
        .collect()
}

/// The controlling terminal, opened for each question. Every text it shows goes through
/// [`printable`].
pub(crate) struct Terminal;

impl Asker for Terminal {
    fn present(&self) -> bool {
        secret::terminal_present()
    }

    fn tell(&self, text: &str) -> Result<(), PromptError> {
        secret::Tty::open()?
            .say(&printable(text))
            .map_err(PromptError::Io)
    }

    fn ask(&self, prompt: &str) -> Result<SecretString, PromptError> {
        // D.4a T57: not dumpable from before a secret is typed until this process exits.
        crate::holder_window::harden_for_secrets();
        secret::Tty::open()?.read_secret(&printable(prompt), PROMPT_TIMEOUT)
    }

    fn answer(&self, prompt: &str) -> Result<String, PromptError> {
        secret::Tty::open()?.read_answer(&printable(prompt), PROMPT_TIMEOUT)
    }
}

/// A scripted person, for unit tests: the answers in order, what was told and asked recorded.
#[cfg(test)]
pub(crate) struct Script {
    present: bool,
    answers: std::sync::Mutex<std::collections::VecDeque<Result<String, PromptError>>>,
    told: std::sync::Mutex<String>,
    asked: std::sync::Mutex<Vec<String>>,
}

#[cfg(test)]
impl Script {
    /// A person at a terminal who gives `answers`, in order (a secret or an answer alike).
    pub(crate) fn new(answers: Vec<Result<&str, PromptError>>) -> Self {
        Script {
            present: true,
            answers: std::sync::Mutex::new(
                answers.into_iter().map(|a| a.map(str::to_string)).collect(),
            ),
            told: std::sync::Mutex::new(String::new()),
            asked: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// No terminal: nothing may be asked.
    pub(crate) fn absent() -> Self {
        Script {
            present: false,
            ..Script::new(Vec::new())
        }
    }

    /// Everything told so far.
    pub(crate) fn told(&self) -> String {
        self.told.lock().unwrap().clone()
    }

    /// Every prompt asked so far, in order.
    pub(crate) fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }

    fn next(&self, prompt: &str) -> Result<String, PromptError> {
        assert!(self.present, "asked `{prompt}` with no terminal");
        self.asked.lock().unwrap().push(prompt.to_string());
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("the script has no answer for `{prompt}`"))
    }
}

#[cfg(test)]
impl Asker for Script {
    fn present(&self) -> bool {
        self.present
    }

    fn tell(&self, text: &str) -> Result<(), PromptError> {
        self.told.lock().unwrap().push_str(text);
        Ok(())
    }

    fn ask(&self, prompt: &str) -> Result<SecretString, PromptError> {
        self.next(prompt).map(SecretString::new)
    }

    fn answer(&self, prompt: &str) -> Result<String, PromptError> {
        self.next(prompt)
    }
}

#[cfg(test)]
mod tests {
    use super::printable;

    /// Critic finding 14: a name holding an escape sequence cannot rewrite the prompt's line;
    /// a newline stays, and text that is only printable is unchanged.
    #[test]
    fn control_characters_are_shown_as_question_marks() {
        assert_eq!(printable("a\x1b[2Kb\rc\u{9b}d\n"), "a?[2Kb?c?d\n");
        assert_eq!(printable("prod — ~/store\n"), "prod — ~/store\n");
    }
}
