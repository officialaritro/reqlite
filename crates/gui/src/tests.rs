//! Behaviour the window must keep: shortcuts, Save, Send and Cancel.

use super::*;
use iced::keyboard::key::{Code, Physical};
use iced_test::simulator;

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
    let opened = open(args.file.as_deref());
    boot(&args, &opened, Instant::now()).0
}

const FILE: &str = "version = 1\nname = \"t\"\nmethod = \"GET\"\nurl = \"http://h/\"\n";

#[test]
fn cmd_shortcuts_pick_their_action_and_plain_keys_do_nothing() {
    let cmd = Modifiers::COMMAND;
    assert!(matches!(
        shortcut(&ch("1"), cmd),
        Some(Msg::Tab(Tab::Query))
    ));
    assert!(matches!(
        shortcut(&ch("2"), cmd),
        Some(Msg::Tab(Tab::Headers))
    ));
    assert!(matches!(shortcut(&ch("3"), cmd), Some(Msg::Tab(Tab::Body))));
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
    a.send = Send::Running(handle);
    let mut ui = simulator(view::view(&a));
    assert!(ui.find("Send").is_err());
    ui.click("Cancel").unwrap();
    assert!(matches!(ui.into_messages().next(), Some(Msg::Cancel)));
}

#[test]
fn a_file_that_cannot_be_read_shows_why_and_never_saves() {
    let mut a = app(Some("version = 1\nnot toml at all"));
    assert!(a.file.is_none());
    drop(update(&mut a, Msg::Url("http://other/".into())));
    let mut ui = simulator(view::view(&a));
    assert!(ui.find("cannot save").is_ok());
    ui.click("Save").unwrap();
    assert!(ui.into_messages().next().is_none());
}

#[test]
fn tabs_show_how_many_entries_they_hold() {
    let a = app(Some(
        "version = 1\nname = \"t\"\nmethod = \"POST\"\nurl = \"http://h/\"\nbody = \"{}\"\n\n[headers]\nA = \"1\"\nB = [\"2\", \"3\"]\n",
    ));
    assert_eq!(a.tab, Tab::Headers, "opens on the first tab with content");
    let mut ui = simulator(view::view(&a));
    assert!(ui.find("3").is_ok(), "three header entries");
    ui.click("Body").unwrap();
    assert!(matches!(
        ui.into_messages().next(),
        Some(Msg::Tab(Tab::Body))
    ));
}
