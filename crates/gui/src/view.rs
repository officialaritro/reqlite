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
use reqlite_gui::present::{Token, human_size, json_tokens};

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

/// No tab is open.
fn empty(workspace: bool) -> Element<'static, Msg> {
    let hint = if workspace {
        format!("Choose a request in the sidebar, or press {MOD}N for a new one.")
    } else {
        format!("Press {MOD}N for a new request.")
    };
    container(
        column![
            text("No request is open.").size(13).color(style::MUTED),
            text(hint).size(12).color(style::FAINT),
        ]
        .spacing(6)
        .align_x(Alignment::Center),
    )
    .center(Length::Fill)
    .into()
}

fn request_bar(doc: &Doc) -> Element<'_, Msg> {
    let action = if doc.running() {
        button(label("Cancel", "Esc")).on_press(Msg::Cancel)
    } else {
        button(label("Send", &format!("{MOD}↵"))).on_press(Msg::Send)
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
        text_input("https://", &doc.url)
            .id(URL)
            .on_input(Msg::Url)
            .on_submit(Msg::Send)
            .font(style::MONO)
            .size(13)
            .padding([8, 12])
            .style(style::input),
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
            .padding([5, 10])
            .on_press(Msg::Section(s))
            .style(move |theme, status| style::tab(theme, status, active))
    };
    let count = |n: usize| if n == 0 { String::new() } else { n.to_string() };
    let sections = row![
        tab(Section::Query, "Query", count(doc.counts.query)),
        tab(Section::Headers, "Headers", count(doc.counts.headers)),
        tab(
            Section::Body,
            "Body",
            if doc.counts.body {
                "●".into()
            } else {
                String::new()
            }
        ),
    ]
    .spacing(4);
    let (content, placeholder, on): (_, _, fn(_) -> Msg) = match doc.section {
        Section::Query => (&doc.query, "name: value, one per line", Msg::Query),
        Section::Headers => (&doc.headers, "Name: value, one per line", Msg::Headers),
        Section::Body => (&doc.body, "Request body", Msg::Body),
    };
    let editor = text_editor(content)
        .placeholder(placeholder)
        .key_binding(super::editor_keys)
        .on_action(on)
        .font(style::MONO)
        .size(13)
        .padding(10)
        .height(Length::Fill)
        .style(style::editor);
    panel(column![sections, editor].spacing(8))
}

fn response_pane(doc: &Doc) -> container::Container<'_, Msg> {
    let mut header = row![status(doc), Space::new().width(Length::Fill)]
        .spacing(10)
        .align_y(Alignment::Center)
        .height(28);
    if let Some(v) = doc.shown() {
        header = header.push(
            text(format!(
                "line {} of {}",
                v.top + 1,
                v.doc.line_count().max(1)
            ))
            .size(12)
            .color(style::FAINT),
        );
    }
    let body: Element<'_, Msg> = match (&doc.send, doc.shown()) {
        (Send::Finished(Err(e)), _) => message(e, style::DANGER),
        (_, Some(v)) => viewer(v, doc.motion.reveal.interpolate(0.0, 1.0, doc.motion.now)),
        (Send::Running(_), None) => message("Sending…", pulse(doc)),
        (Send::Cancelled, None) => message("Cancelled.", style::MUTED),
        _ => hint(),
    };
    panel(column![header, divider(), body].spacing(8))
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
        let pretty = doc.is_pretty();
        let body = column(lines.into_iter().map(|l| line(l, pretty, fade)));
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

fn line(l: String, json: bool, fade: f32) -> Element<'static, Msg> {
    if !json {
        return text(l)
            .color(style::TEXT.scale_alpha(fade))
            .font(style::MONO)
            .size(13)
            .line_height(iced::Pixels(LINE_HEIGHT))
            .wrapping(text::Wrapping::None)
            .into();
    }
    let spans: Vec<text::Span<'static, (), iced::Font>> = json_tokens(&l)
        .into_iter()
        .map(|(piece, token)| {
            let color = match token {
                Token::Key => style::JSON_KEY,
                Token::String => style::JSON_STRING,
                Token::Number => style::JSON_NUMBER,
                Token::Literal => style::JSON_LITERAL,
                Token::Punct => style::MUTED,
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
    let mut bar = row![env];
    if let Some(n) = &app.notice {
        bar = bar.push(text(n).size(12).color(style::WARNING));
    }
    bar.push(Space::new().width(Length::Fill))
        .push(
            text(format!(
                "{MOD}↵ send · {MOD}S save · {MOD}L URL · {MOD}1–3 sections · {MOD}N new · {MOD}Y history · Ctrl+Tab next tab · Esc cancel"
            ))
            .size(12)
            .color(style::FAINT),
        )
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
