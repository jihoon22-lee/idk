//! Project definition workflows; the runtime adapter owns terminal key encoding.
mod draw;
mod forms;
mod git;
mod git_draft;
mod git_draw;
mod git_view;
pub mod input;
mod live;
mod run_draw;
mod run_form;
mod run_log;
mod runs;
mod runtime;
pub mod screen;

use crate::model::{Project, ShellConfig, SourceSpec, TerminalDefinition};
use crate::project::{
    discover_shells, new_terminal, path_availability, ConnectDraft, ConnectPreview,
    InitializationReview, LaunchEnvironment, PathAvailability, ProjectService, ProjectView,
    RootRebindPreview, TerminalDraft, TrustScopeReview, WorkspaceView,
};
use crate::store::Store;
use anyhow::{bail, Context, Result};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use forms::{Field, FieldValue, Form, FormKey, SourceEditor, SourceList, TextInput};
use ratatui::Frame;
use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiOutcome {
    Continue,
    Quit,
    ForwardTerminalKey(KeyEvent),
    ForwardTerminalPaste(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Projects,
    Content,
}

#[derive(Debug, Clone)]
enum SaveAction {
    Connect(ConnectPreview),
    Rename(String),
    Environment(ShellConfig),
    Terminal(TerminalDefinition),
    Reconnect(RootRebindPreview),
    Repository(PathBuf, bool),
    ClearRepository,
    RemoveProject,
    RemoveTerminal(TerminalDefinition),
}

#[derive(Debug, Clone)]
struct Review {
    title: String,
    lines: Vec<String>,
    action: SaveAction,
    project_id: Option<String>,
    revision: u64,
    back: Option<Form>,
    scroll: usize,
}

#[derive(Debug, Clone)]
struct TransientReview {
    project_id: String,
    terminal: TerminalDefinition,
    scope: TrustScopeReview,
    revision: u64,
    scroll: usize,
}

#[derive(Debug, Clone)]
enum Dialog {
    Run(Box<runs::RunDialog>),
    Git(Box<git::GitDialog>),
    Live(live::LiveDialog),
    Form(Form),
    Sources(SourceList),
    Source(SourceEditor),
    Review(Box<Review>),
    Trust {
        review: InitializationReview,
        scroll: usize,
    },
    TransientTrust(TransientReview),
    Help {
        scroll: usize,
    },
}

#[derive(Debug, Clone)]
struct Notice {
    message: String,
    error: bool,
}

/// Real terminal and TestBackend use the same controller and rendering paths.
pub struct App<'a> {
    service: ProjectService<'a>,
    environment: LaunchEnvironment,
    view: WorkspaceView,
    project_index: usize,
    terminal_index: usize,
    repository_index: usize,
    focus: Focus,
    tab: usize,
    dialog: Option<Dialog>,
    drafts: HashMap<FormKey, Form>,
    temporary: HashMap<String, Vec<TerminalDefinition>>,
    temporary_paths: HashMap<String, PathAvailability>,
    terminal_selection: HashMap<String, String>,
    notice: Option<Notice>,
    menu_visible: bool,
    terminal_connected: bool,
    menu_from_terminal: bool,
    runtime: Option<runtime::Runtime>,
    git: git::GitUi,
    runs: runs::RunUi,
    viewport: ratatui::layout::Rect,
    clipboard_request: Option<String>,
}

impl<'a> App<'a> {
    pub fn load(store: &'a Store) -> Result<Self> {
        Self::with_environment(store, LaunchEnvironment::capture()?)
    }

    /// A launch environment is held in memory, never serialized by this UI.
    pub fn with_environment(store: &'a Store, environment: LaunchEnvironment) -> Result<Self> {
        let service = ProjectService { store };
        let view = service.list()?;
        let project_index = view
            .selected_project
            .as_ref()
            .and_then(|id| view.projects.iter().position(|item| &item.project.id == id))
            .unwrap_or(0);
        let mut app = Self {
            service,
            environment,
            focus: if view.projects.is_empty() {
                Focus::Projects
            } else {
                Focus::Content
            },
            view,
            project_index,
            terminal_index: 0,
            repository_index: 0,
            tab: 0,
            dialog: None,
            drafts: HashMap::new(),
            temporary: HashMap::new(),
            temporary_paths: HashMap::new(),
            terminal_selection: HashMap::new(),
            notice: None,
            menu_visible: true,
            terminal_connected: false,
            menu_from_terminal: false,
            runtime: None,
            git: git::GitUi::default(),
            runs: runs::RunUi::default(),
            viewport: ratatui::layout::Rect::default(),
            clipboard_request: None,
        };
        app.restore_terminal_selection();
        Ok(app)
    }

    pub fn render(&mut self, frame: &mut Frame<'_>) {
        self.live_dimensions(frame.area());
        draw::render(self, frame);
    }

    /// A one-shot plain-text clipboard request exists only after Copy preview + F2.
    pub fn take_clipboard_request(&mut self) -> Option<String> {
        self.clipboard_request.take()
    }

    pub fn selected_project_id(&self) -> Option<String> {
        self.current_id()
    }

    pub fn selected_terminal_definition(&self) -> Option<TerminalDefinition> {
        self.current_terminal()
    }

    /// Call only after a verified runtime attach/detach. Forms never call this
    /// with true and never claim that a saved definition is a running terminal.
    pub fn set_terminal_connected(&mut self, connected: bool) {
        self.terminal_connected = connected;
        self.menu_visible = !connected || self.dialog.is_some();
        self.menu_from_terminal = false;
    }

    pub fn handle_event(&mut self, event: Event) -> Result<UiOutcome> {
        if let Err(error) = self.live_event(&event) {
            self.error(error.to_string());
        }
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.handle_key(key),
            Event::Paste(text) => {
                if self.terminal_connected && !self.menu_visible && self.dialog.is_none() {
                    if self.captured_run_input() {
                        self.info(
                            "Captured Run is read-only. Ctrl+g opens controls; k cancels the Run.",
                        );
                        return Ok(UiOutcome::Continue);
                    }
                    return Ok(UiOutcome::ForwardTerminalPaste(text));
                }
                if matches!(self.dialog, Some(Dialog::Run(_))) {
                    if let Err(error) = self.run_paste(&text) {
                        self.error(error.to_string());
                    }
                    return Ok(UiOutcome::Continue);
                }
                if matches!(self.dialog, Some(Dialog::Git(_))) {
                    if let Err(error) = self.git_paste(&text) {
                        self.error(error.to_string());
                    }
                    return Ok(UiOutcome::Continue);
                }
                let result = match self.dialog.as_mut() {
                    Some(Dialog::Live(live::LiveDialog::Search { input, .. })) => {
                        input.insert(&text)
                    }
                    Some(Dialog::Form(form)) => form.paste(&text),
                    Some(Dialog::Source(source)) => match &mut source.fields[source.selected].value
                    {
                        FieldValue::Text(input) => input.insert(&text),
                        _ => Ok(()),
                    },
                    _ => Ok(()),
                };
                if let Err(error) = result {
                    self.error(error);
                }
                Ok(UiOutcome::Continue)
            }
            _ => Ok(UiOutcome::Continue),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Result<UiOutcome> {
        if key.kind == KeyEventKind::Release {
            return Ok(UiOutcome::Continue);
        }
        if self.dialog.is_some() {
            self.handle_dialog(key);
            return Ok(UiOutcome::Continue);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('g') {
            if self.terminal_connected {
                if !self.menu_visible {
                    self.menu_visible = true;
                    self.menu_from_terminal = true;
                } else if self.menu_from_terminal {
                    self.menu_visible = false;
                    self.menu_from_terminal = false;
                    if self.captured_run_input() {
                        return Ok(UiOutcome::Continue);
                    }
                    return Ok(UiOutcome::ForwardTerminalKey(key));
                } else {
                    self.menu_visible = false;
                }
            } else {
                self.info("No terminal is open. Project controls remain available.");
            }
            return Ok(UiOutcome::Continue);
        }
        if self.terminal_connected && !self.menu_visible {
            if self.captured_run_input() {
                self.info("Captured Run is read-only. Ctrl+g opens controls; k cancels the Run.");
                return Ok(UiOutcome::Continue);
            }
            return Ok(UiOutcome::ForwardTerminalKey(key));
        }
        self.menu_from_terminal = false;
        if matches!(
            key.code,
            KeyCode::Char('1' | '2')
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Tab
                | KeyCode::BackTab
        ) {
            self.invalidate_run_navigation();
        }
        if self.run_menu_key(key) {
            return Ok(UiOutcome::Continue);
        }
        if self.git_operation_key(key) || self.git_menu_key(key) {
            return Ok(UiOutcome::Continue);
        }
        if self.live_menu_key(key) {
            return Ok(UiOutcome::Continue);
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            if self.focus == Focus::Content && self.tab == 0 {
                let result = match key.code {
                    KeyCode::Up => self.move_terminal(-1),
                    KeyCode::Down => self.move_terminal(1),
                    _ => Ok(()),
                };
                if let Err(error) = result {
                    self.error(error.to_string());
                }
            }
            return Ok(UiOutcome::Continue);
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(UiOutcome::Quit),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Ok(UiOutcome::Quit)
            }
            KeyCode::F(1) | KeyCode::Char('?') => self.dialog = Some(Dialog::Help { scroll: 0 }),
            KeyCode::F(5) => match self.reload(None) {
                Ok(()) => {
                    self.info("Definitions and path availability refreshed. Drafts are kept.")
                }
                Err(error) => self.error(error.to_string()),
            },
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = if self.focus == Focus::Projects {
                    Focus::Content
                } else {
                    Focus::Projects
                }
            }
            KeyCode::Char('p') => self.focus = Focus::Projects,
            KeyCode::Char('1'..='4') => {
                if let KeyCode::Char(number) = key.code {
                    self.tab = (number as u8 - b'1') as usize;
                }
                self.focus = Focus::Content;
            }
            KeyCode::Left => self.tab = (self.tab + 3) % 4,
            KeyCode::Right => self.tab = (self.tab + 1) % 4,
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-5),
            KeyCode::PageDown => self.move_selection(5),
            KeyCode::Home => self.move_selection(isize::MIN / 2),
            KeyCode::End => self.move_selection(isize::MAX / 2),
            KeyCode::Char('n') => {
                let form = if self.focus == Focus::Projects || self.current_project().is_none() {
                    Some(FormKey::Connect)
                } else if self.tab == 0 {
                    Some(FormKey::Terminal(self.current_id().unwrap(), None))
                } else if self.tab == 1 {
                    Some(FormKey::Repository(self.current_id().unwrap()))
                } else {
                    None
                };
                if let Some(form) = form {
                    self.open_form(form);
                } else {
                    self.info("This area is not available yet. Project and terminal definitions are ready to edit.");
                }
            }
            KeyCode::Char('e') => {
                if let Some(id) = self.current_id() {
                    if self.focus == Focus::Projects {
                        self.open_form(FormKey::Environment(id));
                    } else if self.tab == 0 {
                        if let Some(terminal) = self.current_terminal() {
                            self.open_form(FormKey::Terminal(id, Some(terminal.id)));
                        }
                    } else if self.tab == 1 {
                        self.open_form(FormKey::Repository(id));
                    }
                }
            }
            KeyCode::Char('r') => {
                if let Some(id) = self.current_id() {
                    self.open_form(FormKey::Rename(id));
                }
            }
            KeyCode::Char('m') => {
                if let Some(id) = self.current_id() {
                    self.open_form(FormKey::Reconnect(id));
                }
            }
            KeyCode::Char('g') => {
                if let Some(id) = self.current_id() {
                    self.open_form(FormKey::Repository(id));
                }
            }
            KeyCode::Char('t') => self.open_trust(),
            KeyCode::Delete => self.review_remove(),
            KeyCode::Enter => self.open_selected(),
            KeyCode::Char('d') if self.focus == Focus::Content && self.tab == 0 => {
                self.make_default()
            }
            KeyCode::Char('s') if self.focus == Focus::Content && self.tab == 0 => {
                self.save_temporary()
            }
            KeyCode::Char('c') if self.focus == Focus::Content && self.tab == 0 => {
                self.copy_terminal()
            }
            _ => {}
        }
        Ok(UiOutcome::Continue)
    }

    fn current_project(&self) -> Option<&ProjectView> {
        self.view.projects.get(self.project_index)
    }
    fn current_id(&self) -> Option<String> {
        self.current_project().map(|view| view.project.id.clone())
    }
    fn project(&self, id: &str) -> Result<&Project> {
        self.view
            .projects
            .iter()
            .find(|view| view.project.id == id)
            .map(|view| &view.project)
            .context("Project is no longer in the current view; refresh the list.")
    }
    fn terminals(&self) -> Vec<TerminalDefinition> {
        let Some(project) = self.current_project() else {
            return Vec::new();
        };
        let mut terminals = project.project.terminals.clone();
        if let Some(temporary) = self.temporary.get(&project.project.id) {
            terminals.extend(temporary.iter().cloned());
        }
        terminals
    }
    fn current_terminal(&self) -> Option<TerminalDefinition> {
        self.terminals().get(self.terminal_index).cloned()
    }
    fn repositories(&self) -> Vec<PathBuf> {
        let Some(view) = self.current_project() else {
            return Vec::new();
        };
        let mut roots: Vec<PathBuf> = view.project.repository.iter().cloned().collect();
        for repository in &view.project.related_repositories {
            if !roots.contains(&repository.root) {
                roots.push(repository.root.clone());
            }
        }
        roots
    }
    fn terminal_path(&self, terminal: &TerminalDefinition) -> PathAvailability {
        self.current_project()
            .and_then(|project| {
                project
                    .terminals
                    .iter()
                    .find(|view| view.definition.id == terminal.id)
            })
            .map(|view| view.path.clone())
            .or_else(|| self.temporary_paths.get(&terminal.id).cloned())
            .unwrap_or_else(|| PathAvailability::Unavailable {
                message: "Availability has not been checked; use F5 to refresh.".into(),
            })
    }
    fn move_selection(&mut self, offset: isize) {
        fn moved(index: usize, count: usize, offset: isize) -> usize {
            if count == 0 {
                0
            } else {
                index.saturating_add_signed(offset).min(count - 1)
            }
        }
        if self.focus == Focus::Projects {
            self.project_index = moved(self.project_index, self.view.projects.len(), offset);
            self.restore_terminal_selection();
            self.repository_index = 0;
        } else if self.tab == 0 {
            self.terminal_index = moved(self.terminal_index, self.terminals().len(), offset);
            self.remember_terminal();
        } else if self.tab == 1 {
            self.repository_index = moved(self.repository_index, self.repositories().len(), offset);
        }
    }
    fn remember_terminal(&mut self) {
        if let (Some(project), Some(terminal)) = (self.current_id(), self.current_terminal()) {
            self.terminal_selection.insert(project, terminal.id);
        }
    }
    fn restore_terminal_selection(&mut self) {
        let preferred = self.current_project().and_then(|view| {
            self.terminal_selection
                .get(&view.project.id)
                .cloned()
                .or_else(|| view.project.default_terminal.clone())
        });
        self.terminal_index = preferred
            .and_then(|id| {
                self.terminals()
                    .iter()
                    .position(|terminal| terminal.id == id)
            })
            .unwrap_or(0);
    }
    fn reload(&mut self, preferred: Option<&str>) -> Result<()> {
        let selected = preferred.map(str::to_owned).or_else(|| self.current_id());
        self.view = self.service.list()?;
        // Rendering never performs filesystem I/O, including for external
        // temporary folders. Availability is refreshed at explicit boundaries.
        self.temporary_paths = self
            .temporary
            .values()
            .flatten()
            .map(|terminal| (terminal.id.clone(), path_availability(&terminal.cwd)))
            .collect();
        self.project_index = selected
            .and_then(|id| {
                self.view
                    .projects
                    .iter()
                    .position(|view| view.project.id == id)
            })
            .unwrap_or(0);
        self.restore_terminal_selection();
        self.repository_index = self
            .repository_index
            .min(self.repositories().len().saturating_sub(1));
        self.refresh_live_definition();
        Ok(())
    }
    fn info(&mut self, message: impl Into<String>) {
        self.notice = Some(Notice {
            message: message.into(),
            error: false,
        });
    }
    fn error(&mut self, message: impl Into<String>) {
        self.notice = Some(Notice {
            message: message.into(),
            error: true,
        });
    }

    fn new_form(&self, key: FormKey) -> Result<Form> {
        let mut sources = Vec::new();
        let (title, fields) = match &key {
            FormKey::Connect => {
                let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
                let shell = discover_shells(&self.environment).first().map(|candidate| candidate.path.to_string_lossy().into_owned()).unwrap_or_default();
                ("Connect project", vec![
                    Field::text("Project name", "", "A label for this project; the folder name does not identify it."),
                    Field::text("Project folder", &cwd, "Choose an existing folder. Git is detected without changing the repository."),
                    Field::text("csh / tcsh executable", shell, "Absolute path to the shell you already use."),
                    Field::toggle("Login startup", false, "Use the selected shell's login startup order."),
                    Field::text("Initialization folder", &cwd, "Scripts run here before the terminal changes to its starting folder."),
                    Field::sources(),
                    Field::text("First terminal name", "Development", "Add more terminals later, including several with the same folder."),
                    Field::text("First terminal folder", &cwd, "A starting folder, not the live working directory."),
                ])
            }
            FormKey::Rename(id) => ("Rename project", vec![Field::text("Project name", &self.project(id)?.name, "Only the saved label changes.")]),
            FormKey::Environment(id) => {
                let shell = &self.project(id)?.shell;
                sources = shell.sources.clone();
                ("Project environment", vec![
                    Field::text("csh / tcsh executable", shell.executable.to_string_lossy(), "Absolute path to the existing shell executable."),
                    Field::toggle("Login startup", shell.login, "Startup files and order are reviewed before initialization is approved."),
                    Field::text("Initialization folder", shell.init_cwd.to_string_lossy(), "A new shell initializes here; existing shells keep their state."),
                    Field::sources(),
                ])
            }
            FormKey::Reconnect(id) => ("Reconnect moved project", vec![Field::text("New project folder", self.project(id)?.root.to_string_lossy(), "Review every changed definition path before reconnecting.")]),
            FormKey::Repository(id) => ("Project Git repository", vec![
                Field::text("Repository folder", self.project(id)?.repository.as_ref().map(|path| path.to_string_lossy().into_owned()).unwrap_or_default(), "Existing repository/worktree. Empty disconnects the primary reference."),
                Field::toggle("Use as primary repository", true, "Off registers a related repository without changing the current Git target."),
            ]),
            FormKey::Terminal(project_id, terminal_id) => {
                let project = self.project(project_id)?;
                let existing = terminal_id.as_ref().and_then(|id|
                    project.terminals.iter().find(|terminal| &terminal.id == id)
                        .or_else(|| self.temporary.get(project_id).and_then(|list| list.iter().find(|terminal| &terminal.id == id))));
                if terminal_id.is_some() && existing.is_none() { bail!("Terminal definition was removed; refresh the project."); }
                sources = existing.map(|terminal| terminal.sources.clone()).unwrap_or_default();
                (if existing.is_some() { "Edit terminal" } else { "Add terminal" }, vec![
                    Field::text("Terminal name", existing.map_or("", |terminal| terminal.name.as_str()), "Same folders can have independent named terminals."),
                    Field::text("Starting folder", existing.map(|terminal| &terminal.cwd).unwrap_or(&project.root).to_string_lossy(), "Project membership and the Git target stay the same after cd."),
                    Field::toggle("Save for next time", existing.is_none_or(|terminal| terminal.persistent), "Off keeps this definition in the current app; it can be saved later."),
                    Field::sources(),
                ])
            }
        };
        Ok(Form {
            key,
            title: title.into(),
            revision: self.view.revision,
            fields,
            selected: 0,
            sources,
        })
    }
    fn open_form(&mut self, key: FormKey) {
        match self
            .drafts
            .remove(&key)
            .map(Ok)
            .unwrap_or_else(|| self.new_form(key))
        {
            Ok(form) => self.dialog = Some(Dialog::Form(form)),
            Err(error) => self.error(error.to_string()),
        }
    }
    fn handle_dialog(&mut self, key: KeyEvent) {
        let dialog = self.dialog.take().unwrap();
        match dialog {
            Dialog::Run(dialog) => {
                if let Err(error) = self.handle_run_dialog(*dialog, key) {
                    self.error(error.to_string());
                }
            }
            Dialog::Git(dialog) => self.handle_git_dialog(*dialog, key),
            Dialog::Live(dialog) => self.handle_live_dialog(dialog, key),
            Dialog::Form(mut form) => {
                if key.code == KeyCode::Esc {
                    self.drafts.insert(form.key.clone(), form);
                    self.info(
                        "Draft kept. Open the same form to continue; no definition was saved.",
                    );
                } else if key.code == KeyCode::F(4) {
                    let form_key = form.key.clone();
                    match self.reload(None).and_then(|()| self.new_form(form_key)) {
                        Ok(fresh) => {
                            self.dialog = Some(Dialog::Form(fresh));
                            self.info("Form reset to the saved definition.");
                        }
                        Err(error) => {
                            self.dialog = Some(Dialog::Form(form));
                            self.error(error.to_string());
                        }
                    }
                } else if key.code == KeyCode::F(2) {
                    match self.prepare_review(&form) {
                        Ok(review) => self.dialog = Some(Dialog::Review(Box::new(review))),
                        Err(error) => {
                            self.dialog = Some(Dialog::Form(form));
                            self.error(error.to_string());
                        }
                    }
                } else if key.code == KeyCode::Enter && form.selected_is_sources() {
                    self.dialog = Some(Dialog::Sources(SourceList {
                        sources: form.sources.clone(),
                        parent: form,
                        selected: 0,
                    }));
                } else {
                    form.edit(key);
                    self.dialog = Some(Dialog::Form(form));
                }
            }
            Dialog::Sources(mut list) => match key.code {
                KeyCode::Esc => self.dialog = Some(Dialog::Form(list.parent)),
                KeyCode::F(2) => {
                    list.parent.sources = list.sources;
                    self.dialog = Some(Dialog::Form(list.parent));
                }
                KeyCode::Char('a') if list.sources.len() < 32 => {
                    self.dialog = Some(Dialog::Source(SourceEditor::new(list, None)))
                }
                KeyCode::Enter | KeyCode::Char('e') if !list.sources.is_empty() => {
                    let index = list.selected;
                    self.dialog = Some(Dialog::Source(SourceEditor::new(list, Some(index))));
                }
                _ => {
                    match key.code {
                        KeyCode::Up => list.selected = list.selected.saturating_sub(1),
                        KeyCode::Down => {
                            list.selected =
                                (list.selected + 1).min(list.sources.len().saturating_sub(1))
                        }
                        KeyCode::Delete if !list.sources.is_empty() => {
                            list.sources.remove(list.selected);
                            list.selected = list.selected.min(list.sources.len().saturating_sub(1));
                        }
                        KeyCode::Char('u') if list.selected > 0 => {
                            list.sources.swap(list.selected, list.selected - 1);
                            list.selected -= 1;
                        }
                        KeyCode::Char('d') if list.selected + 1 < list.sources.len() => {
                            list.sources.swap(list.selected, list.selected + 1);
                            list.selected += 1;
                        }
                        _ => {}
                    }
                    self.dialog = Some(Dialog::Sources(list));
                }
            },
            Dialog::Source(mut source) => {
                if key.code == KeyCode::Esc {
                    self.dialog = Some(Dialog::Sources(source.parent));
                } else if key.code == KeyCode::F(2) {
                    let value = source.value();
                    if let Err(error) = crate::model::absolute_path(&value.path) {
                        self.dialog = Some(Dialog::Source(source));
                        self.error(error.to_string());
                    } else {
                        let mut parent = source.parent;
                        if let Some(index) = source.index {
                            parent.sources[index] = value;
                        } else {
                            parent.sources.push(value);
                            parent.selected = parent.sources.len() - 1;
                        }
                        self.dialog = Some(Dialog::Sources(parent));
                    }
                } else {
                    source.edit(key);
                    self.dialog = Some(Dialog::Source(source));
                }
            }
            Dialog::Review(mut review) => {
                if key.code == KeyCode::Esc {
                    self.dialog = review.back.map(Dialog::Form);
                } else if key.code == KeyCode::Char('d') {
                    if let SaveAction::Connect(preview) = &mut review.action {
                        if !preview.duplicates.is_empty() {
                            preview.allow_duplicate_root = !preview.allow_duplicate_root;
                        }
                    }
                    self.dialog = Some(Dialog::Review(review));
                } else if key.code == KeyCode::F(2) {
                    match self.save_review(&review) {
                        Ok(()) => {
                            if let Some(form) = review.back {
                                self.drafts.remove(&form.key);
                            }
                        }
                        Err(error) => {
                            self.dialog = Some(Dialog::Review(review));
                            self.error(error.to_string());
                        }
                    }
                } else {
                    scroll(&mut review.scroll, key);
                    self.dialog = Some(Dialog::Review(review));
                }
            }
            Dialog::Trust {
                review,
                scroll: mut offset,
            } => {
                if key.code == KeyCode::F(2) {
                    let id = review.project_id.clone();
                    match self
                        .service
                        .approve_initialization(review.revision, review.clone())
                    {
                        Ok(()) => {
                            if let Err(error) = self.reload(Some(&id)) {
                                self.error(format!("Approval saved; refresh failed: {error}"));
                            } else {
                                self.info("Readable initialization groups approved. No shell or task was started.");
                            }
                        }
                        Err(error) => {
                            self.dialog = Some(Dialog::Trust {
                                review,
                                scroll: offset,
                            });
                            self.error(error.to_string());
                        }
                    }
                } else if key.code != KeyCode::Esc {
                    scroll(&mut offset, key);
                    self.dialog = Some(Dialog::Trust {
                        review,
                        scroll: offset,
                    });
                }
            }
            Dialog::TransientTrust(mut review) => {
                if key.code == KeyCode::F(2) {
                    match self.service.approve_transient(
                        review.revision,
                        &review.project_id,
                        &mut review.terminal,
                        &review.scope,
                        &self.environment,
                    ) {
                        Ok(()) => {
                            if let Some(terminals) = self.temporary.get_mut(&review.project_id) {
                                if let Some(terminal) = terminals
                                    .iter_mut()
                                    .find(|terminal| terminal.id == review.terminal.id)
                                {
                                    *terminal = review.terminal;
                                }
                            }
                            self.info("Temporary initialization reviewed in memory. No shell was started.");
                        }
                        Err(error) => {
                            self.dialog = Some(Dialog::TransientTrust(review));
                            self.error(error.to_string());
                        }
                    }
                } else if key.code != KeyCode::Esc {
                    scroll(&mut review.scroll, key);
                    self.dialog = Some(Dialog::TransientTrust(review));
                }
            }
            Dialog::Help { scroll: mut offset } => {
                if !matches!(key.code, KeyCode::Esc | KeyCode::F(1) | KeyCode::Char('?')) {
                    scroll(&mut offset, key);
                    self.dialog = Some(Dialog::Help { scroll: offset });
                }
            }
        }
    }

    fn prepare_review(&self, form: &Form) -> Result<Review> {
        let mut lines = Vec::new();
        let mut project_id = None;
        let action = match &form.key {
            FormKey::Connect => {
                let preview = self.service.preview_connect(ConnectDraft {
                    name: form.text(0).into(),
                    root: form.text(1).into(),
                    shell: ShellConfig {
                        executable: form.text(2).into(),
                        login: form.toggle(3),
                        init_cwd: form.text(4).into(),
                        sources: form.sources.clone(),
                        trusted_digest: None,
                    },
                    terminals: vec![TerminalDraft {
                        name: form.text(6).into(),
                        cwd: form.text(7).into(),
                        sources: Vec::new(),
                        persistent: true,
                    }],
                })?;
                lines.extend(project_summary(&preview.project));
                if let Some(notice) = &preview.repository_notice {
                    lines.push(notice.clone());
                }
                lines.push("Only project definitions are saved. Existing folders and scripts are preserved.".into());
                lines.push("Review initialization separately before opening new shells.".into());
                if !preview.duplicates.is_empty() {
                    lines.push("This folder is already connected:".into());
                    for duplicate in &preview.duplicates {
                        lines.push(format!(
                            "  {} · {}",
                            duplicate.name,
                            duplicate.root.display()
                        ));
                    }
                }
                SaveAction::Connect(preview)
            }
            FormKey::Rename(id) => {
                project_id = Some(id.clone());
                crate::model::valid_name(form.text(0))?;
                lines.push(format!("{} → {}", self.project(id)?.name, form.text(0)));
                SaveAction::Rename(form.text(0).into())
            }
            FormKey::Environment(id) => {
                project_id = Some(id.clone());
                let shell = ShellConfig {
                    executable: form.text(0).into(),
                    login: form.toggle(1),
                    init_cwd: form.text(2).into(),
                    sources: form.sources.clone(),
                    trusted_digest: None,
                };
                let mut proposed = self.project(id)?.clone();
                proposed.shell = shell.clone();
                proposed.validate()?;
                lines.extend(shell_summary(&shell));
                lines.push("Changes apply to new shells. Existing shells keep their state and working folder.".into());
                lines.push(
                    "Review changed initialization scripts before opening a new shell.".into(),
                );
                SaveAction::Environment(shell)
            }
            FormKey::Reconnect(id) => {
                project_id = Some(id.clone());
                let preview = self.service.preview_rebind_root(id, form.text(0).into())?;
                lines.push(format!(
                    "{} → {}",
                    preview.old_root.display(),
                    preview.new_root.display()
                ));
                for change in &preview.changes {
                    lines.push(format!(
                        "{}: {} → {}",
                        change.field,
                        change.old.display(),
                        change.new.display()
                    ));
                }
                if let Some(notice) = &preview.repository_notice {
                    lines.push(notice.clone());
                }
                lines.push("This reconnects definitions; it does not move files or change a live shell's cwd.".into());
                SaveAction::Reconnect(preview)
            }
            FormKey::Repository(id) => {
                project_id = Some(id.clone());
                if form.text(0).is_empty() {
                    if !form.toggle(1) {
                        bail!("Enter a folder to add a related repository.");
                    }
                    lines.push("Disconnect the primary Git reference; source files and related repositories remain.".into());
                    SaveAction::ClearRepository
                } else {
                    crate::model::absolute_path(&PathBuf::from(form.text(0)))?;
                    lines.push(format!(
                        "{} repository: {}",
                        if form.toggle(1) { "Primary" } else { "Related" },
                        form.text(0)
                    ));
                    lines.push("Git identity is rechecked when saving. No init, checkout or network action runs.".into());
                    SaveAction::Repository(form.text(0).into(), form.toggle(1))
                }
            }
            FormKey::Terminal(id, terminal_id) => {
                project_id = Some(id.clone());
                let project = self.project(id)?;
                let mut terminal = new_terminal(
                    project,
                    TerminalDraft {
                        name: form.text(0).into(),
                        cwd: form.text(1).into(),
                        persistent: form.toggle(2),
                        sources: form.sources.clone(),
                    },
                )?;
                if let Some(id) = terminal_id {
                    terminal.id = id.clone();
                }
                lines.push(format!("Name: {}", terminal.name));
                lines.push(format!(
                    "Starting folder: {} ({})",
                    terminal.cwd.display(),
                    path_label(&path_availability(&terminal.cwd))
                ));
                lines.push(format!(
                    "Initialization folder: {}",
                    project.shell.init_cwd.display()
                ));
                lines.push(
                    if terminal.persistent {
                        "Saved for next time."
                    } else {
                        "Temporary: kept only while this app is open; save it later to keep it."
                    }
                    .into(),
                );
                lines.extend(source_summary(&terminal.sources));
                lines.push("Definition changes do not start, stop or cd an existing shell.".into());
                SaveAction::Terminal(terminal)
            }
        };
        Ok(Review {
            title: format!("Review · {}", form.title),
            lines,
            action,
            project_id,
            revision: form.revision,
            back: Some(form.clone()),
            scroll: 0,
        })
    }

    fn save_review(&mut self, review: &Review) -> Result<()> {
        let id = review.project_id.as_deref().unwrap_or("");
        let selected = match &review.action {
            SaveAction::Connect(preview) => {
                let project = self.service.create(preview.revision, preview.clone())?;
                self.focus = Focus::Content;
                Some(project.id)
            }
            SaveAction::Rename(name) => {
                self.service.rename(review.revision, id, name.clone())?;
                Some(id.to_owned())
            }
            SaveAction::Environment(shell) => {
                self.service
                    .update_shell(review.revision, id, shell.clone())?;
                Some(id.to_owned())
            }
            SaveAction::Terminal(terminal) => {
                self.service
                    .save_terminal(review.revision, id, terminal.clone())?;
                let temporary = self.temporary.entry(id.to_owned()).or_default();
                temporary.retain(|item| item.id != terminal.id);
                if !terminal.persistent {
                    temporary.push(terminal.clone());
                }
                self.terminal_selection
                    .insert(id.to_owned(), terminal.id.clone());
                Some(id.to_owned())
            }
            SaveAction::Reconnect(preview) => {
                self.service
                    .rebind_root(preview.revision, preview.clone())?;
                Some(id.to_owned())
            }
            SaveAction::Repository(root, primary) => {
                self.service
                    .bind_repository(review.revision, id, root.clone(), *primary)?;
                Some(id.to_owned())
            }
            SaveAction::ClearRepository => {
                self.service.clear_primary_repository(review.revision, id)?;
                Some(id.to_owned())
            }
            SaveAction::RemoveProject => {
                self.service.remove_definition(review.revision, id)?;
                self.temporary.remove(id);
                self.terminal_selection.remove(id);
                self.focus = Focus::Projects;
                None
            }
            SaveAction::RemoveTerminal(terminal) => {
                if terminal.persistent {
                    self.service
                        .remove_terminal_definition(review.revision, id, &terminal.id)?;
                }
                if let Some(temporary) = self.temporary.get_mut(id) {
                    temporary.retain(|item| item.id != terminal.id);
                }
                Some(id.to_owned())
            }
        };
        match self.reload(selected.as_deref()) {
            Ok(()) => self.info("Definition saved. No shell or task was started."),
            Err(error) => self.error(format!(
                "Definition saved; refresh failed: {error}. Use F5 before another change."
            )),
        }
        Ok(())
    }

    fn open_selected(&mut self) {
        let Some(id) = self.current_id() else {
            self.open_form(FormKey::Connect);
            return;
        };
        if self.focus == Focus::Projects {
            match self
                .service
                .select(self.view.revision, &id)
                .and_then(|()| self.reload(Some(&id)))
            {
                Ok(()) => {
                    self.focus = Focus::Content;
                    self.info("Project selected. Choose or define a terminal.");
                }
                Err(error) => self.error(error.to_string()),
            }
        } else if self.tab == 0 {
            if self.current_terminal().is_some() && self.runtime.is_some() {
                if let Err(error) = self.open_live_selected() {
                    self.error(error.to_string());
                }
            } else if self.current_terminal().is_some() {
                self.info("Terminal opening is not available in this build. The definition is ready; nothing was started.");
            } else {
                self.open_form(FormKey::Terminal(id, None));
            }
        } else if self.tab == 1 {
            let root = self.repositories().get(self.repository_index).cloned();
            self.open_form(FormKey::Repository(id));
            if let (Some(root), Some(Dialog::Form(form))) = (root, self.dialog.as_mut()) {
                form.fields[0].value = FieldValue::Text(TextInput::new(root.to_string_lossy()));
            }
        } else {
            self.info("This area is not available in this build. Nothing was started.");
        }
    }
    fn review_remove(&mut self) {
        let Some(project) = self.current_project() else {
            return;
        };
        let (title, action, lines) = if self.focus == Focus::Projects {
            (
                "Remove project connection",
                SaveAction::RemoveProject,
                vec![
                    format!("Remove the saved connection for {}?", project.project.name),
                    format!("Folder: {}", project.project.root.display()),
                    "Only personal definitions are removed. Source, .csh and Git files remain."
                        .into(),
                ],
            )
        } else if self.tab == 0 {
            let Some(terminal) = self.current_terminal() else {
                return;
            };
            let lines = vec![
                format!("Remove the definition for {}?", terminal.name),
                format!("Starting folder: {}", terminal.cwd.display()),
                "No files are removed and no process is stopped.".into(),
            ];
            (
                "Remove terminal definition",
                SaveAction::RemoveTerminal(terminal),
                lines,
            )
        } else {
            return;
        };
        self.dialog = Some(Dialog::Review(Box::new(Review {
            title: title.into(),
            action,
            lines,
            project_id: self.current_id(),
            revision: self.view.revision,
            back: None,
            scroll: 0,
        })));
    }
    fn make_default(&mut self) {
        let (Some(project), Some(terminal)) = (self.current_id(), self.current_terminal()) else {
            return;
        };
        if !terminal.persistent {
            self.info("Save this temporary terminal before choosing it as the next-open default.");
            return;
        }
        match self
            .service
            .set_default_terminal(self.view.revision, &project, &terminal.id)
            .and_then(|()| self.reload(Some(&project)))
        {
            Ok(()) => self.info("Default terminal updated."),
            Err(error) => self.error(error.to_string()),
        }
    }
    fn save_temporary(&mut self) {
        let (Some(project), Some(mut terminal)) = (self.current_id(), self.current_terminal())
        else {
            return;
        };
        if terminal.persistent {
            self.info("This terminal is already saved for next time.");
            return;
        }
        terminal.persistent = true;
        self.dialog = Some(Dialog::Review(Box::new(Review {
            title: "Save terminal for next time".into(),
            lines: vec![format!("{} · {}", terminal.name, terminal.cwd.display())],
            action: SaveAction::Terminal(terminal),
            project_id: Some(project),
            revision: self.view.revision,
            back: None,
            scroll: 0,
        })));
    }
    fn copy_terminal(&mut self) {
        let (Some(project), Some(terminal)) = (self.current_id(), self.current_terminal()) else {
            return;
        };
        match self.new_form(FormKey::Terminal(project, None)) {
            Ok(mut form) => {
                form.fields[0].value =
                    FieldValue::Text(TextInput::new(format!("{} copy", terminal.name)));
                form.fields[1].value =
                    FieldValue::Text(TextInput::new(terminal.cwd.to_string_lossy()));
                form.fields[2].value = FieldValue::Toggle(terminal.persistent);
                form.sources = terminal.sources;
                self.dialog = Some(Dialog::Form(form));
            }
            Err(error) => self.error(error.to_string()),
        }
    }
    fn move_terminal(&mut self, direction: isize) -> Result<()> {
        let (Some(project), Some(terminal)) = (self.current_id(), self.current_terminal()) else {
            return Ok(());
        };
        if terminal.persistent {
            let mut order: Vec<String> = self
                .project(&project)?
                .terminals
                .iter()
                .map(|terminal| terminal.id.clone())
                .collect();
            let index = order.iter().position(|id| id == &terminal.id).unwrap();
            let target = index
                .saturating_add_signed(direction)
                .min(order.len().saturating_sub(1));
            if index != target {
                order.swap(index, target);
                self.service
                    .reorder_terminals(self.view.revision, &project, order)?;
            }
        } else if let Some(temporary) = self.temporary.get_mut(&project) {
            let index = temporary
                .iter()
                .position(|item| item.id == terminal.id)
                .unwrap();
            let target = index
                .saturating_add_signed(direction)
                .min(temporary.len().saturating_sub(1));
            temporary.swap(index, target);
        }
        self.terminal_selection.insert(project.clone(), terminal.id);
        self.reload(Some(&project))?;
        self.info("Terminal order updated. Temporary entries remain separate until saved.");
        Ok(())
    }
    fn open_trust(&mut self) {
        let Some(id) = self.current_id() else { return };
        match self.service.review_initialization(&id, &self.environment) {
            Ok(review) => {
                if let Some(terminal) = self
                    .current_terminal()
                    .filter(|terminal| !terminal.persistent)
                {
                    if review.common.trusted {
                        match self
                            .service
                            .review_terminal(&id, &terminal, &self.environment)
                        {
                            Ok(scope) => {
                                self.dialog = Some(Dialog::TransientTrust(TransientReview {
                                    project_id: id,
                                    terminal,
                                    scope,
                                    revision: review.revision,
                                    scroll: 0,
                                }))
                            }
                            Err(error) => self.error(error.to_string()),
                        }
                        return;
                    }
                    self.info("Review common initialization first, then press t for this temporary terminal's scripts.");
                }
                self.dialog = Some(Dialog::Trust { review, scroll: 0 });
            }
            Err(error) => self.error(error.to_string()),
        }
    }
}

fn scroll(offset: &mut usize, key: KeyEvent) {
    match key.code {
        KeyCode::Up => *offset = offset.saturating_sub(1),
        KeyCode::Down => *offset = offset.saturating_add(1),
        KeyCode::PageUp => *offset = offset.saturating_sub(8),
        KeyCode::PageDown => *offset = offset.saturating_add(8),
        KeyCode::Home => *offset = 0,
        _ => {}
    }
}
fn path_label(path: &PathAvailability) -> &'static str {
    match path {
        PathAvailability::Ready { .. } => "available",
        PathAvailability::Missing => "missing",
        PathAvailability::Denied => "access denied",
        PathAvailability::NotDirectory => "not a folder",
        PathAvailability::Unavailable { .. } => "unavailable",
    }
}
fn source_summary(sources: &[SourceSpec]) -> Vec<String> {
    let mut lines = Vec::new();
    if sources.is_empty() {
        lines.push("No additional initialization scripts.".into());
    }
    for (index, source) in sources.iter().enumerate() {
        lines.push(format!("Script {}: {}", index + 1, source.path.display()));
        for (index, argument) in source.args.iter().enumerate() {
            lines.push(format!("  Argument {}: {argument:?}", index + 1));
        }
    }
    lines
}
fn shell_summary(shell: &ShellConfig) -> Vec<String> {
    let mut lines = vec![
        format!("Shell: {}", shell.executable.display()),
        format!("Login startup: {}", if shell.login { "yes" } else { "no" }),
        format!("Initialization folder: {}", shell.init_cwd.display()),
    ];
    lines.extend(source_summary(&shell.sources));
    lines
}
fn project_summary(project: &Project) -> Vec<String> {
    let mut lines = vec![
        format!("Project: {}", project.name),
        format!("Project folder: {}", project.root.display()),
        format!(
            "Primary Git: {}",
            project
                .repository
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "not connected".into())
        ),
    ];
    lines.extend(shell_summary(&project.shell));
    for terminal in &project.terminals {
        lines.push(format!(
            "Terminal: {} · {}",
            terminal.name,
            terminal.cwd.display()
        ));
    }
    lines
}

pub fn run(store: &Store) -> Result<()> {
    run_screen(store, None, None)
}

/// Attach a host session by identity, including shells whose definitions were removed.
pub fn run_attached(store: &Store, session: &str, takeover: bool) -> Result<()> {
    crate::model::valid_id(session)?;
    run_screen(store, Some((session, takeover)), None)
}

/// Attach a recorded Run by identity, including Runs with removed task definitions.
pub fn run_attached_run(store: &Store, run_id: &str, takeover: bool) -> Result<()> {
    crate::model::valid_id(run_id)?;
    run_screen(store, None, Some((run_id, takeover)))
}

fn run_screen(
    store: &Store,
    attached: Option<(&str, bool)>,
    attached_run: Option<(&str, bool)>,
) -> Result<()> {
    use base64::Engine;
    use crossterm::{
        cursor::SetCursorStyle,
        event::{DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture},
    };
    use std::io::Write;
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("The project screen needs an interactive terminal.");
    }
    let mut app = App::load(store)?;
    app.enable_terminal_runtime(std::env::current_exe()?)?;
    if let Some((session, takeover)) = attached {
        app.attach_session(session, takeover)?;
    }
    if let Some((run_id, takeover)) = attached_run {
        app.attach_run(run_id, takeover)?;
    }
    let mut terminal = ratatui::init();
    let outcome = (|| {
        crossterm::execute!(std::io::stdout(), EnableBracketedPaste, EnableFocusChange)?;
        let mut mouse = false;
        let mut cursor = SetCursorStyle::DefaultUserShape;
        loop {
            app.tick();
            let focused = app.terminal_connected && !app.menu_visible && app.dialog.is_none();
            let wanted_mouse = focused && app.terminal_writable();
            if wanted_mouse != mouse {
                if wanted_mouse {
                    crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;
                } else {
                    crossterm::execute!(std::io::stdout(), DisableMouseCapture)?;
                }
                mouse = wanted_mouse;
            }
            let wanted_cursor = if focused && app.terminal_writable() {
                app.terminal_snapshot()
                    .and_then(screen::cursor_style)
                    .unwrap_or(SetCursorStyle::DefaultUserShape)
            } else {
                SetCursorStyle::DefaultUserShape
            };
            if cursor != wanted_cursor {
                crossterm::execute!(std::io::stdout(), wanted_cursor)?;
                cursor = wanted_cursor;
            }
            terminal.draw(|frame| app.render(frame))?;
            if let Some(text) = app.take_clipboard_request() {
                write!(
                    std::io::stdout(),
                    "\x1b]52;c;{}\x07",
                    base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
                )?;
                std::io::stdout().flush()?;
                app.info("Clipboard request sent. Acceptance depends on your terminal; y opens the plain copy preview.");
            }
            if event::poll(Duration::from_millis(30))? {
                match app.handle_event(event::read()?)? {
                    UiOutcome::Quit => break,
                    UiOutcome::Continue => {}
                    action => {
                        if let Err(error) = app.forward_terminal(action) {
                            app.error(error.to_string());
                        }
                    }
                }
            }
        }
        Ok(())
    })();
    let _ = crossterm::execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        DisableFocusChange,
        DisableMouseCapture,
        SetCursorStyle::DefaultUserShape
    );
    ratatui::restore();
    drop(app); // worker detaches only its owned epochs; it never closes the host
    outcome
}
