//! A Git operation has its own PTY and input epoch. Its completion sender is
//! released only after same-session descendants and the leader are reaped.
use super::children;
use crate::git::CommandOutcome;
use crate::git_wire::{GitOperationInfo, GitOperationSnapshot, GitOperationState};
use crate::protocol::{safe_error, InputOwner};
use crate::terminal::{TerminalExit, TerminalSession, TerminalSnapshot};
use anyhow::{ensure, Context, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

pub(super) struct Operation {
    pub info: GitOperationInfo,
    pub cancelled: Arc<AtomicBool>,
    pub finished_at: Option<Instant>,
    terminal: Option<TerminalSession>,
    completion: Option<mpsc::SyncSender<CommandOutcome>>,
    frozen: Option<TerminalSnapshot>,
    exit: Option<TerminalExit>,
    exited_at: Option<Instant>,
    force: bool,
    cleanup_done: bool,
    signalled: HashMap<libc::pid_t, bool>,
    executed: bool,
}
impl Operation {
    pub fn new(info: GitOperationInfo) -> Self {
        Self {
            info,
            cancelled: Arc::new(AtomicBool::new(false)),
            finished_at: None,
            terminal: None,
            completion: None,
            frozen: None,
            exit: None,
            exited_at: None,
            force: false,
            cleanup_done: false,
            signalled: HashMap::new(),
            executed: false,
        }
    }
    pub fn active(&self) -> bool {
        self.finished_at.is_none()
    }
    pub fn install(
        &mut self,
        terminal: TerminalSession,
        completion: mpsc::SyncSender<CommandOutcome>,
    ) {
        self.terminal = Some(terminal);
        self.executed = true;
        self.completion = Some(completion);
        self.info.terminal_available = true;
        self.info.state = if self.cancelled.load(Ordering::Acquire) {
            GitOperationState::Cancelling
        } else {
            GitOperationState::Running
        };
        if self.cancelled.load(Ordering::Acquire) {
            if let Err(error) = self
                .terminal
                .as_mut()
                .unwrap()
                .request_host_close(self.force)
            {
                self.notice(safe_error(error));
            }
        }
    }
    fn owner(&self, client: &str, epoch: u64) -> Result<()> {
        ensure!(
            self.info
                .owner
                .as_ref()
                .is_some_and(|owner| owner.client_id == client)
                && self.info.input_epoch == epoch,
            "Git operation input ownership changed; attach or explicitly take over"
        );
        Ok(())
    }
    fn advance(&mut self) -> Result<()> {
        self.info.input_epoch = self
            .info
            .input_epoch
            .checked_add(1)
            .context("Git operation ownership epoch exhausted")?;
        Ok(())
    }
    pub fn attach(&mut self, client: &str, takeover: bool) -> Result<GitOperationInfo> {
        if self.active() {
            ensure!(
                self.info
                    .owner
                    .as_ref()
                    .is_none_or(|owner| owner.client_id == client)
                    || takeover,
                "Git operation is owned by another client; explicit takeover required"
            );
            if self
                .info
                .owner
                .as_ref()
                .is_none_or(|owner| owner.client_id != client)
            {
                self.advance()?;
                self.info.owner = Some(InputOwner {
                    client_id: client.into(),
                });
            }
        }
        Ok(self.info.clone())
    }
    pub fn detach(&mut self, client: &str, epoch: u64) -> Result<GitOperationInfo> {
        if self.active() {
            self.owner(client, epoch)?;
            self.advance()?;
            self.info.owner = None;
        }
        Ok(self.info.clone())
    }
    pub fn input(&mut self, client: &str, epoch: u64, bytes: &[u8]) -> Result<()> {
        self.owner(client, epoch)?;
        ensure!(
            self.active() && self.exit.is_none(),
            "Git operation is not accepting input"
        );
        self.terminal
            .as_mut()
            .context("Git operation PTY is not ready")?
            .input(bytes)
    }
    pub fn resize(&mut self, client: &str, epoch: u64, rows: u16, cols: u16) -> Result<()> {
        self.owner(client, epoch)?;
        self.terminal
            .as_mut()
            .context("Git operation PTY is not available")?
            .resize(rows, cols)
    }
    pub fn cancel(&mut self, client: &str, epoch: u64, force: bool) -> Result<GitOperationInfo> {
        if self.active() {
            self.owner(client, epoch)?;
            self.cancelled.store(true, Ordering::Release);
            self.force |= force;
            self.info.state = GitOperationState::Cancelling;
            if let Some(terminal) = &mut self.terminal {
                terminal.request_host_close(self.force)?;
            }
        }
        Ok(self.info.clone())
    }
    pub fn snapshot(&self, since: Option<u64>) -> Result<GitOperationSnapshot> {
        let screen = if let Some(screen) = &self.frozen {
            (since != Some(screen.generation)).then(|| screen.clone())
        } else if let Some(terminal) = &self.terminal {
            terminal.snapshot_since(since)?
        } else {
            None
        };
        let mut operation = self.info.clone();
        if let Some(screen) = &screen {
            operation.generation = screen.generation;
        } else if let Some(since) = since {
            operation.generation = since;
        }
        Ok(GitOperationSnapshot { operation, screen })
    }
    pub fn anchor(&self) -> Option<children::Anchor> {
        if self.exit.is_none() || self.cleanup_done {
            return None;
        }
        Some(children::Anchor {
            session: self.info.id.clone(),
            pid: self.terminal.as_ref()?.child_pid()? as libc::pid_t,
        })
    }
    pub fn inventory(&mut self, report: &children::Report) {
        let Some(anchor) = self.anchor() else {
            return;
        };
        if !report
            .anchors
            .iter()
            .any(|value| value.session == anchor.session && value.pid == anchor.pid)
        {
            return;
        }
        let mut found = 0usize;
        let mut failure = report.error.clone();
        for process in report
            .processes
            .iter()
            .filter(|process| process.session == anchor.pid)
        {
            found += 1;
            if process.parent != std::process::id() as libc::pid_t {
                continue;
            }
            let result = match children::verify_child(process.pid, anchor.pid) {
                Ok(children::ChildState::Exited) => {
                    let result = children::reap_child(process.pid);
                    if result.is_ok() {
                        self.signalled.remove(&process.pid);
                    }
                    result
                }
                Ok(children::ChildState::Running) => {
                    if self.cancelled.load(Ordering::Acquire)
                        && self.signalled.get(&process.pid).copied() != Some(self.force)
                    {
                        let result = children::signal_child(process.pid, self.force);
                        if result.is_ok() {
                            self.signalled.insert(process.pid, self.force);
                        }
                        result
                    } else {
                        Ok(())
                    }
                }
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                failure = Some(safe_error(error));
            }
        }
        self.cleanup_done = report.complete && failure.is_none() && found == 0;
        if let Some(error) = failure {
            self.notice(format!("descendant inventory incomplete: {error}"));
        } else if found > 0 {
            self.notice(format!("{found} same-session processes remain; completion and source lease remain pending; explicit force cancel is available"));
        }
    }
    pub fn notice(&mut self, message: impl ToString) {
        if self
            .info
            .error
            .as_ref()
            .is_none_or(|error| error.starts_with("Git process cleanup:"))
        {
            self.info.error = Some(safe_error(format!(
                "Git process cleanup: {}",
                message.to_string()
            )));
        }
    }
    pub fn tick(&mut self) {
        let Some(terminal) = &mut self.terminal else {
            return;
        };
        if self.frozen.is_none() {
            if let Ok(status) = terminal.status() {
                self.info.generation = status.generation;
                if let Some(error) = status.error {
                    self.info.error = Some(safe_error(error));
                }
            }
        }
        let observed = match terminal.try_wait() {
            Ok(value) => value,
            Err(error) => {
                self.info.error = Some(safe_error(error));
                return;
            }
        };
        if let Some(exit) = observed {
            if self.exit.is_none() {
                self.exit = Some(exit);
                self.exited_at = Some(Instant::now());
                self.info.terminal_available = false;
            }
            if self.frozen.is_none()
                && (terminal.status().is_ok_and(|status| status.reader_closed)
                    || self
                        .exited_at
                        .is_some_and(|time| time.elapsed() >= Duration::from_secs(2)))
            {
                if !terminal.status().is_ok_and(|status| status.reader_closed) {
                    if let Err(error) = terminal.end_collection() {
                        self.info.error = Some(safe_error(error));
                    }
                }
                self.frozen = terminal.snapshot().ok();
                if let Some(screen) = &self.frozen {
                    self.info.generation = screen.generation;
                }
            }
            if self.cleanup_done
                && (self.frozen.is_some()
                    || self
                        .exited_at
                        .is_some_and(|time| time.elapsed() >= Duration::from_secs(2)))
            {
                match terminal.reap_exit() {
                    Ok(exit) => {
                        let outcome = CommandOutcome {
                            exit_code: Some(exit.code),
                            cancelled: self.cancelled.load(Ordering::Acquire),
                            output_limited: self.frozen.as_ref().is_none_or(|screen| {
                                screen.output_limited || screen.error.is_some()
                            }),
                        };
                        self.terminal.take();
                        self.signalled.clear();
                        if let Some(completion) = self.completion.take() {
                            let _ = completion.try_send(outcome);
                        }
                    }
                    Err(error) => {
                        self.info.error = Some(safe_error(error));
                    }
                }
            }
        }
    }
    pub fn finish(&mut self, result: Result<crate::git_wire::GitOperationResult>) {
        if self.terminal.is_some() {
            self.info.state = GitOperationState::Unknown;
            self.info.error = Some(
                "Git executor returned before its owned process cleanup; outcome is unconfirmed"
                    .into(),
            );
            return;
        }
        self.finished_at = Some(Instant::now());
        self.info.owner = None;
        self.info.terminal_available = false;
        match result {
            Ok(result) => {
                self.info.state = if result.outcome == crate::git_wire::GitOutcome::Unknown {
                    GitOperationState::Unknown
                } else {
                    GitOperationState::Complete
                };
                self.info.result = Some(result);
                if self
                    .info
                    .error
                    .as_ref()
                    .is_some_and(|error| error.starts_with("Git process cleanup:"))
                {
                    self.info.error = None;
                }
            }
            Err(error) => {
                self.info.state = if self.executed {
                    GitOperationState::Unknown
                } else {
                    GitOperationState::Complete
                };
                self.info.error = Some(safe_error(error));
            }
        }
    }
    pub fn forget_screen(&mut self) {
        if !self.active() {
            self.frozen = None;
            if let Some(result) = &mut self.info.result {
                if result.after.take().is_some() {
                    result.warnings.push("Historical post-operation snapshot is no longer retained; refresh repository state.".into());
                }
            }
        }
    }
}
