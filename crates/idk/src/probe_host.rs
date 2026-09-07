//! The distributed executable's finite synthetic host/reattach proof. No test
//! harness, compiler, Python interpreter, or network service is involved.
use crate::client::Client;
use crate::model::{new_id, Project, ShellConfig};
use crate::project::{ConnectDraft, LaunchEnvironment, ProjectService, TerminalDraft};
use crate::protocol::{BatchState, SessionInfo, SessionState};
use crate::shell::InitializationState;
use crate::store::{ensure_private_dir, Store};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PREPARE_BUDGET: Duration = Duration::from_secs(10);
const REATTACH_BUDGET: Duration = Duration::from_secs(1);
const IDLE_RSS_BUDGET_KIB: u64 = 256 * 1024;

struct Sandbox(PathBuf);
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
struct OwnedHost {
    child: OwnedChild,
    store: Store,
    launcher: PathBuf,
}

pub(crate) fn probe_child_command(program: &Path) -> Command {
    let mut command = Command::new(program);
    let probe_pid = unsafe { libc::getpid() };
    // SAFETY: the post-fork closure uses only Linux prctl/getppid and creates
    // OS errors from raw codes. The parent-death policy is exclusive to these
    // finite synthetic children; ordinary detached hosts retain their lifetime.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGKILL as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != probe_pid {
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
    command
}
impl Drop for OwnedHost {
    fn drop(&mut self) {
        if self.child.0.try_wait().ok().flatten().is_some() {
            return;
        }
        // On any failed check, close only the host that this probe actually
        // spawned. An endpoint replacement never grants authority over it.
        if let Ok(mut client) = Client::connect_with_launcher(&self.store, &self.launcher) {
            if client.info().pid == self.child.0.id() {
                let _ = client.set_timeout(Duration::from_millis(500));
                if let Ok(preview) = client.preview_close(None) {
                    let targets: Vec<_> = preview
                        .targets
                        .into_iter()
                        .map(|info| info.session_id)
                        .collect();
                    let _ = client.shutdown(&targets, true);
                    let _ = wait_host_exit(&mut self.child.0, Duration::from_secs(3));
                }
            }
        }
        // OwnedChild's destructor handles the still-unready or unresponsive
        // process through its Child handle, never through a saved PID.
    }
}

fn start_host(store: &Store, launcher: &Path) -> Result<(OwnedHost, Client)> {
    let child = probe_child_command(launcher)
        .arg("__host")
        .arg("--config-dir")
        .arg(&store.config_dir)
        .arg("--state-dir")
        .arg(&store.state_dir)
        .arg("--runtime-dir")
        .arg(&store.runtime_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn synthetic same-candidate host")?;
    let mut owned = OwnedHost {
        child: OwnedChild(child),
        store: store.clone(),
        launcher: launcher.into(),
    };
    // Startup includes executable identity checks. The ADR five-shell budget
    // begins after this handshake, as it does for a user opening definitions.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match Client::connect_with_launcher(store, launcher) {
            Ok(client) => {
                ensure!(
                    client.info().pid == owned.child.0.id(),
                    "synthetic endpoint belongs to a different host; preserved"
                );
                return Ok((owned, client));
            }
            Err(error) => {
                ensure!(
                    owned.child.0.try_wait()?.is_none(),
                    "synthetic host exited before its handshake: {error}"
                );
                if Instant::now() >= deadline {
                    return Err(error).context("synthetic host did not become ready");
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn synthetic_project(
    store: &Store,
    root: &Path,
    shell: &Path,
    environment: &LaunchEnvironment,
) -> Result<Project> {
    let project_root = root.join("project");
    ensure_private_dir(&project_root)?;
    let common = project_root.join("common.csh");
    std::fs::write(&common, "set prompt = ''\nset idk_probe_state = initial\nalias idk_probe_alias 'echo IDK_HOST_ALIAS_OK'\necho once >> host-initializations\n")?;
    let mut terminals = Vec::new();
    for index in 0..5 {
        let cwd = if index < 2 {
            project_root.clone()
        } else {
            root.join(format!("external-test-{}", index - 1))
        };
        ensure_private_dir(&cwd)?;
        let source = project_root.join(format!("terminal-{index}.csh"));
        std::fs::write(&source, format!("set idk_probe_slot = slot{index}\n"))?;
        terminals.push(TerminalDraft {
            name: if index < 2 {
                format!("Development {}", index + 1)
            } else {
                format!("External test {}", index - 1)
            },
            cwd,
            sources: vec![source.into()],
            persistent: true,
        });
    }
    let service = ProjectService { store };
    let preview = service.preview_connect(ConnectDraft {
        name: "Synthetic packaged host proof".into(),
        root: project_root.clone(),
        shell: ShellConfig {
            executable: shell.into(),
            login: false,
            init_cwd: project_root,
            sources: vec![common.into()],
            trusted_digest: None,
        },
        terminals,
    })?;
    let project = service.create(preview.revision, preview)?;
    let review = service.review_initialization(&project.id, environment)?;
    ensure!(
        review.common.error.is_none() && review.terminals.iter().all(|scope| scope.error.is_none()),
        "synthetic host initialization could not be reviewed"
    );
    service.approve_initialization(review.revision, review)?;
    Ok(store.load()?.project(&project.id)?.clone())
}

fn wait_line(client: &mut Client, session: &str, expected: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let reply = client.snapshot(session, None)?;
        if let Some(screen) = reply.screen {
            ensure!(
                screen.error.is_none(),
                "synthetic host terminal has an I/O error"
            );
            if screen.text().lines().any(|line| line.trim() == expected) {
                return Ok(());
            }
            ensure!(
                !screen.reader_closed,
                "synthetic host terminal exited before {expected}"
            );
        }
        ensure!(
            Instant::now() < deadline,
            "synthetic host terminal did not display {expected}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn initialization_count(project: &Project) -> Result<usize> {
    Ok(
        std::fs::read_to_string(project.shell.init_cwd.join("host-initializations"))?
            .lines()
            .count(),
    )
}
fn ready_five(
    client: &mut Client,
    project: &Project,
    batch: &str,
    start: Instant,
) -> Result<Vec<SessionInfo>> {
    loop {
        let batch = client.batch(batch)?;
        ensure!(
            batch.failures.is_empty(),
            "synthetic default terminal initialization failed"
        );
        ensure!(
            matches!(batch.state, BatchState::Running | BatchState::Complete),
            "synthetic default terminal initialization paused or was cancelled"
        );
        let sessions = client.list(Some(&project.id))?;
        let complete = batch.state == BatchState::Complete
            && sessions.len() == 5
            && sessions.iter().all(|info| {
                info.state == SessionState::Running
                    && info.initialization == Some(InitializationState::Ready)
                    && info.child_pid.is_some()
            });
        ensure!(
            start.elapsed() <= PREPARE_BUDGET,
            "synthetic five-shell preparation took {} ms; ADR budget is 10000 ms",
            start.elapsed().as_millis()
        );
        if complete {
            let mut ordered = Vec::new();
            for terminal in &project.terminals {
                let session = sessions
                    .iter()
                    .find(|info| info.terminal_id == terminal.id)
                    .context("synthetic saved terminal is missing")?;
                ensure!(
                    session.cwd.as_ref() == Some(&terminal.cwd),
                    "synthetic development/external-test cwd differs from its saved definition"
                );
                ordered.push(session.clone());
            }
            return Ok(ordered);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn wait_closed(client: &mut Client, session: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let reply = client.snapshot(session, None)?;
        if reply.session.state == SessionState::Closed
            && reply.session.exit.is_some()
            && reply.screen.is_some_and(|screen| screen.reader_closed)
        {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "synthetic terminal close was not confirmed with final PTY output"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn wait_ready(client: &mut Client, session: &str) -> Result<SessionInfo> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let info = client
            .list(None)?
            .into_iter()
            .find(|info| info.session_id == session)
            .context("reopened synthetic terminal is missing")?;
        if info.state == SessionState::Running
            && info.initialization == Some(InitializationState::Ready)
        {
            return Ok(info);
        }
        ensure!(
            info.state.is_live() && Instant::now() < deadline,
            "reopened synthetic terminal did not initialize"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn wait_host_exit(child: &mut Child, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(status.success(), "synthetic host returned {status}");
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "synthetic host shutdown remained pending"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn live_host_rss_kib(child: &mut Child) -> Result<u64> {
    ensure!(
        child.try_wait()?.is_none(),
        "synthetic host exited before RSS measurement"
    );
    // An unreaped owned Child PID cannot be reused. Missing VmRSS (e.g. an exit
    // racing the read) is an unavailable measurement, never a zero/idle result.
    let status = std::fs::read_to_string(format!("/proc/{}/status", child.id()))
        .context("read live synthetic host RSS")?;
    let line = status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .context("live synthetic host RSS is unavailable")?;
    let mut fields = line.split_whitespace();
    ensure!(fields.next() == Some("VmRSS:"), "invalid RSS field");
    let rss = fields.next().context("missing RSS value")?.parse::<u64>()?;
    ensure!(
        fields.next() == Some("kB") && child.try_wait()?.is_none(),
        "synthetic host RSS unit or lifetime changed"
    );
    Ok(rss)
}

pub(crate) fn run(shell: &Path, launcher: &Path) -> Result<Value> {
    // A deliberately short Linux-local path also works when TMPDIR is too long
    // for sockaddr_un. All generated inputs belong to this synthetic fixture.
    let root = PathBuf::from("/tmp").join(format!("idk-hp-{}", new_id()));
    ensure_private_dir(&root)?;
    let _sandbox = Sandbox(root.clone());
    let home = root.join("home");
    ensure_private_dir(&home)?;
    let store = Store::open(Some(&root.join("data")))?;
    let variables = BTreeMap::from([
        ("HOME".into(), home.to_string_lossy().into_owned()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("TERM".into(), "xterm-256color".into()),
        ("LANG".into(), "C.UTF-8".into()),
    ]);
    let environment = LaunchEnvironment::from_variables(variables.clone())?;
    let project = synthetic_project(&store, &root, shell, &environment)?;
    let (mut host, mut client) = start_host(&store, launcher)?;
    let candidate_build_id = client.info().build_id.clone();
    let preparing = Instant::now();
    let batch = client.start_defaults(&project.id, variables.clone(), 24, 100)?;
    let original = ready_five(&mut client, &project, &batch.batch_id, preparing)?;
    let prepare5_ms = preparing.elapsed().as_millis() as u64;
    ensure!(
        preparing.elapsed() <= PREPARE_BUDGET,
        "synthetic five-shell preparation took {prepare5_ms} ms; ADR budget is 10000 ms"
    );
    ensure!(
        initialization_count(&project)? == 5,
        "five saved terminals did not source exactly once each"
    );
    for (index, session) in original.iter().enumerate() {
        let attached = client.attach(&session.session_id, false)?;
        client.input(&session.session_id, attached.input_epoch, format!("set idk_probe_state = changed{index}\necho IDK_STATE_${{idk_probe_slot}}:${{idk_probe_state}}\nidk_probe_alias\n").as_bytes())?;
        wait_line(
            &mut client,
            &session.session_id,
            &format!("IDK_STATE_slot{index}:changed{index}"),
        )?;
        wait_line(&mut client, &session.session_id, "IDK_HOST_ALIAS_OK")?;
        client.detach(&session.session_id, attached.input_epoch)?;
    }
    drop(client);
    let reattaching = Instant::now();
    let mut client = Client::connect_with_launcher(&store, launcher)?;
    ensure!(
        client.info().pid == host.child.0.id(),
        "reattach reached a replacement host"
    );
    let first = client.attach(&original[0].session_id, false)?;
    let first_screen = client
        .snapshot(&first.session_id, None)?
        .screen
        .context("reattach did not return its first screen")?;
    ensure!(
        first_screen
            .text()
            .lines()
            .any(|line| line.trim() == "IDK_STATE_slot0:changed0"),
        "reattach first screen did not retain prior state"
    );
    let reattachfirstscreen_ms = reattaching.elapsed().as_millis() as u64;
    ensure!(
        reattaching.elapsed() <= REATTACH_BUDGET,
        "synthetic reattach first screen took {reattachfirstscreen_ms} ms; ADR budget is 1000 ms"
    );
    for (index, session) in original.iter().enumerate() {
        let attached = client.attach(&session.session_id, false)?;
        ensure!(
            attached.child_pid == session.child_pid && attached.session_id == session.session_id,
            "reattach replaced an initialized shell"
        );
        client.input(
            &session.session_id,
            attached.input_epoch,
            b"echo IDK_RESUMED_${idk_probe_slot}:${idk_probe_state}\n",
        )?;
        wait_line(
            &mut client,
            &session.session_id,
            &format!("IDK_RESUMED_slot{index}:changed{index}"),
        )?;
    }
    ensure!(
        initialization_count(&project)? == 5,
        "reattach repeated shell initialization"
    );
    // The five synthetic shells have finished their marker commands and are
    // waiting for input. This is an actual RSS sample, not an inferred zero.
    std::thread::sleep(Duration::from_millis(100));
    let idle_host_rss_kib = live_host_rss_kib(&mut host.child.0)?;
    ensure!(
        idle_host_rss_kib <= IDLE_RSS_BUDGET_KIB,
        "synthetic idle five-shell host RSS was {idle_host_rss_kib} KiB; ADR budget is {IDLE_RSS_BUDGET_KIB} KiB"
    );
    let git = crate::probe_git::run(&mut client, &store, &root, shell, &variables)?;
    let runs = crate::probe_run::run(&mut client, &store, &root, shell, launcher, &variables)?;
    let sleep = ["/usr/bin/sleep", "/bin/sleep"]
        .into_iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .context("synthetic preservation check requires coreutils sleep")?;
    let mut unrelated = OwnedChild(
        probe_child_command(sleep)
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    client.close(&first.session_id, first.input_epoch, false)?;
    wait_closed(&mut client, &first.session_id)?;
    ensure!(
        unrelated.0.try_wait()?.is_none(),
        "terminal close terminated an unrelated synthetic process"
    );
    let reopened = client.start(
        &project.id,
        &project.terminals[0].id,
        variables,
        24,
        100,
        true,
    )?;
    ensure!(
        reopened.session_id != first.session_id,
        "explicit reopen reused an ended runtime identity"
    );
    wait_ready(&mut client, &reopened.session_id)?;
    let attached = client.attach(&reopened.session_id, false)?;
    client.input(
        &reopened.session_id,
        attached.input_epoch,
        b"echo IDK_REOPEN_${idk_probe_slot}:${idk_probe_state}\n",
    )?;
    wait_line(
        &mut client,
        &reopened.session_id,
        "IDK_REOPEN_slot0:initial",
    )?;
    ensure!(
        initialization_count(&project)? == 6,
        "explicit reopen did not initialize exactly one new shell"
    );
    let targets: Vec<_> = client
        .preview_close(None)?
        .targets
        .into_iter()
        .map(|info| info.session_id)
        .collect();
    ensure!(
        targets.len() == 5,
        "synthetic host shutdown inventory changed unexpectedly"
    );
    let closing = client.shutdown(&targets, false)?;
    ensure!(
        closing.errors.is_empty() && closing.shutting_down,
        "synthetic host shutdown was not accepted"
    );
    wait_host_exit(&mut host.child.0, Duration::from_secs(10))?;
    ensure!(
        unrelated.0.try_wait()?.is_none(),
        "host shutdown terminated an unrelated synthetic process"
    );
    Ok(json!({
        "scope": "actual same-candidate host with synthetic local/package inputs; not field acceptance",
        "environment": { "os": std::env::consts::OS, "architecture": std::env::consts::ARCH },
        "candidate_build_id": candidate_build_id,
        "git": git,
        "runs": runs,
        "saved_terminals": { "development": 2, "external_test": 3 },
        "checks": { "defaults_five_ready": "PASS", "independent_shell_state": "PASS", "detach_reattach_same_pids": "PASS", "reattach_source_count_unchanged": "PASS", "explicit_close_reopen": "PASS", "unrelated_process_preserved": "PASS", "host_shutdown_observed": "PASS" },
        "measurements": { "prepare5_ms": prepare5_ms, "reattachfirstscreen_ms": reattachfirstscreen_ms, "idlehostRSS_KiB": idle_host_rss_kib },
        "budgets": { "prepare5_ms": 10000, "reattachfirstscreen_ms": 1000, "idlehostRSS_KiB": IDLE_RSS_BUDGET_KIB },
        "budget_result": "PASS"
    }))
}
