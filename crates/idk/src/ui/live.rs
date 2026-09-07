use super::{
    forms::TextInput,
    runtime::{Runtime, Tag},
    App, Dialog, UiOutcome,
};
use crate::{model::TerminalDefinition, protocol::*, terminal::MAX_TERMINAL_CELLS};
use anyhow::{ensure, Context, Result};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use ratatui::layout::Rect;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub(super) enum LiveDialog {
    Confirm {
        title: String,
        lines: Vec<String>,
        request: Box<Request>,
        tag: Tag,
        force: bool,
        scroll: usize,
    },
    Sessions {
        selected: usize,
    },
    Search {
        input: TextInput,
        backwards: bool,
    },
    Copy {
        text: String,
        scroll: usize,
    },
}
impl App<'_> {
    pub fn enable_terminal_runtime(&mut self, launcher: PathBuf) -> Result<()> {
        self.runtime = Some(Runtime::new(self.service.store.clone(), launcher)?);
        Ok(())
    }
    pub fn attach_session(&mut self, session: &str, takeover: bool) -> Result<()> {
        self.send(
            Request::Attach {
                session: session.into(),
                takeover,
            },
            Tag::Attached,
        )
    }
    fn send(&mut self, request: Request, tag: Tag) -> Result<()> {
        self.runtime
            .as_mut()
            .context("Terminal runtime is unavailable.")?
            .submit(Some(request), tag)
    }
    pub fn tick(&mut self) {
        let replies = self
            .runtime
            .as_mut()
            .map(Runtime::drain)
            .unwrap_or_default();
        for reply in replies {
            let runtime = self.runtime.as_mut().unwrap();
            if let Some((host, client_id)) = reply.identity {
                if runtime
                    .host
                    .as_ref()
                    .is_some_and(|old| old.host_instance != host.host_instance)
                {
                    runtime.clear_input();
                    runtime.active = None;
                    runtime.screen = None;
                    runtime.sessions.clear();
                    runtime.definitions.clear();
                    runtime.batch = None;
                    runtime.seen.clear();
                    runtime.resized = None;
                }
                runtime.host = Some(host);
                runtime.client_id = Some(client_id);
            }
            match reply.result {
                Err(error) => {
                    if matches!(
                        reply.tag,
                        Tag::Connect | Tag::Input | Tag::Ack | Tag::Snapshot(_) | Tag::Attached
                    ) {
                        runtime.online = false;
                        runtime.clear_input();
                    }
                    // A missing host at initial load is normal; opening is explicit.
                    if reply.tag != Tag::Connect || runtime.host.is_some() {
                        self.error(format!("{error:#}"));
                    }
                }
                Ok(value) => {
                    runtime.online = true;
                    if let Err(error) = self.accept_reply(reply.tag, value) {
                        let runtime = self.runtime.as_mut().unwrap();
                        runtime.online = false;
                        runtime.clear_input();
                        self.error(format!("{error:#}"));
                    }
                }
            }
        }
        if let Some(runtime) = &mut self.runtime {
            if let Err(error) = runtime.poll() {
                runtime.online = false;
                runtime.clear_input();
                self.error(error.to_string());
            }
        }
    }
    fn accept_reply(&mut self, tag: Tag, value: serde_json::Value) -> Result<()> {
        let refresh_definition = matches!(tag, Tag::Definition(_));
        let runtime = self.runtime.as_mut().unwrap();
        match tag {
            Tag::Connect => {
                runtime.submit(Some(Request::List { project: None }), Tag::List)?;
            }
            Tag::List => {
                runtime.sessions = serde_json::from_value(value)?;
                if let Some(active) = &mut runtime.active {
                    if let Some(fresh) = runtime
                        .sessions
                        .iter()
                        .find(|item| item.session_id == active.session_id)
                    {
                        *active = fresh.clone();
                    }
                }
            }
            Tag::Snapshot(id) => {
                let reply: SnapshotReply = serde_json::from_value(value)?;
                if runtime.active.as_ref().is_some_and(|active| {
                    active.session_id == id && active.host_instance == reply.session.host_instance
                }) {
                    runtime.active = Some(reply.session);
                    if let Some(screen) = reply.screen {
                        super::screen::validate(&screen)?;
                        runtime.screen = Some(screen);
                    }
                    if !self.menu_visible {
                        let active = runtime.active.as_ref().unwrap();
                        runtime.seen.insert(id, active.generation);
                    }
                }
            }
            Tag::Started => {
                let session: SessionInfo = serde_json::from_value(value)?;
                self.info(format!(
                    "Shell {:?}; attaching. Initialization may still be in progress.",
                    session.state
                ));
                self.attach_session(&session.session_id, false)?;
            }
            Tag::Attached => {
                let session: SessionInfo = serde_json::from_value(value)?;
                let id = session.session_id.clone();
                runtime.active = Some(session);
                runtime.screen = None;
                runtime.resized = None;
                runtime.submit(
                    Some(Request::Snapshot {
                        session: id.clone(),
                        since: None,
                    }),
                    Tag::Snapshot(id.clone()),
                )?;
                runtime.submit(
                    Some(Request::Definition {
                        session: id.clone(),
                    }),
                    Tag::Definition(id),
                )?;
                self.set_terminal_connected(true);
            }
            Tag::Definition(id) => {
                let terminal: TerminalDefinition = serde_json::from_value(value)?;
                runtime.definitions.insert(id.clone(), terminal.clone());
                if let Some(active) = runtime
                    .active
                    .as_ref()
                    .filter(|active| active.session_id == id)
                {
                    if let Some(project) = self
                        .view
                        .projects
                        .iter()
                        .find(|project| project.project.id == active.project_id)
                    {
                        if !project
                            .project
                            .terminals
                            .iter()
                            .any(|item| item.id == terminal.id)
                        {
                            let mut terminal = terminal;
                            terminal.persistent = false;
                            let temporary =
                                self.temporary.entry(active.project_id.clone()).or_default();
                            if !temporary.iter().any(|item| item.id == terminal.id) {
                                temporary.push(terminal);
                            }
                        }
                    }
                }
            }
            Tag::Batch => {
                let batch: BatchInfo = serde_json::from_value(value)?;
                let changed = runtime.batch.as_ref().is_none_or(|old| {
                    old.state != batch.state || old.waiting_session != batch.waiting_session
                });
                let message=format!("Default launch {:?}: {} started, {} remaining, {} failed. F8: continue unknown initialization · O: cancel remaining.",batch.state,batch.sessions.len(),batch.remaining.len(),batch.failures.len());
                runtime.batch = Some(batch);
                if changed {
                    self.info(message);
                }
            }
            Tag::Preview => {
                let preview: ClosePreview = serde_json::from_value(value)?;
                let ids = preview
                    .targets
                    .iter()
                    .map(|session| session.session_id.clone())
                    .collect();
                let title = if preview.project.is_some() {
                    "Close all project shells"
                } else {
                    "Close all shells and stop host"
                };
                let request = match preview.project {
                    Some(project) => Request::CloseProject {
                        project,
                        sessions: ids,
                        force: false,
                    },
                    None => Request::Shutdown {
                        sessions: ids,
                        force: false,
                    },
                };
                let mut lines = vec![
                    "Only these exact sessions are targeted. Other input owners are overridden."
                        .into(),
                    "F3 toggles forced termination; F2 confirms; Esc cancels.".into(),
                    "Detached programs are not adopted or claimed stopped.".into(),
                ];
                lines.extend(preview.targets.iter().map(|session| {
                    format!(
                        "{} · {} · {:?} · owner {}",
                        session.name,
                        session.session_id,
                        session.state,
                        session
                            .owner
                            .as_ref()
                            .map(|owner| owner.client_id.as_str())
                            .unwrap_or("none")
                    )
                }));
                self.dialog = Some(Dialog::Live(LiveDialog::Confirm {
                    title: title.into(),
                    lines,
                    request: Box::new(request),
                    tag: Tag::Close,
                    force: false,
                    scroll: 0,
                }));
            }
            Tag::Close => {
                if let Ok(reply) = serde_json::from_value::<CloseReply>(value.clone()) {
                    self.info(format!("Close requested: {} targets; {} cleanup pending; {} errors. Host shutdown: {}. {}",reply.sessions.len(),reply.pending.len(),reply.errors.len(),reply.shutting_down,reply.errors.values().cloned().collect::<Vec<_>>().join("; ")));
                } else {
                    let session: SessionInfo = serde_json::from_value(value)?;
                    self.info(format!(
                        "Shell {:?}; {}",
                        session.state,
                        session
                            .error
                            .as_deref()
                            .unwrap_or("cleanup status will refresh")
                    ));
                }
            }
            Tag::Search => {
                let reply: SearchReply = serde_json::from_value(value)?;
                self.info(format!(
                    "Search {} ({} rows examined).",
                    if reply.found.is_some() {
                        "matched"
                    } else {
                        "not found"
                    },
                    reply.searched_rows
                ));
            }
            Tag::Ack | Tag::Input => {}
        }
        if refresh_definition {
            self.refresh_live_definition();
        }
        Ok(())
    }
    pub(super) fn refresh_live_definition(&mut self) {
        let Some(active) = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.active.as_ref())
            .cloned()
        else {
            return;
        };
        let terminal = self
            .view
            .projects
            .iter()
            .find(|project| project.project.id == active.project_id)
            .and_then(|project| {
                project
                    .project
                    .terminals
                    .iter()
                    .find(|terminal| terminal.id == active.terminal_id)
            })
            .cloned()
            .or_else(|| {
                self.temporary
                    .get(&active.project_id)
                    .and_then(|items| {
                        items
                            .iter()
                            .find(|terminal| terminal.id == active.terminal_id)
                    })
                    .cloned()
            });
        let notice = match terminal {
            None => Some("Saved definition is absent; this shell remains accessible through l or session attach.".into()),
            Some(terminal) => match self.service.review_terminal(&active.project_id,&terminal,&self.environment) {
                Ok(review) if review.digest.is_some() && review.digest == active.launch_digest => None,
                _ => Some("Launch inputs changed or need review. The live shell retains its original initialization and state.".into()),
            },
        };
        if let Some(runtime) = &mut self.runtime {
            runtime.definition_notice = notice;
        }
    }
    pub(super) fn live_dimensions(&mut self, area: Rect) {
        let width = area.width.min(MAX_TERMINAL_CELLS as u16);
        let height = area
            .height
            .saturating_sub(2)
            .min((MAX_TERMINAL_CELLS / usize::from(width.max(1))) as u16);
        self.viewport = Rect::new(
            area.x,
            area.y.saturating_add(u16::from(area.height > 0)),
            width,
            height,
        );
        if width >= 2 && height > 0 {
            if let Some(runtime) = &mut self.runtime {
                runtime.dimensions = (height, width);
            }
        }
    }
    pub(super) fn live_menu_key(&mut self, key: KeyEvent) -> bool {
        if self.runtime.is_none() {
            return false;
        }
        let result = (|| -> Result<bool> {
            match key.code {
                KeyCode::Char('l') => {
                    self.send(Request::List { project: None }, Tag::List)?;
                    self.dialog = Some(Dialog::Live(LiveDialog::Sessions { selected: 0 }));
                }
                KeyCode::F(5) => {
                    self.runtime.as_mut().unwrap().connect()?;
                    return Ok(false);
                }
                KeyCode::Char('o') => {
                    let project = self.current_id().context("Select a project first.")?;
                    let (rows, cols) = self.runtime.as_ref().unwrap().dimensions;
                    self.send(
                        Request::StartDefaults {
                            project,
                            env: self.environment.variables().clone(),
                            rows,
                            cols,
                        },
                        Tag::Batch,
                    )?;
                }
                KeyCode::F(8) | KeyCode::Char('O') => {
                    let batch=self.runtime.as_ref().unwrap().batch.as_ref().context("No default launch batch is known. Press o to recover the project's batch.")?.batch_id.clone();
                    self.send(
                        if key.code == KeyCode::F(8) {
                            Request::ContinueDefaults { batch }
                        } else {
                            Request::CancelDefaults { batch }
                        },
                        Tag::Batch,
                    )?;
                }
                KeyCode::Char('x') => {
                    let session = self
                        .selected_live()
                        .context("This definition has no live shell.")?;
                    ensure!(
                        session
                            .owner
                            .as_ref()
                            .is_some_and(|owner| Some(&owner.client_id)
                                == self.runtime.as_ref().unwrap().client_id.as_ref()),
                        "Attach the selected shell before closing it."
                    );
                    self.dialog=Some(Dialog::Live(LiveDialog::Confirm{title:"Close selected shell".into(),lines:vec![format!("{} · {} · {:?}",session.name,session.session_id,session.state),"This ends the shell and its owned process groups. Detached programs may remain.".into(),"F3 toggles force; F2 confirms; Esc cancels.".into()],request:Box::new(Request::Close{session:session.session_id,epoch:session.input_epoch,force:false}),tag:Tag::Close,force:false,scroll:0}));
                }
                KeyCode::Char('X') => {
                    self.send(
                        Request::PreviewClose {
                            project: Some(self.current_id().context("Select a project first.")?),
                        },
                        Tag::Preview,
                    )?;
                }
                KeyCode::Char('H') => {
                    self.send(Request::PreviewClose { project: None }, Tag::Preview)?;
                }
                KeyCode::Char('/') => {
                    self.active_owner()?;
                    self.dialog = Some(Dialog::Live(LiveDialog::Search {
                        input: TextInput::new(""),
                        backwards: true,
                    }));
                }
                KeyCode::Char('y') => {
                    let text = self
                        .runtime
                        .as_ref()
                        .unwrap()
                        .screen
                        .as_ref()
                        .context("No screen is available to copy.")?
                        .text();
                    self.dialog = Some(Dialog::Live(LiveDialog::Copy { text, scroll: 0 }));
                }
                KeyCode::PageUp | KeyCode::PageDown
                    if key.modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    self.scroll_live(if key.code == KeyCode::PageUp { 10 } else { -10 })?;
                }
                _ => return Ok(false),
            }
            Ok(true)
        })();
        match result {
            Ok(handled) => handled,
            Err(error) => {
                self.error(error.to_string());
                true
            }
        }
    }
    pub(super) fn selected_live(&self) -> Option<SessionInfo> {
        let project = self.current_id()?;
        let terminal = self.current_terminal()?;
        let runtime = self.runtime.as_ref()?;
        let listed = runtime
            .sessions
            .iter()
            .find(|session| session.project_id == project && session.terminal_id == terminal.id);
        if let Some(active) = runtime
            .active
            .as_ref()
            .filter(|session| session.project_id == project && session.terminal_id == terminal.id)
        {
            if listed.is_none_or(|session| session.session_id == active.session_id) {
                return Some(active.clone());
            }
        }
        listed.cloned()
    }
    pub(super) fn open_live_selected(&mut self) -> Result<()> {
        if let Some(session) = self.selected_live() {
            if session.state.is_live() {
                return self.review_attach(session);
            }
            let project = self.current_id().unwrap();
            let terminal = self.current_terminal().unwrap();
            let (rows, cols) = self.runtime.as_ref().unwrap().dimensions;
            let request = if terminal.persistent {
                Request::Start {
                    project,
                    terminal: terminal.id,
                    env: self.environment.variables().clone(),
                    rows,
                    cols,
                    reopen: true,
                }
            } else {
                Request::StartTransient {
                    project,
                    terminal,
                    env: self.environment.variables().clone(),
                    rows,
                    cols,
                    reopen: true,
                }
            };
            self.dialog = Some(Dialog::Live(LiveDialog::Confirm {
                title: "Reopen terminal with a new shell".into(),
                lines: vec![format!("Previous shell: {:?}. Its state cannot be recovered.",session.state),
                    "A new shell will execute the reviewed initialization again. F2 reopens; Esc cancels.".into()],
                request: Box::new(request), tag:Tag::Started, force:false, scroll:0,
            }));
            return Ok(());
        }
        let project = self.current_id().context("Select a project.")?;
        let terminal = self.current_terminal().context("Select a terminal.")?;
        // Host rechecks trust and all launch inputs immediately before spawning.
        self.service
            .terminal_launch_plan(&project, &terminal, self.environment.clone())
            .context("Initialization must be reviewed with t before opening")?;
        let (rows, cols) = self.runtime.as_ref().unwrap().dimensions;
        let request = if terminal.persistent {
            Request::Start {
                project,
                terminal: terminal.id,
                env: self.environment.variables().clone(),
                rows,
                cols,
                reopen: false,
            }
        } else {
            Request::StartTransient {
                project,
                terminal,
                env: self.environment.variables().clone(),
                rows,
                cols,
                reopen: false,
            }
        };
        self.send(request, Tag::Started)
    }
    fn review_attach(&mut self, session: SessionInfo) -> Result<()> {
        if session.owner.as_ref().is_some_and(|owner| {
            Some(&owner.client_id) != self.runtime.as_ref().unwrap().client_id.as_ref()
        }) {
            self.dialog=Some(Dialog::Live(LiveDialog::Confirm{title:"Take over terminal input".into(),lines:vec![format!("{} is attached by another client.",session.name),"F2 transfers input ownership and resize control. The other client becomes read-only.".into()],request:Box::new(Request::Attach{session:session.session_id,takeover:true}),tag:Tag::Attached,force:false,scroll:0}));
            Ok(())
        } else {
            self.attach_session(&session.session_id, false)
        }
    }
    fn active_owner(&self) -> Result<(String, u64)> {
        let runtime = self.runtime.as_ref().context("No runtime")?;
        ensure!(runtime.can_input(),"Terminal is read-only or disconnected. Ctrl+g opens controls; refresh and attach explicitly.");
        let active = runtime.active.as_ref().unwrap();
        Ok((active.session_id.clone(), active.input_epoch))
    }
    fn scroll_live(&mut self, delta: i32) -> Result<()> {
        let (session, epoch) = self.active_owner()?;
        self.send(
            Request::Scroll {
                session,
                epoch,
                delta,
            },
            Tag::Ack,
        )
    }
    fn send_input(&mut self, bytes: Vec<u8>) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let (session, epoch) = self.active_owner()?;
        self.runtime
            .as_mut()
            .unwrap()
            .buffer_input(session, epoch, bytes)
    }
    pub fn forward_terminal(&mut self, outcome: UiOutcome) -> Result<()> {
        let modes = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.screen.as_ref())
            .context("Waiting for a terminal screen; input was not sent.")?
            .modes
            .clone();
        let bytes = match outcome {
            UiOutcome::ForwardTerminalKey(key) => super::input::encode_key(key, &modes)?,
            UiOutcome::ForwardTerminalPaste(text) => super::input::encode_paste(&text, &modes)?,
            _ => return Ok(()),
        };
        self.send_input(bytes)
    }
    pub(super) fn live_event(&mut self, event: &Event) -> Result<()> {
        if !self.terminal_connected || self.menu_visible || self.dialog.is_some() {
            return Ok(());
        }
        let Some(modes) = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.screen.as_ref())
            .map(|screen| screen.modes.clone())
        else {
            return Ok(());
        };
        let bytes = match event {
            Event::FocusGained => super::input::encode_focus(true, &modes),
            Event::FocusLost => super::input::encode_focus(false, &modes),
            Event::Mouse(mouse) => {
                if !modes.mouse_click && !modes.mouse_motion && !modes.mouse_drag {
                    match mouse.kind {
                        MouseEventKind::ScrollUp => self.scroll_live(3)?,
                        MouseEventKind::ScrollDown => self.scroll_live(-3)?,
                        _ => {}
                    }
                    return Ok(());
                }
                super::input::encode_mouse(*mouse, &modes, self.viewport).unwrap_or_default()
            }
            _ => return Ok(()),
        };
        self.send_input(bytes)
    }
    pub(super) fn handle_live_dialog(&mut self, mut dialog: LiveDialog, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            return;
        }
        let result = (|| -> Result<bool> {
            match &mut dialog {
                LiveDialog::Confirm {
                    request,
                    tag,
                    force,
                    scroll,
                    ..
                } => {
                    super::scroll(scroll, key);
                    if key.code == KeyCode::F(3) {
                        match request.as_mut() {
                            Request::Close { force: value, .. }
                            | Request::CloseProject { force: value, .. }
                            | Request::Shutdown { force: value, .. } => {
                                *value = !*value;
                                *force = *value;
                            }
                            _ => {}
                        }
                    }
                    if key.code == KeyCode::F(2) {
                        self.send(request.as_ref().clone(), tag.clone())?;
                        return Ok(true);
                    }
                }
                LiveDialog::Sessions { selected } => {
                    let runtime = self.runtime.as_ref().unwrap();
                    match key.code {
                        KeyCode::Up => *selected = selected.saturating_sub(1),
                        KeyCode::Down => {
                            *selected = selected
                                .saturating_add(1)
                                .min(runtime.sessions.len().saturating_sub(1))
                        }
                        KeyCode::Enter => {
                            let session = runtime
                                .sessions
                                .get(*selected)
                                .context("No host sessions are available.")?
                                .clone();
                            ensure!(
                                session.state.is_live(),
                                "This shell is {:?}; select its definition to explicitly reopen.",
                                session.state
                            );
                            self.review_attach(session)?;
                            return Ok(true);
                        }
                        _ => {}
                    }
                }
                LiveDialog::Search { input, backwards } => {
                    if key.code == KeyCode::Tab {
                        *backwards = !*backwards;
                    } else if key.code == KeyCode::Enter {
                        let (session, epoch) = self.active_owner()?;
                        self.send(
                            Request::Search {
                                session,
                                epoch,
                                query: input.text.clone(),
                                backwards: *backwards,
                            },
                            Tag::Search,
                        )?;
                        return Ok(true);
                    } else {
                        input.key(key);
                    }
                }
                LiveDialog::Copy { text, scroll } => {
                    if key.code == KeyCode::F(2) {
                        self.clipboard_request = Some(text.clone());
                        self.info("Clipboard request prepared. Your outer terminal decides whether to accept it.");
                        return Ok(true);
                    }
                    super::scroll(scroll, key);
                }
            }
            Ok(false)
        })();
        match result {
            Ok(true) => {}
            Ok(false) => self.dialog = Some(Dialog::Live(dialog)),
            Err(error) => {
                self.dialog = Some(Dialog::Live(dialog));
                self.error(error.to_string());
            }
        }
    }
}
