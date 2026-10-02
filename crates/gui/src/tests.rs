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
        file: Some(file),
        env: None,
    };
    let opened = args.file.as_deref().map_or(Opened::Missing, doc::open);
    boot(&args, &opened, Instant::now()).0
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
    // Window edge, page padding 12, panel padding 10, half the 12 px bar.
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
