//! The workspace sidebar: the folder tree of request files, and the file
//! actions on it. Request files stay the source of truth: every action here is
//! a plain file operation, and the tree is read back from disk after it.

use super::{App, Msg, doc};
use crate::style;
use iced::widget::{
    Space, button, column, container, mouse_area, row, scrollable, text, text_input,
};
use iced::{Alignment, Element, Length, Padding, Task};
use reqlite_gui::workspace::{self, Kind, Node};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const WIDTH: f32 = 230.0;
const INDENT: f32 = 14.0;
pub const NAME_INPUT: iced::widget::Id = iced::widget::Id::new("sidebar-name");

pub struct Workspace {
    pub root: PathBuf,
    pub tree: Vec<Node>,
    /// `envs/*.toml`, for the environment picker.
    pub envs: Vec<PathBuf>,
}

#[derive(Default)]
pub struct Sidebar {
    pub hidden: bool,
    /// Folders the user closed. New folders start open.
    pub collapsed: HashSet<PathBuf>,
    /// The entry whose actions show under it (right click).
    pub menu: Option<PathBuf>,
    pub edit: Option<Edit>,
    /// An entry waiting for "delete?" to be confirmed.
    pub deleting: Option<PathBuf>,
}

pub struct Edit {
    pub action: Action,
    pub text: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    NewRequest { dir: PathBuf },
    NewFolder { dir: PathBuf },
    Rename { path: PathBuf },
}

#[derive(Clone, Debug)]
pub enum SideMsg {
    Open(PathBuf),
    Toggle(PathBuf),
    Menu(Option<PathBuf>),
    Start(Action),
    Text(String),
    Commit,
    Delete(PathBuf),
    ConfirmDelete(bool),
    ToggleHidden,
}

impl Workspace {
    pub fn open(root: PathBuf) -> Workspace {
        let mut w = Workspace {
            root,
            tree: Vec::new(),
            envs: Vec::new(),
        };
        w.rescan();
        w
    }

    pub fn rescan(&mut self) {
        // A root that cannot be read shows an empty tree; the error shows on use.
        self.tree = workspace::scan(&self.root).unwrap_or_default();
        self.envs = workspace::environments(&self.root);
    }
}

pub fn update(app: &mut App, msg: SideMsg) -> Task<Msg> {
    match msg {
        SideMsg::Open(path) => {
            app.sidebar.menu = None;
            if let Some(i) = app
                .docs
                .iter()
                .position(|d| d.file.as_deref() == Some(&path))
            {
                app.active = i;
            } else {
                let opened = doc::open(&path);
                app.open_tab(Some(path), &opened);
            }
        }
        SideMsg::Toggle(path) => {
            if !app.sidebar.collapsed.remove(&path) {
                app.sidebar.collapsed.insert(path);
            }
        }
        SideMsg::Menu(path) => {
            app.sidebar.menu = path;
            app.sidebar.deleting = None;
        }
        SideMsg::Start(action) => {
            let text = match &action {
                Action::Rename { path } => display_name(path),
                _ => String::new(),
            };
            app.sidebar.menu = None;
            app.sidebar.edit = Some(Edit {
                action,
                text,
                error: None,
            });
            return Task::batch([
                iced::widget::operation::focus(NAME_INPUT),
                iced::widget::operation::select_all(NAME_INPUT),
            ]);
        }
        SideMsg::Text(t) => {
            if let Some(e) = &mut app.sidebar.edit {
                e.text = t;
                e.error = None;
            }
        }
        SideMsg::Commit => commit(app),
        SideMsg::Delete(path) => {
            app.sidebar.menu = None;
            app.sidebar.deleting = Some(path);
        }
        SideMsg::ConfirmDelete(yes) => {
            if let Some(path) = app.sidebar.deleting.take().filter(|_| yes) {
                delete(app, &path);
            }
        }
        SideMsg::ToggleHidden => app.sidebar.hidden = !app.sidebar.hidden,
    }
    Task::none()
}

/// A request shows without `.toml`, a folder as it is.
fn display_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.strip_suffix(".toml")
        .map(str::to_string)
        .unwrap_or(name)
}

fn commit(app: &mut App) {
    let Some(edit) = &app.sidebar.edit else {
        return;
    };
    let (action, name) = (edit.action.clone(), edit.text.clone());
    let result = match &action {
        Action::NewRequest { dir } => workspace::file_name(&name, true).and_then(|f| {
            let path = dir.join(f);
            if path.exists() {
                return Err(format!("{} already exists", display_name(&path)));
            }
            // The file is written by the first Save, once the request has a URL.
            app.open_tab(Some(path), &doc::Opened::Missing);
            Ok(())
        }),
        Action::NewFolder { dir } => workspace::file_name(&name, false).and_then(|f| {
            let path = dir.join(f);
            std::fs::create_dir(&path).map_err(|e| format!("cannot create {}: {e}", path.display()))
        }),
        Action::Rename { path } => {
            let request = path.is_file();
            workspace::file_name(&name, request).and_then(|f| {
                let to = path.with_file_name(f);
                if to == *path {
                    return Ok(());
                }
                if to.exists() {
                    return Err(format!("{} already exists", display_name(&to)));
                }
                std::fs::rename(path, &to)
                    .map_err(|e| format!("cannot rename {}: {e}", path.display()))?;
                moved(app, path, &to);
                Ok(())
            })
        }
    };
    match result {
        Ok(()) => {
            app.sidebar.edit = None;
            rescan(app);
        }
        Err(e) => {
            if let Some(edit) = &mut app.sidebar.edit {
                edit.error = Some(e);
            }
        }
    }
}

/// Open tabs follow a renamed file, or files in a renamed folder.
fn moved(app: &mut App, from: &Path, to: &Path) {
    for d in &mut app.docs {
        let Some(file) = &d.file else { continue };
        if let Ok(rest) = file.strip_prefix(from) {
            let new = if rest.as_os_str().is_empty() {
                to.to_path_buf()
            } else {
                to.join(rest)
            };
            d.label = new
                .file_name()
                .map_or(d.label.clone(), |n| n.to_string_lossy().into_owned());
            d.file = Some(new);
        }
    }
    if app.sidebar.collapsed.remove(from) {
        app.sidebar.collapsed.insert(to.to_path_buf());
    }
}

/// A request file goes, with any tab showing it. A folder goes only when it is
/// empty, so no request is lost by one click.
fn delete(app: &mut App, path: &Path) {
    let result = if path.is_dir() {
        std::fs::remove_dir(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::DirectoryNotEmpty => {
                format!(
                    "{} is not empty. Delete its requests first.",
                    display_name(path)
                )
            }
            _ => format!("cannot delete {}: {e}", path.display()),
        })
    } else {
        std::fs::remove_file(path).map_err(|e| format!("cannot delete {}: {e}", path.display()))
    };
    match result {
        Ok(()) => {
            let gone: Vec<_> = app
                .docs
                .iter()
                .filter(|d| d.file.as_deref() == Some(path))
                .map(|d| d.id)
                .collect();
            for id in gone {
                app.close(id);
            }
            rescan(app);
        }
        Err(e) => app.notice = Some(e),
    }
}

pub fn rescan(app: &mut App) {
    if let Some(w) = &mut app.workspace {
        w.rescan();
    }
}

/// The folder new entries go into: the open request's folder, or the root.
fn target_dir(app: &App, root: &Path) -> PathBuf {
    app.doc()
        .and_then(|d| d.file.as_deref())
        .and_then(Path::parent)
        .filter(|dir| dir.starts_with(root) && dir.is_dir())
        .map_or_else(|| root.to_path_buf(), Path::to_path_buf)
}

pub fn view<'a>(app: &'a App, w: &'a Workspace) -> Element<'a, Msg> {
    let side = |m| Msg::Side(m);
    let dir = target_dir(app, &w.root);
    let title = w.root.file_name().map_or_else(
        || w.root.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let header = row![
        text(title).size(12).font(style::MEDIUM).color(style::MUTED),
        Space::new().width(Length::Fill),
        icon(
            "+ Request",
            side(SideMsg::Start(Action::NewRequest { dir: dir.clone() }))
        ),
        icon("+ Folder", side(SideMsg::Start(Action::NewFolder { dir }))),
    ]
    .spacing(4)
    .align_y(Alignment::Center);

    let mut list = column![].spacing(1);
    if let Some(edit) = &app.sidebar.edit {
        if let Action::NewRequest { dir } | Action::NewFolder { dir } = &edit.action {
            if *dir == w.root {
                list = list.push(edit_row(edit, 0));
            }
        }
    }
    for node in &w.tree {
        list = push_node(app, list, node, 0);
    }
    if w.tree.is_empty() && app.sidebar.edit.is_none() {
        list = list.push(
            text("No requests yet. Use + Request.")
                .size(12)
                .color(style::FAINT),
        );
    }
    let mut col = column![header, scrollable(list).height(Length::Fill)].spacing(8);
    if let Some(path) = &app.sidebar.deleting {
        col = col.push(confirm_delete(path));
    }
    container(col)
        .padding(10)
        .width(WIDTH)
        .height(Length::Fill)
        .style(style::panel)
        .into()
}

fn push_node<'a>(
    app: &'a App,
    mut list: iced::widget::Column<'a, Msg>,
    node: &'a Node,
    depth: usize,
) -> iced::widget::Column<'a, Msg> {
    let renaming = app.sidebar.edit.as_ref().filter(|e| {
        e.action
            == Action::Rename {
                path: node.path.clone(),
            }
    });
    if let Some(edit) = renaming {
        list = list.push(edit_row(edit, depth));
    } else {
        list = list.push(entry(app, node, depth));
    }
    if app.sidebar.menu.as_deref() == Some(&node.path) {
        list = list.push(menu(node, depth));
    }
    if let Kind::Folder(children) = &node.kind {
        if !app.sidebar.collapsed.contains(&node.path) {
            if let Some(edit) = &app.sidebar.edit {
                if let Action::NewRequest { dir } | Action::NewFolder { dir } = &edit.action {
                    if *dir == node.path {
                        list = list.push(edit_row(edit, depth + 1));
                    }
                }
            }
            for child in children {
                list = push_node(app, list, child, depth + 1);
            }
        }
    }
    list
}

fn entry<'a>(app: &'a App, node: &'a Node, depth: usize) -> Element<'a, Msg> {
    let (glyph, msg, open) = match node.kind {
        Kind::Folder(_) => {
            let collapsed = app.sidebar.collapsed.contains(&node.path);
            (
                if collapsed { "▸" } else { "▾" },
                SideMsg::Toggle(node.path.clone()),
                false,
            )
        }
        Kind::Request => (
            "",
            SideMsg::Open(node.path.clone()),
            app.doc().and_then(|d| d.file.as_deref()) == Some(&node.path),
        ),
    };
    let caption = row![
        text(glyph).size(11).color(style::FAINT).width(10),
        text(&node.name).size(13),
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    let b = button(caption)
        .padding(Padding {
            left: 6.0 + INDENT * depth as f32,
            ..Padding::from([4, 6])
        })
        .width(Length::Fill)
        .on_press(Msg::Side(msg))
        .style(move |theme, status| style::tab(theme, status, open));
    mouse_area(b)
        .on_right_press(Msg::Side(SideMsg::Menu(Some(node.path.clone()))))
        .into()
}

/// The actions for one entry, shown under it after a right click.
fn menu(node: &Node, depth: usize) -> Element<'_, Msg> {
    let side = |m| Msg::Side(m);
    let path = node.path.clone();
    let mut actions = column![
        row![
            icon(
                "Rename",
                side(SideMsg::Start(Action::Rename { path: path.clone() }))
            ),
            icon("Delete", side(SideMsg::Delete(path.clone()))),
            icon("Close", side(SideMsg::Menu(None))),
        ]
        .spacing(4)
    ]
    .spacing(2);
    if let Kind::Folder(_) = node.kind {
        actions = actions.push(
            row![
                icon(
                    "+ Request",
                    side(SideMsg::Start(Action::NewRequest { dir: path.clone() }))
                ),
                icon(
                    "+ Folder",
                    side(SideMsg::Start(Action::NewFolder { dir: path }))
                ),
            ]
            .spacing(4),
        );
    }
    container(actions)
        .padding(Padding::ZERO.left(INDENT * (depth as f32 + 1.0)))
        .into()
}

fn edit_row(edit: &Edit, depth: usize) -> Element<'_, Msg> {
    let placeholder = match edit.action {
        Action::NewRequest { .. } => "Request name",
        Action::NewFolder { .. } => "Folder name",
        Action::Rename { .. } => "New name",
    };
    let mut col = column![
        text_input(placeholder, &edit.text)
            .id(NAME_INPUT)
            .on_input(|t| Msg::Side(SideMsg::Text(t)))
            .on_submit(Msg::Side(SideMsg::Commit))
            .size(13)
            .padding([4, 6])
            .style(style::input)
    ]
    .spacing(2);
    if let Some(e) = &edit.error {
        col = col.push(text(e).size(11).color(style::DANGER));
    }
    container(col)
        .padding(Padding::ZERO.left(INDENT * depth as f32))
        .into()
}

fn confirm_delete(path: &Path) -> Element<'_, Msg> {
    column![
        text(format!("Delete {}?", display_name(path)))
            .size(12)
            .color(style::WARNING),
        row![
            button(text("Delete").size(12))
                .padding([4, 10])
                .on_press(Msg::Side(SideMsg::ConfirmDelete(true)))
                .style(style::stop),
            button(text("Keep").size(12))
                .padding([4, 10])
                .on_press(Msg::Side(SideMsg::ConfirmDelete(false)))
                .style(style::neutral),
        ]
        .spacing(6),
    ]
    .spacing(6)
    .into()
}

fn icon(caption: &str, msg: Msg) -> Element<'_, Msg> {
    button(text(caption).size(11))
        .padding([2, 6])
        .on_press(msg)
        .style(style::close)
        .into()
}
