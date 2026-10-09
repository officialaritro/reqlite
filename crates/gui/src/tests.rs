//! Behaviour the window must keep: shortcuts, Save, Send, Cancel and tabs.

use super::*;
use crate::motion::Motion;
use iced::keyboard::key::{Code, Physical};
use iced::mouse;
use iced_test::simulator;
use std::time::Duration;

fn ch(c: &str) -> Key {
    Key::Character(c.into())
}

fn press(key: Key, code: Code, modifiers: Modifiers) -> text_editor::KeyPress {
    text_editor::KeyPress {
        // macOS sends the character even with Cmd held.
        text: match &key {
            Key::Character(c) => Some(c.clone()),
            _ => None,
        },
        modified_key: key.clone(),
        key,
        physical_key: Physical::Code(code),
        modifiers,
        status: text_editor::Status::Focused { is_hovered: false },
    }
}

fn app(text: Option<&str>) -> App {
    let dir = std::env::temp_dir().join(format!("reqlite-gui-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Tests run in parallel, so each one gets its own file.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let file = dir.join(format!("{n}.toml"));
    if let Some(text) = text {
        std::fs::write(&file, text).unwrap();
    }
    let args = Args {
        path: Some(file),
        env: None,
    };
    boot(&args, &start(args.path.as_deref()), Instant::now(), None).0
}

/// The shown tab.
fn active(a: &mut App) -> &mut Doc {
    a.doc_mut().unwrap()
}

const FILE: &str = "version = 1\nname = \"t\"\nmethod = \"GET\"\nurl = \"http://h/\"\n";

#[test]
fn cmd_shortcuts_pick_their_action_and_plain_keys_do_nothing() {
    let cmd = Modifiers::COMMAND;
    assert!(matches!(
        shortcut(&ch("1"), cmd),
        Some(Msg::Section(Section::Query))
    ));
    assert!(matches!(
        shortcut(&ch("2"), cmd),
        Some(Msg::Section(Section::Headers))
    ));
    assert!(matches!(
        shortcut(&ch("3"), cmd),
        Some(Msg::Section(Section::Body))
    ));
    assert!(matches!(shortcut(&ch("l"), cmd), Some(Msg::FocusUrl)));
    assert!(matches!(shortcut(&ch("s"), cmd), Some(Msg::Save)));
    assert!(matches!(
        shortcut(&Key::Named(Named::Enter), cmd),
        Some(Msg::Send)
    ));
    assert!(shortcut(&ch("1"), Modifiers::empty()).is_none());
    assert!(shortcut(&Key::Named(Named::Enter), Modifiers::SHIFT).is_none());
}

#[test]
fn a_shortcut_never_types_into_the_editor() {
    let typed = |key, code, m| editor_keys(press(key, code, m));
    for (c, code) in [("1", Code::Digit1), ("s", Code::KeyS), ("l", Code::KeyL)] {
        assert!(typed(ch(c), code, Modifiers::COMMAND).is_none(), "Cmd+{c}");
    }
    assert!(typed(Key::Named(Named::Enter), Code::Enter, Modifiers::COMMAND).is_none());
    assert!(matches!(
        typed(ch("2"), Code::Digit2, Modifiers::empty()),
        Some(text_editor::Binding::Insert('2'))
    ));
}

#[test]
fn save_is_enabled_only_when_the_form_differs_from_the_file() {
    let mut clean = app(Some(FILE));
    let mut ui = simulator(view::view(&clean));
    ui.click("Save").unwrap();
    assert!(ui.into_messages().next().is_none(), "a clean file saves");

    drop(update(&mut clean, Msg::Url("http://other/".into())));
    let mut ui = simulator(view::view(&clean));
    ui.click("Save").unwrap();
    assert!(matches!(ui.into_messages().next(), Some(Msg::Save)));
}

#[test]
fn send_turns_into_cancel_while_a_send_runs() {
    let mut a = app(Some(FILE));
    let mut ui = simulator(view::view(&a));
    ui.click("Send").unwrap();
    assert!(matches!(ui.into_messages().next(), Some(Msg::Send)));

    let (_task, handle) = Task::<Msg>::none().abortable();
    active(&mut a).send = Send::Running(handle);
    let mut ui = simulator(view::view(&a));
    assert!(ui.find("Send").is_err());
    ui.click("Cancel").unwrap();
    assert!(matches!(ui.into_messages().next(), Some(Msg::Cancel)));
}

#[test]
fn a_file_that_cannot_be_read_shows_why_and_never_saves() {
    let mut a = app(Some("version = 1\nnot toml at all"));
    assert!(active(&mut a).file.is_none());
    drop(update(&mut a, Msg::Url("http://other/".into())));
    assert!(title(&a).contains("(cannot save)"), "{}", title(&a));
    let mut ui = simulator(view::view(&a));
    ui.click("Save").unwrap();
    assert!(ui.into_messages().next().is_none());
}

#[test]
fn tabs_show_how_many_entries_they_hold() {
    let mut a = app(Some(
        "version = 1\nname = \"t\"\nmethod = \"POST\"\nurl = \"http://h/\"\nbody = \"{}\"\n\n[headers]\nA = \"1\"\nB = [\"2\", \"3\"]\n",
    ));
    assert_eq!(
        active(&mut a).section,
        Section::Headers,
        "opens on the first section with content"
    );
    let mut ui = simulator(view::view(&a));
    assert!(ui.find("3").is_ok(), "three header entries");
    ui.click("Body").unwrap();
    assert!(matches!(
        ui.into_messages().next(),
        Some(Msg::Section(Section::Body))
    ));
}

/// A window showing a 1000-line response, and the scrollbar's x position.
fn scrolled(top: usize) -> (App, f32) {
    let mut a = app(Some(FILE));
    let body = "line\n".repeat(1000);
    let doc = Document::build(body.as_bytes()).unwrap();
    active(&mut a).viewer = Some(Viewer {
        doc: Arc::new(doc),
        top,
    });
    active(&mut a).send = Send::Finished(Ok(Summary {
        status: 200,
        elapsed: Duration::ZERO,
        bytes: 5000,
    }));
    // The side-by-side layout, without the sidebar: the scrollbar sits at the
    // window edge, past page padding 12, panel padding 10 and half the 12 px bar.
    a.sidebar.hidden = true;
    (a, 1024.0 - 12.0 - 10.0 - 6.0)
}

fn scroll_targets(ui: iced_test::Simulator<'_, Msg>) -> Vec<f64> {
    ui.into_messages()
        .filter_map(|m| match m {
            Msg::ScrollTo(t) => Some(t),
            _ => None,
        })
        .collect()
}

#[test]
fn a_click_low_on_the_scrollbar_jumps_near_the_end() {
    let (a, x) = scrolled(0);
    let mut ui = simulator(view::view(&a));
    ui.point_at((x, 700.0));
    ui.simulate(iced_test::simulator::click());
    let targets = scroll_targets(ui);
    assert_eq!(targets.len(), 1, "{targets:?}");
    assert!(targets[0] > 800.0, "{targets:?}");
}

#[test]
fn dragging_the_scrollbar_follows_the_cursor_until_release() {
    let (a, x) = scrolled(0);
    let mut ui = simulator(view::view(&a));
    let event = |e| [iced::Event::Mouse(e)];
    let moved = |y| {
        event(mouse::Event::CursorMoved {
            position: iced::Point::new(x, y),
        })
    };
    ui.point_at((x, 300.0));
    ui.simulate(event(mouse::Event::ButtonPressed(mouse::Button::Left)));
    ui.point_at((x, 500.0));
    ui.simulate(moved(500.0));
    ui.simulate(event(mouse::Event::ButtonReleased(mouse::Button::Left)));
    ui.point_at((x, 700.0));
    ui.simulate(moved(700.0));
    let targets = scroll_targets(ui);
    assert_eq!(targets.len(), 2, "press, then one drag step: {targets:?}");
    assert!(targets[1] > targets[0], "{targets:?}");
}

#[test]
fn animations_end_so_the_window_stops_drawing_frames() {
    let ms = Duration::from_millis;
    let mut m = Motion::new(false);
    let t0 = m.now;
    assert!(!m.animating(t0), "a new window is still");

    m.reveal(t0);
    assert!(m.animating(t0 + ms(250)));
    assert!(!m.animating(t0 + ms(501)));

    let t1 = t0 + ms(600);
    m.running.go_mut(true, t1);
    assert!(m.animating(t1 + ms(100)));
    assert!(!m.animating(t1 + ms(201)));
}

#[test]
fn reduced_motion_shows_every_change_at_once() {
    let mut m = Motion::new(true);
    let t0 = m.now;
    m.reveal(t0);
    m.running.go_mut(true, t0);
    assert!(!m.animating(t0));
    assert_eq!(m.reveal.interpolate(0.0, 1.0, t0), 1.0);
}

/// A second tab for `text`, opened next to the first. It becomes the shown tab.
fn add_tab(a: &mut App, text: &str) -> DocId {
    let file = std::env::temp_dir().join(format!(
        "reqlite-gui-test-{}/tab-{}.toml",
        std::process::id(),
        a.next_id
    ));
    std::fs::write(&file, text).unwrap();
    let opened = doc::open(&file);
    let id = a.open_tab(Some(file), &opened);
    drop(update(a, Msg::Select(0)));
    id
}

#[test]
fn each_tab_keeps_its_own_form() {
    let mut a = app(Some(FILE));
    add_tab(&mut a, &FILE.replace("http://h/", "http://two/"));
    drop(update(&mut a, Msg::Url("http://edited/".into())));
    drop(update(&mut a, Msg::Select(1)));
    assert_eq!(active(&mut a).url, "http://two/");
    drop(update(&mut a, Msg::NextTab));
    assert_eq!(active(&mut a).url, "http://edited/");
    assert!(active(&mut a).unsaved());
}

#[test]
fn a_result_goes_to_the_tab_that_sent_it() {
    let mut a = app(Some(FILE));
    let first = a.docs[0].id;
    add_tab(&mut a, FILE);
    drop(update(&mut a, Msg::Select(1)));
    let done = Finished {
        result: Err("connection refused".into()),
        checked: None,
        opened: None,
        warning: None,
    };
    drop(update(&mut a, Msg::Sent(first, Box::new(done))));
    assert!(matches!(a.docs[0].send, Send::Finished(Err(_))));
    assert!(matches!(a.docs[1].send, Send::Idle));
}

#[test]
fn closing_a_tab_with_unsaved_changes_asks_first() {
    let mut a = app(Some(FILE));
    let id = a.docs[0].id;
    let clean = add_tab(&mut a, FILE);
    drop(update(&mut a, Msg::Url("http://edited/".into())));

    drop(update(&mut a, Msg::Close(id)));
    assert_eq!(a.docs.len(), 2, "the first close only asks");
    let mut ui = simulator(view::view(&a));
    ui.click("Keep").unwrap();
    assert!(matches!(
        ui.into_messages().next(),
        Some(Msg::Discard(false))
    ));
    drop(update(&mut a, Msg::Discard(false)));
    assert_eq!(a.docs.len(), 2);

    drop(update(&mut a, Msg::Close(id)));
    drop(update(&mut a, Msg::Discard(true)));
    assert_eq!(a.docs.len(), 1);
    assert_eq!(a.docs[0].id, clean);

    drop(update(&mut a, Msg::Close(clean)));
    assert!(a.docs.is_empty(), "a clean tab closes at once");
    let mut ui = simulator(view::view(&a));
    assert!(ui.find("No request is open.").is_ok());
}

/// A window on a workspace folder holding `get.toml` and `users/list.toml`.
fn workspace() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("users")).unwrap();
    std::fs::write(dir.path().join("get.toml"), FILE).unwrap();
    std::fs::write(dir.path().join("users/list.toml"), FILE).unwrap();
    let args = Args {
        path: Some(dir.path().to_path_buf()),
        env: None,
    };
    let a = boot(&args, &start(args.path.as_deref()), Instant::now(), None).0;
    (dir, a)
}

fn side(a: &mut App, m: SideMsg) {
    drop(update(a, Msg::Side(m)));
}

fn name(a: &mut App, action: sidebar::Action, text: &str) {
    side(a, SideMsg::Start(action));
    side(a, SideMsg::Text(text.into()));
    side(a, SideMsg::Commit);
}

#[test]
fn a_folder_opens_as_a_workspace_with_no_tab() {
    let (_dir, mut a) = workspace();
    assert!(a.docs.is_empty());
    let mut ui = simulator(view::view(&a));
    assert!(ui.find("get").is_ok());
    assert!(ui.find("users").is_ok());
    ui.click("list").unwrap();
    let opened: Vec<_> = ui.into_messages().collect();
    for m in opened {
        drop(update(&mut a, m));
    }
    assert_eq!(a.docs.len(), 1);
    assert!(
        a.docs[0]
            .file
            .as_ref()
            .unwrap()
            .ends_with("users/list.toml")
    );
}

#[test]
fn opening_a_file_twice_shows_its_tab_again() {
    let (dir, mut a) = workspace();
    let get = dir.path().join("get.toml");
    side(&mut a, SideMsg::Open(get.clone()));
    side(&mut a, SideMsg::Open(dir.path().join("users/list.toml")));
    side(&mut a, SideMsg::Open(get));
    assert_eq!(a.docs.len(), 2);
    assert_eq!(a.active, 0);
}

#[test]
fn a_new_request_is_written_by_its_first_save() {
    let (dir, mut a) = workspace();
    let root = dir.path().to_path_buf();
    name(
        &mut a,
        sidebar::Action::NewRequest { dir: root.clone() },
        "Get user",
    );
    let path = root.join("Get user.toml");
    assert!(!path.exists(), "nothing is written before Save");
    assert_eq!(a.doc().unwrap().file.as_deref(), Some(path.as_path()));
    assert!(a.doc().unwrap().unsaved());

    name(&mut a, sidebar::Action::NewRequest { dir: root }, "get");
    assert!(
        a.sidebar
            .edit
            .as_ref()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("already exists")
    );
}

#[test]
fn a_new_folder_shows_in_the_tree() {
    let (dir, mut a) = workspace();
    name(
        &mut a,
        sidebar::Action::NewFolder {
            dir: dir.path().join("users"),
        },
        "admin",
    );
    assert!(dir.path().join("users/admin").is_dir());
    assert!(a.sidebar.edit.is_none());
    let mut ui = simulator(view::view(&a));
    assert!(ui.find("admin").is_ok());
}

#[test]
fn a_bad_name_is_refused_and_nothing_changes() {
    let (dir, mut a) = workspace();
    name(
        &mut a,
        sidebar::Action::NewFolder {
            dir: dir.path().into(),
        },
        "../out",
    );
    assert!(a.sidebar.edit.as_ref().unwrap().error.is_some());
    assert!(!dir.path().parent().unwrap().join("out").exists());
}

#[test]
fn renaming_moves_the_file_and_its_open_tab_follows() {
    let (dir, mut a) = workspace();
    let users = dir.path().join("users");
    side(&mut a, SideMsg::Open(users.join("list.toml")));
    name(
        &mut a,
        sidebar::Action::Rename {
            path: users.clone(),
        },
        "people",
    );
    let moved = dir.path().join("people/list.toml");
    assert!(moved.is_file() && !users.exists());
    assert_eq!(a.docs[0].file.as_deref(), Some(moved.as_path()));

    name(&mut a, sidebar::Action::Rename { path: moved }, "all");
    let renamed = dir.path().join("people/all.toml");
    assert!(renamed.is_file());
    assert_eq!(a.docs[0].file.as_deref(), Some(renamed.as_path()));
    assert_eq!(a.docs[0].label, "all.toml");
}

#[test]
fn delete_asks_first_closes_the_tab_and_keeps_full_folders() {
    let (dir, mut a) = workspace();
    let get = dir.path().join("get.toml");
    side(&mut a, SideMsg::Open(get.clone()));
    side(&mut a, SideMsg::Delete(get.clone()));
    side(&mut a, SideMsg::ConfirmDelete(false));
    assert!(get.exists(), "Keep keeps the file");

    side(&mut a, SideMsg::Delete(get.clone()));
    side(&mut a, SideMsg::ConfirmDelete(true));
    assert!(!get.exists());
    assert!(a.docs.is_empty(), "its tab closes");

    let users = dir.path().join("users");
    side(&mut a, SideMsg::Delete(users.clone()));
    side(&mut a, SideMsg::ConfirmDelete(true));
    assert!(
        users.join("list.toml").exists(),
        "a folder with requests stays"
    );
    assert!(a.notice.as_deref().unwrap().contains("not empty"));
}

const OTHER: &str = "version = 1\nname = \"t\"\nmethod = \"POST\"\nurl = \"http://elsewhere/\"\n";

#[test]
fn a_clean_tab_follows_its_file_on_disk() {
    let (dir, mut a) = workspace();
    let get = dir.path().join("get.toml");
    side(&mut a, SideMsg::Open(get.clone()));
    std::fs::write(&get, OTHER).unwrap();
    drop(update(&mut a, Msg::FsChanged));
    let d = a.doc().unwrap();
    assert_eq!(
        (d.method.as_str(), d.url.as_str()),
        ("POST", "http://elsewhere/")
    );
    assert!(!d.unsaved());
}

#[test]
fn a_tab_with_changes_keeps_them_and_save_stops_at_a_disk_change() {
    let (dir, mut a) = workspace();
    let get = dir.path().join("get.toml");
    side(&mut a, SideMsg::Open(get.clone()));
    drop(update(&mut a, Msg::Url("http://mine/".into())));
    std::fs::write(&get, OTHER).unwrap();
    drop(update(&mut a, Msg::FsChanged));
    assert_eq!(a.doc().unwrap().url, "http://mine/", "typing is kept");

    let doc = a.doc().unwrap();
    let req = doc.draft().to_request().unwrap();
    let first = doc::write(&get, &req, doc.saved.as_deref(), doc.conflict).unwrap();
    assert_eq!(first, doc::Written::Changed);
    assert_eq!(
        std::fs::read_to_string(&get).unwrap(),
        OTHER,
        "nothing written"
    );
    let id = doc.id;
    drop(update(&mut a, Msg::Saved(id, Ok(first))));
    assert!(a.notice.as_deref().unwrap().contains("changed on disk"));

    let doc = a.doc().unwrap();
    let second = doc::write(&get, &req, doc.saved.as_deref(), doc.conflict).unwrap();
    assert!(
        matches!(second, doc::Written::Saved(_)),
        "a second Save overwrites"
    );
    assert!(
        std::fs::read_to_string(&get)
            .unwrap()
            .contains("http://mine/")
    );
}

#[test]
fn a_new_request_does_not_replace_a_file_made_meanwhile() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new.toml");
    let req = reqlite_format::parse(FILE).unwrap();
    std::fs::write(&path, OTHER).unwrap();
    assert_eq!(
        doc::write(&path, &req, None, false).unwrap(),
        doc::Written::Changed
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), OTHER);
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(
        doc::write(&path, &req, None, false).unwrap(),
        doc::Written::Saved(_)
    ));
}

#[test]
fn the_environment_picker_lists_the_workspace_envs_and_switches_without_restart() {
    let (dir, mut a) = workspace();
    std::fs::create_dir(dir.path().join("envs")).unwrap();
    for f in ["dev.toml", "dev.local.toml", "prod.toml"] {
        std::fs::write(dir.path().join("envs").join(f), "version = 1\n").unwrap();
    }
    drop(update(&mut a, Msg::FsChanged));
    let envs = dir.path().join("envs");
    assert_eq!(
        a.env_choices(),
        [
            Env(None),
            Env(Some(envs.join("dev.toml"))),
            Env(Some(envs.join("prod.toml")))
        ]
    );
    assert_eq!(Env(Some(envs.join("prod.toml"))).to_string(), "env prod");

    drop(update(
        &mut a,
        Msg::PickEnv(Env(Some(envs.join("prod.toml")))),
    ));
    assert_eq!(a.env.as_deref(), Some(envs.join("prod.toml").as_path()));
    drop(update(&mut a, Msg::PickEnv(Env(None))));
    assert!(a.env.is_none());
}

#[test]
fn an_env_from_outside_the_workspace_stays_a_choice() {
    let (_dir, mut a) = workspace();
    let outside = PathBuf::from("/elsewhere/staging.toml");
    a.env = Some(outside.clone());
    assert_eq!(a.env_choices(), [Env(None), Env(Some(outside))]);
}

fn sent(url: &str, outcome: reqlite_store::Outcome) -> reqlite_store::Entry {
    reqlite_store::Entry {
        file: Some("users/list.toml".into()),
        env: None,
        request: reqlite_store::SentRequest {
            method: "POST".into(),
            url: url.into(),
            headers: vec![("Authorization".into(), "Bearer {{token}}".into())],
            query: vec![("tag".into(), "a".into()), ("tag".into(), "b".into())],
            body: Some("{\"name\": \"ada\"}".into()),
        },
        outcome,
    }
}

fn hist(a: &mut App, m: history::HistMsg) {
    drop(update(a, Msg::History(m)));
}

#[test]
fn a_history_entry_opens_in_a_new_tab_that_cannot_overwrite_a_file() {
    let (_dir, mut a) = workspace();
    let entry = sent(
        "http://api/users?x=1",
        reqlite_store::Outcome::Response {
            status: 201,
            headers: vec![],
            body: b"{\"id\": 7}".to_vec(),
            body_len: 9,
            elapsed_ms: 40,
        },
    );
    hist(&mut a, history::HistMsg::Fetched(Ok(Some(entry))));
    assert_eq!(a.docs.len(), 1);
    let d = active(&mut a);
    assert_eq!(d.label, "POST /users");
    assert_eq!(
        (d.method.as_str(), d.url.as_str()),
        ("POST", "http://api/users?x=1")
    );
    assert_eq!(d.headers.text(), "Authorization: Bearer {{token}}\n");
    assert_eq!(d.query.text(), "tag: a\ntag: b\n");
    assert_eq!(d.body.text(), "{\"name\": \"ada\"}");
    assert!(d.file.is_none(), "no file, so Save cannot write over one");
    assert!(matches!(
        d.send,
        Send::Finished(Ok(Summary { status: 201, .. }))
    ));
    assert_eq!(
        d.viewer.as_ref().unwrap().doc.lines(0, 5).unwrap(),
        ["{", "  \"id\": 7", "}"]
    );
    assert!(a.notice.is_none());
}

#[test]
fn a_restored_failure_and_a_cut_body_say_so() {
    let (_dir, mut a) = workspace();
    let failed = sent(
        "http://down/",
        reqlite_store::Outcome::Failed {
            error: "cannot connect".into(),
        },
    );
    hist(&mut a, history::HistMsg::Fetched(Ok(Some(failed))));
    assert!(matches!(&active(&mut a).send, Send::Finished(Err(e)) if e == "cannot connect"));

    let cut = sent(
        "http://big/",
        reqlite_store::Outcome::Response {
            status: 200,
            headers: vec![],
            body: vec![b'x'; 1024],
            body_len: 50 * 1024 * 1024,
            elapsed_ms: 80,
        },
    );
    hist(&mut a, history::HistMsg::Fetched(Ok(Some(cut))));
    assert!(a.notice.as_deref().unwrap().contains("first 1.0 KB"));
}

#[test]
fn history_can_show_only_the_shown_request() {
    let (dir, mut a) = workspace();
    let list = dir.path().join("users/list.toml");
    side(&mut a, SideMsg::Open(list.clone()));
    let row = |id, file: Option<&Path>| reqlite_store::Summary {
        id,
        at_ms: 0,
        file: file.map(|p| p.display().to_string()),
        method: "GET".into(),
        url: format!("http://h/{id}"),
        status: Some(200),
        error: None,
        elapsed_ms: Some(5),
    };
    drop(update(&mut a, Msg::Panel(Panel::History)));
    hist(
        &mut a,
        history::HistMsg::Listed(Ok(vec![row(2, None), row(1, Some(&list))])),
    );
    let count = |a: &App| {
        let mut ui = simulator(view::view(a));
        ["/1", "/2"].iter().filter(|u| ui.find(**u).is_ok()).count()
    };
    assert_eq!(count(&a), 2);
    hist(&mut a, history::HistMsg::ThisRequest(true));
    assert_eq!(count(&a), 1);
}

#[test]
fn the_left_panel_switches_between_files_and_history_and_hides() {
    let (_dir, mut a) = workspace();
    assert_eq!(a.left_panel(), Some(Panel::Files));
    drop(update(&mut a, Msg::Panel(Panel::History)));
    assert_eq!(a.left_panel(), Some(Panel::History));
    drop(update(&mut a, Msg::Panel(Panel::History)));
    assert_eq!(a.left_panel(), None, "choosing the shown panel hides it");

    let mut lone = app(Some(FILE));
    lone.workspace = None;
    assert_eq!(lone.left_panel(), None, "no workspace, no file tree");
    drop(update(&mut lone, Msg::Panel(Panel::History)));
    assert_eq!(lone.left_panel(), Some(Panel::History));
}

/// Presses `c` with Cmd held in the focused URL field, the way macOS sends it:
/// the modifier first, then the key with its text.
fn cmd_key_in_url(c: &str) -> Vec<Msg> {
    let a = app(Some(FILE));
    let mut ui = simulator(view::view(&a));
    ui.click(URL).unwrap();
    let key = ch(c);
    ui.simulate([
        iced::Event::Keyboard(keyboard::Event::ModifiersChanged(Modifiers::COMMAND)),
        iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: Physical::Code(Code::KeyS),
            location: keyboard::Location::Standard,
            modifiers: Modifiers::COMMAND,
            text: Some(c.into()),
            repeat: false,
        }),
    ]);
    ui.into_messages().collect()
}

#[test]
fn a_shortcut_never_types_into_the_url() {
    for c in ["s", "w", "l", "n", "1"] {
        let typed = cmd_key_in_url(c)
            .into_iter()
            .any(|m| matches!(m, Msg::Url(u) if u.ends_with(c)));
        assert!(!typed, "Cmd+{c} typed into the URL");
    }
}

#[test]
fn copy_and_paste_keys_still_reach_the_url() {
    // Cmd+A selects all: no edit, but the key is not swallowed either, so
    // nothing is typed. Plain letters still type.
    let a = app(Some(FILE));
    let mut ui = simulator(view::view(&a));
    ui.click(URL).unwrap();
    ui.typewrite("x");
    assert!(
        ui.into_messages()
            .any(|m| matches!(m, Msg::Url(u) if u.ends_with('x')))
    );
}

const JSON_FILE: &str = "version = 2\nname = \"t\"\nmethod = \"POST\"\nurl = \"http://h/\"\n\n[body]\ntype = \"json\"\ntext = '{\"a\": 1}'\n\n[auth]\ntype = \"bearer\"\ntoken = \"{{token}}\"\n";

#[test]
fn a_version_2_file_opens_clean_and_shows_its_body_type_and_auth() {
    let mut a = app(Some(JSON_FILE));
    let d = active(&mut a);
    assert!(!d.unsaved(), "opening changes nothing");
    assert_eq!(
        d.section,
        Section::Body,
        "opens on the first section with content"
    );
    assert_eq!(d.body_kind, BodyKind::Json);
    assert!(d.counts.auth);
    let mut ui = simulator(view::view(&a));
    assert!(
        ui.find("Adds Content-Type: application/json unless Headers sets one.")
            .is_ok()
    );
    drop(ui);
    drop(update(&mut a, Msg::Section(Section::Auth)));
    let mut ui = simulator(view::view(&a));
    assert!(
        ui.find("Use a {{secret}} so the value stays out of the file.")
            .is_ok()
    );
    assert!(ui.find("{{token}}").is_ok(), "the token field holds it");
}

#[test]
fn picking_a_body_type_and_auth_changes_what_is_saved() {
    let mut a = app(Some(FILE));
    drop(update(&mut a, Msg::Method("POST".into())));
    drop(update(&mut a, Msg::BodyKind(BodyKind::File)));
    drop(update(&mut a, Msg::BodyFile("data/p.bin".into())));
    drop(update(&mut a, Msg::AuthKind(AuthKind::ApiKey)));
    drop(update(
        &mut a,
        Msg::Auth(AuthField::KeyName, "X-Key".into()),
    ));
    drop(update(
        &mut a,
        Msg::Auth(AuthField::KeyValue, "{{key}}".into()),
    ));
    drop(update(&mut a, Msg::KeyIn(reqlite_format::KeyIn::Query)));
    let req = active(&mut a).draft().to_request().unwrap();
    assert_eq!(
        req.body,
        Some(reqlite_format::Body::File {
            path: "data/p.bin".into()
        })
    );
    assert_eq!(
        req.auth,
        Some(reqlite_format::Auth::ApiKey {
            name: "X-Key".into(),
            value: "{{key}}".into(),
            location: reqlite_format::KeyIn::Query
        })
    );
    assert!(active(&mut a).unsaved());
    assert!(matches!(
        shortcut(&ch("4"), Modifiers::COMMAND),
        Some(Msg::Section(Section::Auth))
    ));
}

#[test]
fn the_headers_tab_lists_the_response_headers_in_order() {
    let mut a = app(Some(FILE));
    let id = a.docs[0].id;
    let doc = Document::build_with(&b"<a>1</a>"[..], Some("text/xml")).unwrap();
    let done = Finished {
        checked: None,
        result: Ok(Loaded {
            doc: Arc::new(doc),
            summary: Summary {
                status: 200,
                elapsed: Duration::ZERO,
                bytes: 8,
            },
            headers: vec![
                ("content-type".into(), "text/xml".into()),
                ("x-trace".into(), "abc".into()),
            ],
        }),
        opened: None,
        warning: None,
    };
    drop(update(&mut a, Msg::Sent(id, Box::new(done))));
    let mut ui = simulator(view::view(&a));
    assert!(ui.find("Headers 2").is_ok());
    assert!(ui.find("x-trace").is_err(), "the body shows first");
    drop(ui);
    drop(update(&mut a, Msg::ResponseTab(doc::ResponseTab::Headers)));
    let mut ui = simulator(view::view(&a));
    assert!(ui.find("x-trace").is_ok() && ui.find("abc").is_ok());
}

#[test]
fn a_graphql_body_has_a_variables_editor_and_a_schema_button() {
    let mut a = app(Some(
        "version = 2\nname = \"q\"\nmethod = \"POST\"\nurl = \"http://h/graphql\"\n\n[body]\ntype = \"graphql\"\nquery = \"{ me { id } }\"\nvariables = '{\"a\": 1}'\n",
    ));
    drop(update(&mut a, Msg::Section(Section::Body)));
    let mut ui = simulator(view::view(&a));
    assert!(ui.find("Variables, as JSON").is_ok());
    ui.click("Schema").unwrap();
    assert!(matches!(ui.into_messages().next(), Some(Msg::Schema)));
    assert_eq!(a.docs[0].variables.text().trim_end(), "{\"a\": 1}");
}

#[test]
fn a_websocket_tab_connects_logs_and_sends() {
    use reqlite_engine::stream::{Event, Timed};
    let mut a = app(Some(
        "version = 1\nname = \"chat\"\nurl = \"ws://127.0.0.1:9/chat\"\n",
    ));
    let id = a.docs[0].id;
    let mut ui = simulator(view::view(&a));
    assert!(ui.find("Connect").is_ok());
    drop(ui);

    // The state a Connect leaves, with the engine's events fed in by hand.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    a.docs[0].live = Some(doc::Live {
        log: reqlite_gui::live::Log::default(),
        tx: Some(tx),
        websocket: true,
        open: false,
        message: String::new(),
        started: Instant::now(),
    });
    let at = |ms| Duration::from_millis(ms);
    for (ms, event) in [
        (5, Event::Open { status: 101 }),
        (
            9,
            Event::Received {
                name: None,
                data: "welcome".into(),
            },
        ),
    ] {
        drop(update(&mut a, Msg::Live(id, Timed { at: at(ms), event })));
    }
    drop(update(&mut a, Msg::LiveMessage("{\"a\": 1}".into())));
    drop(update(&mut a, Msg::LiveSend));
    assert_eq!(rx.try_recv().unwrap(), "{\"a\": 1}");
    assert_eq!(
        a.docs[0].live.as_ref().unwrap().message,
        "",
        "the box empties"
    );
    drop(update(
        &mut a,
        Msg::Live(
            id,
            Timed {
                at: at(12),
                event: Event::Sent {
                    data: "{\"a\": 1}".into(),
                },
            },
        ),
    ));
    let mut ui = simulator(view::view(&a));
    assert!(ui.find("Connected").is_ok() && ui.find("welcome").is_ok());
    assert!(ui.find("3 messages").is_ok());
    drop(ui);

    drop(update(
        &mut a,
        Msg::Live(
            id,
            Timed {
                at: at(20),
                event: Event::Closed {
                    reason: "closed by the server: 1000 done".into(),
                },
            },
        ),
    ));
    let live = a.docs[0].live.as_ref().unwrap();
    assert!(!live.open && live.tx.is_none());
    let mut ui = simulator(view::view(&a));
    assert!(ui.find("Closed").is_ok() && ui.find("closed by the server: 1000 done").is_ok());
    assert!(ui.find("Connect").is_ok(), "it can connect again");
}

#[test]
fn a_grpc_answer_shows_its_grpc_status() {
    let mut a = app(Some(
        "version = 4\nname = \"g\"\nurl = \"http://h:1\"\n\n[grpc]\nmethod = \"t.S/Get\"\n",
    ));
    let id = a.docs[0].id;
    let done = Finished {
        checked: None,
        result: Ok(Loaded {
            doc: Arc::new(Document::build_with(&b"{}"[..], None).unwrap()),
            summary: Summary {
                status: 200,
                elapsed: Duration::ZERO,
                bytes: 2,
            },
            headers: vec![("grpc-status".into(), "5".into())],
        }),
        opened: None,
        warning: None,
    };
    drop(update(&mut a, Msg::Sent(id, Box::new(done))));
    let mut ui = simulator(view::view(&a));
    assert!(ui.find("5 NOT_FOUND").is_ok());
    assert!(
        ui.find("gRPC").is_ok() && ui.find("GET").is_err(),
        "no method choice"
    );
    assert!(ui.find("200 OK").is_err());
}

#[test]
fn a_captured_value_fills_the_next_send() {
    let mut a = app(Some(
        "version = 1\nname = \"user\"\nurl = \"http://127.0.0.1:9/users/{{id}}\"\n",
    ));
    drop(update(&mut a, Msg::Send));
    let Send::Finished(Err(e)) = &a.docs[0].send else {
        panic!("sent without a value for id")
    };
    assert!(e.contains("id"), "{e}");
    a.captured.insert("id".into(), "7".into());
    a.docs[0].send = Send::Idle;
    drop(update(&mut a, Msg::Send));
    assert!(a.docs[0].running(), "the captured id fills the URL");
}

#[test]
fn the_tests_tab_shows_each_check_and_captures_are_kept() {
    use reqlite_engine::check::{Checked, Outcome};
    let mut a = app(Some(FILE));
    let id = a.docs[0].id;
    let line = |text: &str, pass: bool, detail: &str| Outcome {
        text: text.into(),
        pass,
        detail: detail.into(),
    };
    let done = Finished {
        checked: Some(Checked {
            outcomes: vec![
                line("status == 201", false, "got 200"),
                line("capture id = json $.id", true, ""),
            ],
            captured: vec![("id".into(), "42".into())],
        }),
        result: Ok(Loaded {
            doc: Arc::new(Document::build_with(&b"{}"[..], None).unwrap()),
            summary: Summary {
                status: 200,
                elapsed: Duration::ZERO,
                bytes: 2,
            },
            headers: Vec::new(),
        }),
        opened: None,
        warning: None,
    };
    drop(update(&mut a, Msg::Sent(id, Box::new(done))));
    assert_eq!(a.captured.get("id").map(String::as_str), Some("42"));
    let mut ui = simulator(view::view(&a));
    assert!(ui.find("Tests 1/2").is_ok());
    assert!(ui.find("got 200").is_err(), "the body shows first");
    drop(ui);
    drop(update(&mut a, Msg::ResponseTab(doc::ResponseTab::Tests)));
    let mut ui = simulator(view::view(&a));
    assert!(ui.find("status == 201").is_ok() && ui.find("got 200").is_ok());
    assert!(ui.find("capture id = json $.id").is_ok());
}

#[test]
fn a_restored_xml_response_keeps_its_headers_and_kind() {
    let (_dir, mut a) = workspace();
    let entry = sent(
        "http://api/feed",
        reqlite_store::Outcome::Response {
            status: 200,
            headers: vec![("Content-Type".into(), b"application/xml".to_vec())],
            body: b"<feed><item/></feed>".to_vec(),
            body_len: 20,
            elapsed_ms: 3,
        },
    );
    hist(&mut a, history::HistMsg::Fetched(Ok(Some(entry))));
    let d = active(&mut a);
    assert_eq!(
        d.response_headers,
        [("Content-Type".to_string(), "application/xml".to_string())]
    );
    let v = d.viewer.as_ref().unwrap();
    assert_eq!(v.doc.kind(), reqlite_viewer::Kind::Markup);
    assert_eq!(
        v.doc.lines(0, 5).unwrap(),
        ["<feed>", "  <item/>", "</feed>"]
    );
}

#[test]
fn the_first_start_offers_a_folder_and_the_next_start_reopens_it() {
    let data = tempfile::tempdir().unwrap();
    let args = Args {
        path: None,
        env: None,
    };
    let first = |data: &Path| {
        boot(
            &args,
            &Start::Nothing,
            Instant::now(),
            Some(data.to_path_buf()),
        )
        .0
    };

    let mut a = first(data.path());
    assert!(a.workspace.is_none() && a.docs.is_empty(), "no folder yet");
    let mut ui = simulator(view::view(&a));
    ui.click("Open folder…").unwrap();
    assert!(matches!(ui.into_messages().next(), Some(Msg::OpenFolder)));

    let (ws, _) = workspace();
    drop(update(
        &mut a,
        Msg::FolderPicked(Some(ws.path().to_path_buf())),
    ));
    assert_eq!(a.workspace.as_ref().unwrap().root, ws.path());
    assert_eq!(a.left_panel(), Some(Panel::Files));

    let again = first(data.path());
    let root = &again.workspace.as_ref().expect("reopened").root;
    assert_eq!(
        root.canonicalize().unwrap(),
        ws.path().canonicalize().unwrap()
    );

    drop(update(&mut a, Msg::FolderPicked(None)));
    assert!(a.workspace.is_some(), "a cancelled dialog changes nothing");
    assert!(matches!(
        shortcut(&ch("o"), Modifiers::COMMAND),
        Some(Msg::OpenFolder)
    ));
}

/// What Chrome's "Copy as cURL (bash)" gives: parts joined with `\` and a line break.
const CHROME_CURL: &str = "curl 'https://api.example.com/v1/users?page=2' \\\n  -H 'accept: application/json' \\\n  -H 'content-type: application/json' \\\n  -u ada:pw \\\n  --data-raw '{\"name\":\"ada\",\n\"tags\":[1]}'";

#[test]
fn a_pasted_curl_command_fills_the_shown_request() {
    let mut a = app(Some(FILE));
    drop(update(&mut a, Msg::PasteCurl(CHROME_CURL.into())));
    let d = active(&mut a);
    assert_eq!(d.method, "POST", "curl sends a body as POST");
    assert_eq!(d.url, "https://api.example.com/v1/users?page=2");
    assert_eq!(
        d.headers.text(),
        "accept: application/json\ncontent-type: application/json\n"
    );
    assert_eq!(
        d.body.text(),
        "{\"name\":\"ada\",\n\"tags\":[1]}",
        "line breaks in the body stay"
    );
    assert_eq!(d.auth.kind, AuthKind::Basic);
    assert_eq!(d.auth.username, "ada");
    assert_eq!(
        d.auth.password, "{{password}}",
        "the password never enters the form"
    );
    assert!(d.unsaved(), "the file now differs");
    assert_eq!(d.name, "t", "an existing request keeps its name");
    assert!(
        a.notice
            .as_deref()
            .unwrap()
            .starts_with("Imported the cURL command"),
        "{:?}",
        a.notice
    );
    assert!(
        a.notice.as_deref().unwrap().contains("password"),
        "{:?}",
        a.notice
    );
}

#[test]
fn a_clean_curl_command_says_so_and_an_untitled_tab_takes_the_command_name() {
    let mut a = app(None);
    drop(update(
        &mut a,
        Msg::PasteCurl("curl -H 'X-Tag: a' https://h/x".into()),
    ));
    assert_eq!(a.notice.as_deref(), Some("Imported the cURL command."));
    let d = active(&mut a);
    assert_eq!((d.method.as_str(), d.url.as_str()), ("GET", "https://h/x"));
    assert_eq!(d.headers.text(), "X-Tag: a\n");
    assert_eq!(
        d.section,
        Section::Headers,
        "opens on what the command filled"
    );
}

#[test]
fn a_curl_command_that_cannot_be_read_changes_nothing_and_says_why() {
    let mut a = app(Some(FILE));
    drop(update(&mut a, Msg::PasteCurl("curl 'https://h/".into())));
    let d = active(&mut a);
    assert_eq!(d.url, "http://h/");
    assert!(!d.unsaved());
    assert!(
        a.notice
            .as_deref()
            .unwrap()
            .starts_with("Cannot read the cURL command"),
        "{:?}",
        a.notice
    );
}

#[test]
fn the_url_field_asks_for_a_paste_of_a_curl_command_only() {
    use crate::guard::PasteProbe;
    let ask = |text: &str| PasteProbe::curl(text);
    assert!(matches!(ask("curl https://h/"), Some(Msg::PasteCurl(t)) if t == "curl https://h/"));
    assert!(ask("https://h/users").is_none());
}
