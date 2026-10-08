//! What the window shows, as a function of [`App`].
//!
//! ```text
//!  title row (macOS)    file name, unsaved dot
//!  sidebar | tab strip  the workspace tree beside one tab per open request
//!  request bar          method · URL · Send · Save
//!  split                request sections | response   (stacked below SPLIT_WIDTH)
//!  status bar           environment · notice · shortcuts
//! ```

use super::{App, Doc, LINE_HEIGHT, Msg, Section, Send, Summary, URL, Viewer};
use crate::scrollbar::scrollbar;
use crate::style;
use iced::widget::{
    Space, button, column, container, mouse_area, pick_list, responsive, rich_text, row,
    scrollable, span, text, text_editor, text_input,
};
use iced::{Alignment, Color, Element, Length, Padding, mouse};
use reqlite_gui::present::{Token, human_size, json_tokens, markup_tokens};
use reqlite_viewer::Kind;

/// Below this width of the area right of the sidebar, the response moves under
/// the request.
const SPLIT_WIDTH: f32 = 680.0;
/// macOS draws the traffic lights over the content, left of this inset.
const TRAFFIC_LIGHTS: f32 = 78.0;
/// The macOS title bar height, so the name lines up with the traffic lights.
const TITLE_HEIGHT: f32 = 28.0;

#[cfg(target_os = "macos")]
const MOD: &str = "⌘";
#[cfg(not(target_os = "macos"))]
const MOD: &str = "Ctrl+";

pub fn view(app: &App) -> Element<'_, Msg> {
    let mut outer = column![].spacing(10).padding(Padding {
        top: if super::TITLE_ROW { 0.0 } else { 12.0 },
        ..Padding::new(12.0)
    });
    if super::TITLE_ROW {
        outer = outer.push(title_row(app.doc()));
    }
    let main = column![main(app), status_bar(app)].spacing(10);
    let left: Option<Element<'_, Msg>> = match (app.left_panel(), &app.workspace) {
        (Some(super::Panel::Files), Some(w)) => Some(crate::sidebar::view(app, w)),
        (Some(super::Panel::History), _) => Some(crate::history::view(app)),
        _ => None,
    };
    match left {
        Some(left) => {
            let side = column![panel_switch(app), left].spacing(6);
            outer.push(row![side, main].spacing(10)).into()
        }
        None => outer.push(main).into(),
    }
}

/// Files and History, above the left panel.
fn panel_switch(app: &App) -> Element<'_, Msg> {
    let choice = |p: super::Panel, name: &'static str| {
        let active = app.panel == p;
        button(text(name).size(12))
            .padding([3, 10])
            .on_press(Msg::Panel(p))
            .style(move |t, s| style::tab(t, s, active))
    };
    let mut r = row![].spacing(4);
    if app.workspace.is_some() {
        r = r.push(choice(super::Panel::Files, "Files"));
    }
    r.push(choice(super::Panel::History, "History"))
        .push(Space::new().width(Length::Fill))
        .push(
            button(text("Open…").size(11))
                .padding([2, 6])
                .on_press(Msg::OpenFolder)
                .style(style::close),
        )
        .width(crate::sidebar::WIDTH)
        .into()
}

/// Everything right of the sidebar, above the status bar.
fn main(app: &App) -> Element<'_, Msg> {
    let mut page = column![].spacing(10);
    if !app.docs.is_empty() {
        page = page.push(tab_strip(app));
    }
    if let Some(name) = app
        .closing
        .and_then(|id| app.docs.iter().find(|d| d.id == id))
        .map(|d| d.label.as_str())
    {
        page = page.push(discard_prompt(name));
    }
    let Some(doc) = app.doc() else {
        return page
            .push(container(empty(app.workspace.is_some())).height(Length::Fill))
            .into();
    };
    page = page.push(request_bar(doc));
    if let Some(e) = &doc.open_error {
        page = page.push(banner(e));
    }
    page.push(responsive(move |size| {
        if size.width >= SPLIT_WIDTH {
            row![
                request_pane(doc).width(Length::FillPortion(2)),
                response_pane(doc).width(Length::FillPortion(3)),
            ]
            .spacing(10)
            .into()
        } else {
            column![request_pane(doc).height(260), response_pane(doc)]
                .spacing(10)
                .into()
        }
    }))
    .into()
}

fn title_row(doc: Option<&Doc>) -> Element<'_, Msg> {
    let mut name = row![
        text(doc.map_or("Reqlite", |d| d.label.as_str()))
            .size(13)
            .font(style::MEDIUM)
            .color(style::MUTED)
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if doc.is_some_and(Doc::unsaved) {
        name = name.push(text("●").size(9).color(style::ACCENT));
    }
    if doc.is_some_and(|d| d.open_error.is_some()) {
        name = name.push(text("cannot save").size(12).color(style::DANGER));
    }
    container(name)
        .padding(Padding::ZERO.left(TRAFFIC_LIGHTS - 12.0))
        .height(TITLE_HEIGHT)
        .align_y(Alignment::Center)
        .into()
}

/// One tab per open request. The shown one is raised. A dot marks unsaved
/// changes, and × closes the tab.
fn tab_strip(app: &App) -> Element<'_, Msg> {
    let tabs = row(app.docs.iter().enumerate().map(|(i, d)| {
        let active = i == app.active;
        let mut caption = row![text(&d.label).size(12)]
            .spacing(6)
            .align_y(Alignment::Center);
        if d.unsaved() {
            caption = caption.push(text("●").size(8).color(style::ACCENT));
        }
        let close = button(text("×").size(13))
            .padding([0, 4])
            .on_press(Msg::Close(d.id))
            .style(style::close);
        button(row![caption, close].spacing(8).align_y(Alignment::Center))
            .padding([4, 10])
            .on_press(Msg::Select(i))
            .style(move |theme, status| style::tab(theme, status, active))
            .into()
    }))
    .spacing(4);
    scrollable(tabs)
        .direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::new().width(0).scroller_width(0),
        ))
        .into()
}

fn discard_prompt(name: &str) -> Element<'_, Msg> {
    container(
        row![
            text(format!("{name} has unsaved changes."))
                .size(12)
                .color(style::WARNING),
            Space::new().width(Length::Fill),
            button(text("Discard").size(12))
                .padding([4, 10])
                .on_press(Msg::Discard(true))
                .style(style::stop),
            button(text("Keep").size(12))
                .padding([4, 10])
                .on_press(Msg::Discard(false))
                .style(style::neutral),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([6, 12])
    .width(Length::Fill)
    .style(style::warning_banner)
    .into()
}

/// No tab is open. With no workspace, this is the first-start window.
fn empty(workspace: bool) -> Element<'static, Msg> {
    let (title, hint) = if workspace {
        (
            "No request is open.",
            format!("Choose a request in the sidebar, or press {MOD}N for a new one."),
        )
    } else {
        (
            "Open a folder of request files, or start a new request.",
            format!("{MOD}O open a folder · {MOD}N new request"),
        )
    };
    let mut col = column![
        text(title).size(13).color(style::MUTED),
        text(hint).size(12).color(style::FAINT),
    ]
    .spacing(6)
    .align_x(Alignment::Center);
    if !workspace {
        col = col.push(
            row![
                button(text("Open folder…").size(13))
                    .padding([6, 14])
                    .on_press(Msg::OpenFolder)
                    .style(style::action(0.0)),
                button(text("New request").size(13))
                    .padding([6, 14])
                    .on_press(Msg::New)
                    .style(style::neutral),
            ]
            .spacing(8),
        );
    }
    container(col).center(Length::Fill).into()
}

fn request_bar(doc: &Doc) -> Element<'_, Msg> {
    let action = match (doc.running(), doc.live.is_some()) {
        (true, true) => button(label("Disconnect", "Esc")).on_press(Msg::Cancel),
        (true, false) => button(label("Cancel", "Esc")).on_press(Msg::Cancel),
        (false, _) if doc.opens_stream() => {
            button(label("Connect", &format!("{MOD}↵"))).on_press(Msg::Send)
        }
        (false, _) => button(label("Send", &format!("{MOD}↵"))).on_press(Msg::Send),
    };
    let action = action.style(style::action(doc.motion.running.interpolate(
        0.0,
        1.0,
        doc.motion.now,
    )));
    let save = button(text("Save").size(13))
        .padding([8, 14])
        .on_press_maybe(doc.unsaved().then_some(Msg::Save))
        .style(style::neutral);
    row![
        pick_list(&doc.methods[..], Some(&doc.method), Msg::Method)
            .font(style::MONO)
            .text_size(13)
            .padding([8, 12])
            .width(110)
            .style(style::method_picker(style::method_color(&doc.method)))
            .menu_style(style::method_menu),
        crate::guard::guard(
            text_input("https://", &doc.url)
                .id(URL)
                .on_input(Msg::Url)
                .on_submit(Msg::Send)
                .font(style::MONO)
                .size(13)
                .padding([8, 12])
                .style(style::input),
        )
        .on_paste(crate::guard::PasteProbe::curl),
        action.padding([8, 14]),
        save,
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

/// A button caption with its shortcut in a fainter tone.
fn label<'a>(name: &'a str, key: &str) -> Element<'a, Msg> {
    row![
        text(name).size(13).font(style::MEDIUM),
        text(key.to_string()).size(11).color(style::white(0.6)),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

fn banner(message: &str) -> Element<'_, Msg> {
    container(
        text(message)
            .size(12)
            .font(style::MONO)
            .color(style::DANGER),
    )
    .padding([8, 12])
    .width(Length::Fill)
    .style(style::danger_banner)
    .into()
}

fn request_pane(doc: &Doc) -> container::Container<'_, Msg> {
    let tab = |s: Section, name: &'static str, badge: String| {
        let active = doc.section == s;
        let mut caption = row![text(name).size(13)]
            .spacing(6)
            .align_y(Alignment::Center);
        if !badge.is_empty() {
            caption = caption.push(text(badge).size(11).color(if active {
                style::ACCENT
            } else {
                style::FAINT
            }));
        }
        button(caption)
            .padding([5, 8])
            .on_press(Msg::Section(s))
            .style(move |theme, status| style::tab(theme, status, active))
    };
    let count = |n: usize| if n == 0 { String::new() } else { n.to_string() };
    let dot = |on: bool| if on { "●".to_string() } else { String::new() };
    let sections = row![
        tab(Section::Query, "Query", count(doc.counts.query)),
        tab(Section::Headers, "Headers", count(doc.counts.headers)),
        tab(Section::Body, "Body", dot(doc.counts.body)),
        tab(Section::Auth, "Auth", dot(doc.counts.auth)),
        tab(Section::Tests, "Tests", count(doc.counts.tests)),
    ]
    .spacing(4)
    // A narrow pane puts the last tabs on a second line rather than hiding them.
    .wrap()
    .vertical_spacing(4);
    let editor = |content, placeholder, on: fn(text_editor::Action) -> Msg| {
        text_editor(content)
            .placeholder(placeholder)
            .key_binding(super::editor_keys)
            .on_action(on)
            .font(style::MONO)
            .size(13)
            .padding(10)
            .height(Length::Fill)
            .style(style::editor)
    };
    let content: Element<'_, Msg> = match doc.section {
        Section::Query => editor(&doc.query, "name: value, one per line", Msg::Query).into(),
        Section::Headers => editor(&doc.headers, "Name: value, one per line", Msg::Headers).into(),
        Section::Body => body_editor(doc, editor),
        Section::Auth => auth_editor(doc),
        Section::Tests => editor(
            &doc.tests,
            "One check per line, such as\nstatus == 200\njson $.id exists\ncapture id = json $.id",
            Msg::Tests,
        )
        .into(),
    };
    panel(column![sections, content].spacing(8))
}

/// A small picker in the request pane, styled like the method picker.
fn picker<'a, T: ToString + PartialEq + Clone + 'a>(
    options: &'a [T],
    selected: T,
    on: impl Fn(T) -> Msg + 'a,
) -> Element<'a, Msg> {
    pick_list(options, Some(selected), on)
        .text_size(12)
        .padding([4, 10])
        .style(style::method_picker(style::TEXT))
        .menu_style(style::method_menu)
        .into()
}

fn field<'a>(
    placeholder: &'a str,
    value: &'a str,
    on: impl Fn(String) -> Msg + 'a,
) -> Element<'a, Msg> {
    crate::guard::guard(
        text_input(placeholder, value)
            .on_input(on)
            .font(style::MONO)
            .size(13)
            .padding([8, 10])
            .style(style::input),
    )
    .into()
}

fn note(t: &str) -> Element<'_, Msg> {
    text(t).size(12).color(style::FAINT).into()
}

fn body_editor<'a, E: Into<Element<'a, Msg>>>(
    doc: &'a Doc,
    editor: impl Fn(&'a text_editor::Content, &'static str, fn(text_editor::Action) -> Msg) -> E,
) -> Element<'a, Msg> {
    use reqlite_gui::draft::BodyKind as K;
    let kind = picker(&K::ALL, doc.body_kind, Msg::BodyKind);
    let below: Element<'_, Msg> = match doc.body_kind {
        K::Text => editor(&doc.body, "Request body, sent as it is", Msg::Body).into(),
        K::Json => editor(&doc.body, "{\"name\": \"value\"}", Msg::Body).into(),
        K::Form => editor(&doc.body, "name: value, one per line", Msg::Body).into(),
        K::Multipart => editor(
            &doc.body,
            "name: value\nfile: @path/to/file\nimage: @a.png;type=image/png",
            Msg::Body,
        )
        .into(),
        K::Graphql => column![
            editor(&doc.body, "query { user(id: 1) { name } }", Msg::Body).into(),
            row![
                note("Variables, as JSON"),
                Space::new().width(Length::Fill),
                button(text("Schema").size(12))
                    .padding([3, 10])
                    .on_press(Msg::Schema)
                    .style(|t, s| style::tab(t, s, false)),
            ]
            .align_y(Alignment::Center),
            editor(&doc.variables, "{\"id\": \"{{user_id}}\"}", Msg::Variables).into(),
        ]
        .spacing(6)
        .into(),
        K::File => column![
            field(
                "path/to/file, from the request file's folder",
                &doc.body_file,
                Msg::BodyFile
            ),
            note("The file is read when the request is sent. Set Content-Type in Headers."),
        ]
        .spacing(6)
        .into(),
    };
    let hint = match doc.body_kind {
        K::Text => "",
        K::Json => "Adds Content-Type: application/json unless Headers sets one.",
        K::Form => "Sent URL-encoded, with its Content-Type.",
        K::Multipart => "@ marks a file part, read when the request is sent.",
        K::File => "",
        K::Graphql => "Sent as a JSON POST. Schema shows the server's types.",
    };
    column![
        row![kind, note(hint)]
            .spacing(10)
            .align_y(Alignment::Center),
        below
    ]
    .spacing(8)
    .into()
}

fn auth_editor(doc: &Doc) -> Element<'_, Msg> {
    use super::AuthField as F;
    use reqlite_format::{ClientAuth, Grant, KeyIn};
    use reqlite_gui::draft::AuthKind as K;
    let a = &doc.auth;
    let on = |f: F| move |v| Msg::Auth(f, v);
    let choice = |active: bool, name: &'static str, msg: Msg| {
        button(text(name).size(12))
            .padding([3, 10])
            .on_press(msg)
            .style(move |t, s| style::tab(t, s, active))
    };
    let fields: Element<'_, Msg> = match a.kind {
        K::None => note("Add an Authorization header in Headers, or pick an auth type."),
        K::Bearer => column![field(
            "Token, for example {{token}}",
            &a.token,
            on(F::Token)
        )]
        .into(),
        K::Basic => column![
            field("Username", &a.username, on(F::Username)),
            field(
                "Password, for example {{password}}",
                &a.password,
                on(F::Password)
            ),
        ]
        .spacing(6)
        .into(),
        K::ApiKey => {
            let place = |k: KeyIn, name| choice(a.key_in == k, name, Msg::KeyIn(k));
            column![
                row![
                    note("Send it in"),
                    place(KeyIn::Header, "Header"),
                    place(KeyIn::Query, "Query")
                ]
                .spacing(6)
                .align_y(Alignment::Center),
                field("Name, for example X-API-Key", &a.key_name, on(F::KeyName)),
                field(
                    "Value, for example {{api_key}}",
                    &a.key_value,
                    on(F::KeyValue)
                ),
            ]
            .spacing(6)
            .into()
        }
        K::OAuth2 => {
            let client = |c: ClientAuth, name| choice(a.client_auth == c, name, Msg::ClientAuth(c));
            // Several URLs look alike once filled, so each field keeps a label.
            let labeled = |label, placeholder, value, f| {
                column![note(label), field(placeholder, value, on(f))].spacing(2)
            };
            let endpoint = match a.grant {
                Grant::ClientCredentials => None,
                Grant::AuthorizationCode => Some(labeled(
                    "Authorization URL",
                    "https://id.example.com/authorize",
                    &a.auth_url,
                    F::AuthUrl,
                )),
                Grant::DeviceCode => Some(labeled(
                    "Device authorization URL",
                    "https://id.example.com/device",
                    &a.device_url,
                    F::DeviceUrl,
                )),
            };
            let secret = match a.grant {
                Grant::ClientCredentials => "{{client_secret}}",
                _ => "Only if the client has one",
            };
            column![
                row![note("Grant"), picker(&Grant::ALL, a.grant, Msg::Grant)]
                    .spacing(6)
                    .align_y(Alignment::Center),
                labeled(
                    "Token URL",
                    "https://id.example.com/token",
                    &a.token_url,
                    F::TokenUrl
                ),
            ]
            .push(endpoint)
            .push(labeled("Client ID", "", &a.client_id, F::ClientId))
            .push(labeled(
                "Client secret",
                secret,
                &a.client_secret,
                F::ClientSecret,
            ))
            .push(labeled("Scope", "read write", &a.scope, F::Scope))
            .push(
                row![
                    note("Send the secret in"),
                    client(ClientAuth::Basic, "Header"),
                    client(ClientAuth::Body, "Body")
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .spacing(8)
            .into()
        }
    };
    let hint = match a.kind {
        K::None => "",
        K::OAuth2 => "Tokens stay in the OS keychain, never in the file.",
        _ => "Use a {{secret}} so the value stays out of the file.",
    };
    column![
        row![picker(&K::ALL, a.kind, Msg::AuthKind), note(hint)]
            .spacing(10)
            .align_y(Alignment::Center),
        fields
    ]
    .spacing(8)
    .height(Length::Fill)
    .into()
}

/// A connection's log, newest at the bottom, and for a WebSocket a message box.
fn live_pane<'a>(doc: &'a Doc, live: &'a crate::doc::Live) -> container::Container<'a, Msg> {
    use reqlite_gui::live::Dir;
    let state = match (live.open, doc.running()) {
        (true, _) => pill("Connected", style::status_color(200)),
        (false, true) => text("Connecting…").size(13).color(style::MUTED).into(),
        (false, false) => pill("Closed", style::MUTED),
    };
    let mut count = format!("{} messages", live.log.len());
    if live.log.dropped() > 0 {
        count.push_str(&format!(", {} older ones not kept", live.log.dropped()));
    }
    let header = row![
        state,
        text(count).size(12).color(style::FAINT),
        Space::new().width(Length::Fill)
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .height(28);
    let rows = live.log.entries().map(|e| {
        let (mark, color) = match e.dir {
            Dir::In => ("←", style::JSON_KEY),
            Dir::Out => ("→", style::status_color(200)),
            Dir::Info => ("·", style::FAINT),
        };
        let mut line = row![
            text(format!("{:>8.3}s", e.at.as_secs_f64()))
                .size(12)
                .font(style::MONO)
                .color(style::FAINT),
            text(mark).size(13).color(color).width(14),
        ]
        .spacing(8);
        if let Some(name) = &e.name {
            line = line.push(text(name).size(12).font(style::MONO).color(style::ACCENT));
        }
        let body = text(&e.text).size(13).font(style::MONO);
        line.push(if e.dir == Dir::Info {
            body.color(style::MUTED)
        } else {
            body
        })
        .into()
    });
    let log = scrollable(column(rows).spacing(4).width(Length::Fill))
        .anchor_bottom()
        .spacing(4)
        .width(Length::Fill)
        .height(Length::Fill);
    let mut col = column![header, divider(), log].spacing(8);
    if live.websocket {
        let can_send = live.open && !live.message.is_empty();
        col = col.push(
            row![
                text_input("Message, text or JSON", &live.message)
                    .on_input(Msg::LiveMessage)
                    .on_submit(Msg::LiveSend)
                    .font(style::MONO)
                    .size(13)
                    .padding([8, 10])
                    .style(style::input),
                button(text("Send").size(13))
                    .padding([7, 14])
                    .on_press_maybe(can_send.then_some(Msg::LiveSend))
                    .style(style::neutral),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    panel(col)
}

fn response_pane(doc: &Doc) -> container::Container<'_, Msg> {
    use crate::doc::ResponseTab;
    if let Some(live) = &doc.live {
        return live_pane(doc, live);
    }
    let mut header = row![status(doc), Space::new().width(Length::Fill)]
        .spacing(10)
        .align_y(Alignment::Center)
        .height(28);
    let shown = doc.shown();
    if shown.is_some() {
        let tab = |t: ResponseTab, name: String| {
            let active = doc.response_tab == t;
            button(text(name).size(12))
                .padding([3, 10])
                .on_press(Msg::ResponseTab(t))
                .style(move |th, st| style::tab(th, st, active))
        };
        header = header.push(tab(ResponseTab::Body, "Body".into())).push(tab(
            ResponseTab::Headers,
            format!("Headers {}", doc.response_headers.len()),
        ));
        if let Some(c) = doc.checks.as_ref().filter(|c| !c.outcomes.is_empty()) {
            let passed = c.outcomes.len() - c.failed();
            header = header.push(tab(
                ResponseTab::Tests,
                format!("Tests {passed}/{}", c.outcomes.len()),
            ));
        }
    }
    if let (Some(v), ResponseTab::Body) = (shown, doc.response_tab) {
        // Short, so it fits next to three response tabs.
        let at = text(format!("{}/{}", v.top + 1, v.doc.line_count().max(1)))
            .size(12)
            .font(style::MONO)
            .color(style::FAINT);
        let tip = container(text("Top line shown / lines").size(12))
            .padding([3, 8])
            .style(container::rounded_box);
        header = header.push(iced::widget::tooltip(
            at,
            tip,
            iced::widget::tooltip::Position::Bottom,
        ));
    }
    let body: Element<'_, Msg> = match (&doc.send, shown) {
        (Send::Finished(Err(e)), _) => message(e, style::DANGER),
        (_, Some(_)) if doc.response_tab == ResponseTab::Headers => headers(&doc.response_headers),
        (_, Some(_)) if doc.response_tab == ResponseTab::Tests => checks(doc.checks.as_ref()),
        (_, Some(v)) => viewer(v, doc.motion.reveal.interpolate(0.0, 1.0, doc.motion.now)),
        (Send::Running(_), None) => message("Sending…", pulse(doc)),
        (Send::Cancelled, None) => message("Cancelled.", style::MUTED),
        _ => hint(),
    };
    panel(column![header, divider(), body].spacing(8))
}

/// The response headers, one per line, in the order the server sent them.
fn headers(list: &[(String, String)]) -> Element<'_, Msg> {
    let rows = list.iter().map(|(k, v)| {
        row![
            text(k)
                .size(13)
                .font(style::MONO)
                .color(style::JSON_KEY)
                .width(Length::FillPortion(2)),
            text(v)
                .size(13)
                .font(style::MONO)
                .width(Length::FillPortion(5)),
        ]
        .spacing(12)
        .into()
    });
    scrollable(column(rows).spacing(4))
        .spacing(4)
        .height(Length::Fill)
        .into()
}

/// One row per assertion and capture: a mark, the line, and why it failed.
fn checks(checked: Option<&reqlite_engine::check::Checked>) -> Element<'_, Msg> {
    let rows = checked.into_iter().flat_map(|c| &c.outcomes).map(|o| {
        let (mark, color) = if o.pass {
            ("✓", style::status_color(200))
        } else {
            ("✕", style::DANGER)
        };
        let mut line = row![
            text(mark).size(13).color(color).width(16),
            text(&o.text).size(13).font(style::MONO),
        ]
        .spacing(8);
        if !o.pass {
            line = line.push(
                text(&o.detail)
                    .size(13)
                    .font(style::MONO)
                    .color(style::MUTED),
            );
        }
        line.into()
    });
    scrollable(column(rows).spacing(6))
        .spacing(4)
        .height(Length::Fill)
        .into()
}

/// "Sending…" breathes once every 1.2 s, unless motion is reduced.
fn pulse(doc: &Doc) -> Color {
    if doc.motion.reduced {
        return style::MUTED;
    }
    let t = doc
        .motion
        .now
        .saturating_duration_since(doc.motion.since)
        .as_secs_f32();
    let wave = 0.5 + 0.5 * (t * std::f32::consts::TAU / 1.2).cos();
    style::MUTED.scale_alpha(0.45 + 0.55 * wave)
}

/// The status pill and timing for the latest send.
fn status(doc: &Doc) -> Element<'_, Msg> {
    match &doc.send {
        Send::Idle => text("Response").size(13).color(style::MUTED).into(),
        Send::Running(_) => text("Sending…").size(13).color(pulse(doc)).into(),
        Send::Cancelled => text("Cancelled").size(13).color(style::MUTED).into(),
        Send::Finished(Err(_)) => pill("Error", style::DANGER),
        Send::Finished(Ok(s)) => row![
            pill(&s.status_line(), style::status_color(s.status)),
            text(format!("{} ms", s.elapsed.as_millis()))
                .size(12)
                .font(style::MONO)
                .color(style::MUTED),
            text(human_size(s.bytes))
                .size(12)
                .font(style::MONO)
                .color(style::MUTED),
        ]
        .spacing(12)
        .align_y(Alignment::Center)
        .into(),
    }
}

fn pill(caption: &str, color: Color) -> Element<'static, Msg> {
    container(
        text(caption.to_string())
            .size(12)
            .font(style::MEDIUM)
            .color(color),
    )
    .padding([3, 8])
    .style(move |_| style::pill(color))
    .into()
}

fn message(msg: &str, color: Color) -> Element<'_, Msg> {
    container(text(msg).size(13).font(style::MONO).color(color))
        .padding(4)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn hint() -> Element<'static, Msg> {
    container(
        column![
            text("Send the request to see the response here.")
                .size(13)
                .color(style::MUTED),
            text(format!("{MOD}↵"))
                .size(12)
                .font(style::MONO)
                .color(style::FAINT),
        ]
        .spacing(8)
        .align_x(Alignment::Center),
    )
    .center(Length::Fill)
    .into()
}

/// The visible lines only, with a line-number gutter. JSON gets colours.
/// `fade` runs from 0 to 1 as a new response comes in.
fn viewer(v: &Viewer, fade: f32) -> Element<'_, Msg> {
    let doc = v.doc.clone();
    let top = v.top;
    let lines = responsive(move |size| {
        let count = (size.height / LINE_HEIGHT).floor() as usize;
        let lines = doc
            .lines(top, count)
            .unwrap_or_else(|e| vec![format!("cannot read the response: {e}")]);
        // Geist Mono's advance is 0.6 em.
        let gutter = digits(doc.line_count()) as f32 * 13.0 * 0.6 + 12.0;
        let numbers = column((top + 1..=top + lines.len()).map(|n| {
            text(n.to_string())
                .font(style::MONO)
                .size(13)
                .line_height(iced::Pixels(LINE_HEIGHT))
                .color(style::white(0.22 * fade))
                .width(gutter)
                .align_x(Alignment::End)
                .into()
        }));
        let kind = doc.kind();
        let body = column(lines.into_iter().map(|l| line(l, kind, fade)));
        container(row![numbers, body].spacing(14))
            .padding(Padding::ZERO.top(4.0 * (1.0 - fade)))
            .clip(true)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    });
    let area = row![
        lines,
        scrollbar(v.doc.line_count(), v.top, LINE_HEIGHT, |top| {
            Msg::ScrollTo(top as f64)
        }),
    ]
    .height(Length::Fill);
    mouse_area(area)
        .on_scroll(|delta| {
            Msg::Scroll(match delta {
                mouse::ScrollDelta::Lines { y, .. } => (-y * 3.0).round() as i64,
                mouse::ScrollDelta::Pixels { y, .. } => (-y / LINE_HEIGHT).round() as i64,
            })
        })
        .into()
}

fn line(l: String, kind: Kind, fade: f32) -> Element<'static, Msg> {
    let tokens = match kind {
        Kind::Json => json_tokens(&l),
        Kind::Markup => markup_tokens(&l),
        Kind::Plain => Vec::new(),
    };
    if tokens.is_empty() {
        return text(l)
            .color(style::TEXT.scale_alpha(fade))
            .font(style::MONO)
            .size(13)
            .line_height(iced::Pixels(LINE_HEIGHT))
            .wrapping(text::Wrapping::None)
            .into();
    }
    let spans: Vec<text::Span<'static, (), iced::Font>> = tokens
        .into_iter()
        .map(|(piece, token)| {
            let color = match token {
                Token::Key | Token::Tag => style::JSON_KEY,
                Token::String => style::JSON_STRING,
                Token::Number => style::JSON_NUMBER,
                Token::Literal | Token::Attr => style::JSON_LITERAL,
                Token::Punct => style::MUTED,
                Token::Text => style::TEXT,
                Token::Comment => style::FAINT,
            };
            span(piece.to_string()).color(color.scale_alpha(fade))
        })
        .collect();
    rich_text(spans)
        .font(style::MONO)
        .size(13)
        .line_height(iced::Pixels(LINE_HEIGHT))
        .wrapping(text::Wrapping::None)
        .into()
}

fn digits(n: usize) -> usize {
    n.max(1).ilog10() as usize + 1
}

fn status_bar(app: &App) -> Element<'_, Msg> {
    let env = pick_list(
        app.env_choices(),
        Some(super::Env(app.env.clone())),
        Msg::PickEnv,
    )
    .font(style::MONO)
    .text_size(12)
    .padding([3, 8])
    .style(style::method_picker(if app.env.is_some() {
        style::TEXT
    } else {
        style::FAINT
    }))
    .menu_style(style::method_menu);
    // A notice takes the place of the shortcut hints, so the bar stays one line.
    let right = match &app.notice {
        Some(n) => text(n).size(12).color(style::WARNING),
        None => text(format!(
            "{MOD}↵ send · {MOD}S save · {MOD}L URL · {MOD}1–5 sections · {MOD}N new · {MOD}Y history · Ctrl+Tab next tab · Esc cancel"
        ))
        .size(12)
        .color(style::FAINT),
    };
    row![env, Space::new().width(Length::Fill), right]
        .spacing(16)
        .align_y(Alignment::Center)
        .into()
}

fn panel<'a>(content: impl Into<Element<'a, Msg>>) -> container::Container<'a, Msg> {
    container(content)
        .padding(10)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(style::panel)
}

fn divider() -> Element<'static, Msg> {
    container(Space::new().height(1))
        .width(Length::Fill)
        .style(|_| container::Style {
            background: Some(style::white(0.08).into()),
            ..container::Style::default()
        })
        .into()
}

impl Summary {
    fn status_line(&self) -> String {
        match reqwest::StatusCode::from_u16(self.status)
            .ok()
            .and_then(|c| c.canonical_reason())
        {
            Some(reason) => format!("{} {reason}", self.status),
            None => self.status.to_string(),
        }
    }
}
