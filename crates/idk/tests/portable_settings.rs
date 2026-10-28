use idk_workspace::model::{NativeShell, ShellConfig, TaskExecution, Workspace};
use idk_workspace::portable::{
    self, EditorDeclaration, EnvironmentPatch, Preferences, ShellDeclaration,
};
use idk_workspace::project::{LaunchEnvironment, ProjectService};
use idk_workspace::store::{atomic_write, Store};
use idk_workspace::task::TaskService;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

struct Fixture {
    temp: tempfile::TempDir,
    root: PathBuf,
    store: Store,
    env: LaunchEnvironment,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("checkout");
        fs::create_dir(&root).unwrap();
        let bin = temp.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let tool = bin.join("build-tool");
        fs::write(
            &tool,
            format!("#!/bin/sh\ntouch '{}'\n", root.join("executed").display()),
        )
        .unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        let env = LaunchEnvironment::from_variables(BTreeMap::from([
            ("HOME".into(), temp.path().to_string_lossy().into()),
            ("SHELL".into(), "/bin/sh".into()),
            ("PATH".into(), format!("{}:/usr/bin:/bin", bin.display())),
        ]))
        .unwrap();
        let store = Store::open(Some(&temp.path().join("local"))).unwrap();
        Self {
            temp,
            root,
            store,
            env,
        }
    }
    fn write(&self, text: &str) {
        fs::write(self.root.join("idk.toml"), text).unwrap();
    }
    fn sync(&self) -> idk_workspace::model::Project {
        portable::apply(
            &self.store,
            portable::resolve(&self.store, &self.root, &self.env).unwrap(),
        )
        .unwrap()
    }
}

#[test]
fn absent_and_minimal_declarations_resolve_without_execution_or_generated_files() {
    let f = Fixture::new();
    let empty = portable::resolve(&f.store, &f.root, &f.env).unwrap();
    assert!(empty.project.tasks.is_empty());
    assert_eq!(empty.project.name, "checkout");
    assert_eq!(empty.project.terminals[0].cwd, f.root);
    assert!(!f.root.join("idk.toml").exists());
    f.write("schema_version = 1\n[tasks.test]\nprogram = 'build-tool'\nargs = ['test', 'literal;$(false)']\n");
    let project = f.sync();
    let TaskExecution::Process { program, args, .. } = project.tasks[0].execution.as_ref().unwrap()
    else {
        panic!("not a process")
    };
    assert_eq!(program, &f.temp.path().join("bin/build-tool"));
    assert_eq!(args, &["test", "literal;$(false)"]);
    assert_eq!(project.tasks[0].cwd, f.root);
    assert!(!f.root.join("executed").exists());
    let service = ProjectService { store: &f.store };
    assert!(
        service
            .review_initialization(&project.id, &f.env)
            .unwrap()
            .common
            .trusted
    );
    service
        .terminal_launch_plan(&project.id, &project.terminals[0], f.env.clone())
        .unwrap();
    assert_eq!(f.sync().tasks[0].id, project.tasks[0].id);
}

#[test]
fn same_declaration_at_two_checkouts_resolves_paths_locally_and_keeps_separate_ids() {
    let f = Fixture::new();
    let other = f.temp.path().join("other-checkout");
    fs::create_dir(&other).unwrap();
    for root in [&f.root, &other] {
        fs::create_dir(root.join("tools")).unwrap();
        fs::write(root.join("tools/test"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(root.join("tools/test"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(root.join("idk.toml"), "schema_version=1\n[tasks.test]\nprogram='./tools/test'\ncwd='tools'\nartifact='out/result'\n").unwrap();
    }
    let first = f.sync();
    let second = portable::apply(
        &f.store,
        portable::resolve(&f.store, &other, &f.env).unwrap(),
    )
    .unwrap();
    assert_ne!(first.id, second.id);
    assert_ne!(first.tasks[0].id, second.tasks[0].id);
    assert_eq!(first.tasks[0].cwd, f.root.join("tools"));
    assert_eq!(second.tasks[0].cwd, other.join("tools"));
    assert_eq!(second.tasks[0].artifact, Some(other.join("out/result")));
}

#[test]
fn preferences_project_values_and_explicit_empty_lists_have_defined_precedence() {
    let f = Fixture::new();
    let preferences = Preferences {
        schema_version: 1,
        shell: Some(ShellDeclaration {
            program: Some("/bin/sh".into()),
            args: Some(vec!["-i".into()]),
            env: BTreeMap::from([("A".into(), "user-a".into()), ("B".into(), "user-b".into())]),
            ..Default::default()
        }),
        editor: Some(EditorDeclaration {
            program: Some("/bin/true".into()),
            args: Some(vec!["{file}".into()]),
            ..Default::default()
        }),
    };
    portable::save_preferences(&f.store, None, &preferences).unwrap();
    f.write("schema_version=1\n[shell]\nargs=[]\nenv={A='project-a', C='project-c'}\nunset_env=['B']\n[tasks.test]\nprogram='build-tool'\nargs=['unchanged']\n");
    let resolved = portable::resolve(&f.store, &f.root, &f.env).unwrap();
    let native = resolved.project.shell.native.as_ref().unwrap();
    assert!(native.args.is_empty());
    assert_eq!(
        native.env,
        BTreeMap::from([
            ("A".into(), "project-a".into()),
            ("C".into(), "project-c".into())
        ])
    );
    assert_eq!(resolved.origins["shell"], "project declaration");
    assert_eq!(
        resolved.project.editor.as_ref().unwrap().executable,
        Path::new("/bin/true")
    );
    let TaskExecution::Process { args, .. } = resolved.project.tasks[0].execution.as_ref().unwrap()
    else {
        unreachable!()
    };
    assert_eq!(args, &["unchanged"]);
    let project = portable::apply(&f.store, resolved).unwrap();
    let service = ProjectService { store: &f.store };
    assert!(
        !service
            .review_initialization(&project.id, &f.env)
            .unwrap()
            .common
            .trusted
    );
    assert!(service
        .terminal_launch_plan(&project.id, &project.terminals[0], f.env.clone())
        .is_err());
}

#[test]
fn project_startup_requires_review_but_semantic_unchanged_sync_keeps_approval() {
    let f = Fixture::new();
    fs::create_dir(f.root.join("work")).unwrap();
    let text = "schema_version=1\n[shell]\nprogram='/bin/sh'\nargs=['-i']\nenv={MODE='reviewed'}\ncwd='work'\n[tasks.test]\nprogram='build-tool'\n";
    f.write(text);
    let project = f.sync();
    assert_eq!(project.terminals[0].cwd, f.root.join("work"));
    let projects = ProjectService { store: &f.store };
    let review = projects.review_initialization(&project.id, &f.env).unwrap();
    assert!(!review.common.trusted);
    projects
        .approve_initialization(review.revision, review)
        .unwrap();
    let task_service = TaskService { store: &f.store };
    let task_review = task_service
        .review(&project.id, &project.tasks[0].id, &f.env)
        .unwrap();
    task_service
        .approve(
            task_review.revision,
            &project.id,
            &project.tasks[0].id,
            &task_review.digest,
            &f.env,
        )
        .unwrap();
    f.write(&format!("# Comment only\n{text}"));
    assert!(portable::ensure_current(&f.store, &project.id).is_err());
    assert!(projects
        .terminal_launch_plan(&project.id, &project.terminals[0], f.env.clone())
        .is_err());
    let synced = f.sync();
    assert_eq!(synced.tasks[0].id, project.tasks[0].id);
    assert_eq!(synced.tasks[0].approved_digest, Some(task_review.digest));
    projects
        .terminal_launch_plan(&synced.id, &synced.terminals[0], f.env.clone())
        .unwrap();
    f.write(&text.replace("reviewed", "changed"));
    let changed = f.sync();
    assert!(
        !projects
            .review_initialization(&changed.id, &f.env)
            .unwrap()
            .common
            .trusted
    );
    assert!(projects
        .terminal_launch_plan(&changed.id, &changed.terminals[0], f.env.clone())
        .is_err());
}

#[test]
fn terminal_cwd_from_checkout_is_a_separate_approval_scope() {
    let f = Fixture::new();
    f.write("schema_version=1\n[terminals.dev]\ncwd='.'\n");
    let project = f.sync();
    let service = ProjectService { store: &f.store };
    let review = service.review_initialization(&project.id, &f.env).unwrap();
    assert!(review.common.trusted);
    assert!(!review.terminals[0].trusted);
    assert!(service
        .terminal_launch_plan(&project.id, &project.terminals[0], f.env.clone())
        .is_err());
    service
        .approve_initialization(review.revision, review)
        .unwrap();
    let project = f
        .store
        .load()
        .unwrap()
        .project(&project.id)
        .unwrap()
        .clone();
    service
        .terminal_launch_plan(&project.id, &project.terminals[0], f.env.clone())
        .unwrap();
}

#[test]
fn native_repeat_and_key_edits_preserve_unrelated_settings() {
    let mut shell = ShellConfig {
        executable: "/bin/sh".into(),
        native: Some(NativeShell {
            args: vec!["-i".into()],
            env: BTreeMap::from([("A".into(), "a".into()), ("B".into(), "b".into())]),
        }),
        login: false,
        init_cwd: "/tmp".into(),
        sources: vec![],
        trusted_digest: None,
    };
    portable::patch_native(&mut shell, true, None, &EnvironmentPatch::default()).unwrap();
    assert_eq!(shell.native.as_ref().unwrap().args, &["-i"]);
    portable::patch_native(
        &mut shell,
        true,
        None,
        &EnvironmentPatch {
            set: BTreeMap::from([("A".into(), "updated".into())]),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(shell.native.as_ref().unwrap().env["B"], "b");
    portable::patch_native(
        &mut shell,
        false,
        Some(vec![]),
        &EnvironmentPatch {
            unset: vec!["A".into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(shell.native.as_ref().unwrap().args.is_empty());
    assert_eq!(shell.native.as_ref().unwrap().env.len(), 1);
    portable::patch_native(
        &mut shell,
        false,
        None,
        &EnvironmentPatch {
            replace: Some(BTreeMap::from([("C".into(), "c".into())])),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        shell
            .native
            .as_ref()
            .unwrap()
            .env
            .keys()
            .collect::<Vec<_>>(),
        vec!["C"]
    );
    portable::patch_native(
        &mut shell,
        false,
        None,
        &EnvironmentPatch {
            clear: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(shell.native.as_ref().unwrap().env.is_empty());
}

#[test]
fn declaration_cas_preserves_external_edits_other_overrides_and_permissions() {
    let f = Fixture::new();
    let path = f.root.join("idk.toml");
    let initial="schema_version=1\n[shell]\nenv={A='a',B='b'}\n[tasks.test]\nprogram='build-tool'\nargs=['one']\n";
    portable::write_declaration(&f.store, &path, None, initial).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();
    let snapshot = portable::read(&f.root).unwrap().unwrap();
    let draft = portable::patch_declaration(
        &snapshot,
        &["shell", "env", "A"],
        Some(toml::Value::String("new".into())),
    )
    .unwrap();
    portable::write_declaration(&f.store, &path, Some(&snapshot.digest), &draft).unwrap();
    let updated = portable::read(&f.root).unwrap().unwrap();
    assert_eq!(updated.declaration.shell.unwrap().env["B"], "b");
    assert_eq!(
        updated.declaration.tasks["test"].args.as_ref().unwrap(),
        &["one"]
    );
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o664
    );
    assert!(portable::write_declaration(&f.store, &path, Some(&snapshot.digest), &draft).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), draft);
    assert!(portable::write_declaration(&f.store, &path, None, initial).is_err());
    assert!(!f.root.join(".idk-declaration.lock").exists());
    assert!(!fs::read_dir(&f.root).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".tmp")));
}

#[test]
fn stale_resolve_and_future_schema_preserve_local_data_and_original_declaration() {
    let f = Fixture::new();
    f.write("schema_version=1\n[tasks.test]\nprogram='build-tool'\n");
    let resolved = portable::resolve(&f.store, &f.root, &f.env).unwrap();
    f.write("schema_version=1\n[tasks.test]\nprogram='build-tool'\nargs=['new']\n");
    assert!(portable::apply(&f.store, resolved).is_err());
    assert!(f.store.load().unwrap().projects.is_empty());
    f.write("schema_version=99\n");
    assert!(portable::resolve(&f.store, &f.root, &f.env).is_err());
    assert_eq!(
        fs::read_to_string(f.root.join("idk.toml")).unwrap(),
        "schema_version=99\n"
    );
    let mut old = Workspace {
        schema: 2,
        ..Workspace::default()
    };
    f.store.save(&mut old, 0).unwrap();
    let original = fs::read(f.store.config_path()).unwrap();
    f.write("schema_version=1\n");
    f.sync();
    assert_eq!(f.store.load().unwrap().schema, 3);
    assert_eq!(
        fs::read(f.store.config_dir.join("workspace.schema2.toml")).unwrap(),
        original
    );
    atomic_write(&f.store.config_path(), b"schema=99\nrevision=0\n").unwrap();
    assert!(f.store.load().is_err());
}

#[test]
fn explicit_missing_program_is_not_replaced_and_typos_include_file_context() {
    let f = Fixture::new();
    f.write("schema_version=1\n[tasks.test]\nprogram='./does-not-exist'\n");
    assert!(format!(
        "{:#}",
        portable::resolve(&f.store, &f.root, &f.env).unwrap_err()
    )
    .contains("no substitute"));
    f.write("schema_version=1\n[tasks.test]\nprogam='build-tool'\n");
    let error = format!("{:#}", portable::read(&f.root).unwrap_err());
    assert!(
        error.contains("idk.toml") && error.contains("progam"),
        "{error}"
    );
    assert!(!f.root.join("executed").exists());
}

#[test]
fn concurrent_initializers_using_different_stores_never_clobber_each_other() {
    let f = Fixture::new();
    let other = Store::open(Some(&f.temp.path().join("other-local"))).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let writes: Vec<_> = [(f.store.clone(), "first"), (other, "second")]
        .into_iter()
        .map(|(store, name)| {
            let barrier = barrier.clone();
            let path = f.root.join("idk.toml");
            std::thread::spawn(move || {
                barrier.wait();
                portable::write_declaration(
                    &store,
                    &path,
                    None,
                    &format!("schema_version=1\nname='{name}'\n"),
                )
            })
        })
        .collect();
    barrier.wait();
    let outcomes: Vec<_> = writes
        .into_iter()
        .map(|writer| writer.join().unwrap())
        .collect();
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    let declaration = portable::read(&f.root).unwrap().unwrap();
    assert!(matches!(
        declaration.declaration.name.as_deref(),
        Some("first" | "second")
    ));
    assert!(!fs::read_dir(&f.root).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".tmp")));
}

#[test]
fn export_keeps_advanced_tasks_relative_paths_and_predecessor_keys_without_local_ids() {
    let f = Fixture::new();
    f.write("schema_version=1\n[shell]\nprogram='/bin/sh'\n[editor]\nprogram='/bin/true'\nargs=['{file}']\n[tasks.build]\nprogram='build-tool'\nargs=['one']\nartifact='build/app'\nbuild_outputs=['build']\ntimeout_seconds=12\ncontinue_on_failure=true\n[tasks.test]\ninterpreter='/bin/sh'\nscript='printf test'\nartifact_from_task='build'\ncwd='.'\ndisable_logging=true\n");
    let project = f.sync();
    let declaration = portable::export_declaration(&project).unwrap();
    let raw = toml::to_string_pretty(&declaration).unwrap();
    assert!(!raw.contains(&project.id));
    for task in &project.tasks {
        assert!(!raw.contains(&task.id));
    }
    assert!(
        !raw.contains("approved_digest")
            && !raw.contains("revision")
            && !raw.contains("trusted_digest")
    );
    assert_eq!(
        declaration.tasks["test"].artifact_from_task.as_deref(),
        Some("build")
    );
    assert_eq!(
        declaration.tasks["build"].artifact,
        Some(PathBuf::from("build/app"))
    );
    assert_eq!(declaration.tasks["build"].timeout_seconds, Some(12));
    assert!(declaration.tasks["build"].continue_on_failure);
    assert_eq!(
        declaration.editor.unwrap().args,
        Some(vec!["{file}".into()])
    );
    let second = f.temp.path().join("export-checkout");
    fs::create_dir(&second).unwrap();
    fs::write(second.join("idk.toml"), raw).unwrap();
    let resolved = portable::resolve(&f.store, &second, &f.env).unwrap();
    assert_eq!(
        resolved
            .project
            .tasks
            .iter()
            .find(|t| t.name == "build")
            .unwrap()
            .artifact,
        Some(second.join("build/app"))
    );
}

#[test]
fn portable_root_reconnect_preserves_mapping_and_requires_the_same_declaration() {
    let f = Fixture::new();
    f.write("schema_version=1\n[tasks.test]\nprogram='build-tool'\n");
    let project = f.sync();
    let moved = f.temp.path().join("moved-checkout");
    fs::rename(&f.root, &moved).unwrap();
    let service = ProjectService { store: &f.store };
    let preview = service
        .preview_rebind_root(&project.id, moved.clone())
        .unwrap();
    service.rebind_root(preview.revision, preview).unwrap();
    portable::ensure_current(&f.store, &project.id).unwrap();
    let refreshed = portable::apply(
        &f.store,
        portable::resolve(&f.store, &moved, &f.env).unwrap(),
    )
    .unwrap();
    assert_eq!(refreshed.id, project.id);
    assert_eq!(refreshed.tasks[0].id, project.tasks[0].id);
    assert_eq!(refreshed.tasks[0].cwd, moved);
}

#[test]
fn project_explicit_editor_and_shell_mask_unavailable_lower_priority_programs() {
    let f = Fixture::new();
    let preferences = Preferences {
        schema_version: 1,
        shell: Some(ShellDeclaration {
            program: Some("/unavailable/user-shell".into()),
            args: Some(vec!["-i".into()]),
            ..Default::default()
        }),
        editor: Some(EditorDeclaration {
            program: Some("/unavailable/user-editor".into()),
            args: Some(vec!["{file}".into()]),
            ..Default::default()
        }),
    };
    portable::save_preferences(&f.store, None, &preferences).unwrap();
    f.write("schema_version=1\n[shell]\nprogram='/bin/sh'\n[editor]\nprogram='/bin/true'\n");
    let resolved = portable::resolve(&f.store, &f.root, &f.env).unwrap();
    assert_eq!(resolved.project.shell.executable, Path::new("/bin/sh"));
    assert_eq!(resolved.project.shell.native.unwrap().args, &["-i"]);
    assert_eq!(
        resolved.project.editor.unwrap().executable,
        Path::new("/bin/true")
    );
}

#[test]
fn default_editor_comes_from_request_and_existing_explicit_choice_is_preserved() {
    let f = Fixture::new();
    let mut values = f.env.variables().clone();
    values.insert("EDITOR".into(), "/bin/true --literal".into());
    let env = LaunchEnvironment::from_variables(values).unwrap();
    let resolved = portable::resolve(&f.store, &f.root, &env).unwrap();
    assert_eq!(resolved.origins["editor"], "discovered: EDITOR");
    let project = portable::apply(&f.store, resolved).unwrap();
    assert_eq!(
        project.editor.as_ref().unwrap().executable,
        Path::new("/bin/true").canonicalize().unwrap()
    );
    let again = portable::resolve(&f.store, &f.root, &f.env).unwrap();
    assert_eq!(again.project.editor, project.editor);
}
