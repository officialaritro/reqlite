//! A thin scrollbar for the virtual viewer. iced's own scrollbar belongs to
//! `scrollable`, which needs the whole content laid out. The viewer lays out
//! only the lines on screen, so it gets this one.
//!
//! Drag the thumb, or click the track to jump there. It redraws only when its
//! look changes: hover, drag, or a new position.

use crate::style;
use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Tree, tree};
use iced::advanced::{Clipboard, Shell, Widget, renderer};
use iced::{Border, Element, Event, Length, Rectangle, Size, Theme, mouse, window};
use reqlite_gui::present::{thumb, top_at};

const WIDTH: f32 = 12.0;
const INSET: f32 = 3.0;

pub struct Scrollbar<'a, Message> {
    lines: usize,
    top: usize,
    line_height: f32,
    on_scroll: Box<dyn Fn(usize) -> Message + 'a>,
}

pub fn scrollbar<'a, Message>(
    lines: usize,
    top: usize,
    line_height: f32,
    on_scroll: impl Fn(usize) -> Message + 'a,
) -> Scrollbar<'a, Message> {
    Scrollbar {
        lines,
        top,
        line_height,
        on_scroll: Box::new(on_scroll),
    }
}

#[derive(Default)]
struct State {
    /// While dragging: how far below the thumb's top edge it was grabbed.
    grab: Option<f32>,
    /// The look last drawn, to redraw only when it changes.
    drawn: Option<Look>,
}

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Idle,
    Hovered,
    Dragged,
}

impl<Message> Scrollbar<'_, Message> {
    fn visible(&self, bounds: Rectangle) -> usize {
        (bounds.height / self.line_height).floor() as usize
    }

    fn thumb(&self, bounds: Rectangle) -> Rectangle {
        let (offset, len) = thumb(bounds.height, self.lines, self.visible(bounds), self.top);
        Rectangle {
            x: bounds.x + INSET,
            y: bounds.y + offset,
            width: bounds.width - 2.0 * INSET,
            height: len,
        }
    }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Scrollbar<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(WIDTH), Length::Fill)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::atomic(limits, WIDTH, Length::Fill)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let bounds = layout.bounds();
        let to = |y: f32, grab: f32| {
            top_at(
                bounds.height,
                self.lines,
                self.visible(bounds),
                y - bounds.y - grab,
            )
        };
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(p) = cursor.position_over(bounds) {
                    let thumb = self.thumb(bounds);
                    let grab = if thumb.contains(p) {
                        p.y - thumb.y
                    } else {
                        // A click on the track centres the thumb there.
                        let grab = thumb.height / 2.0;
                        shell.publish((self.on_scroll)(to(p.y, grab)));
                        grab
                    };
                    state.grab = Some(grab);
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                if let Some(grab) = state.grab {
                    let top = to(position.y, grab);
                    if top != self.top {
                        shell.publish((self.on_scroll)(top));
                    }
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.grab = None;
            }
            _ => {}
        }

        let look = if state.grab.is_some() {
            Look::Dragged
        } else if cursor.is_over(bounds) {
            Look::Hovered
        } else {
            Look::Idle
        };
        if let Event::Window(window::Event::RedrawRequested(_)) = event {
            state.drawn = Some(look);
        } else if state.drawn.is_some_and(|drawn| drawn != look) {
            shell.request_redraw();
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        let bounds = layout.bounds();
        if self.visible(bounds) >= self.lines {
            return;
        }
        let state = tree.state.downcast_ref::<State>();
        let dragged = state.grab.is_some();
        let hovered = cursor.is_over(bounds);
        if hovered || dragged {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    border: Border {
                        radius: (WIDTH / 2.0).into(),
                        ..Border::default()
                    },
                    ..renderer::Quad::default()
                },
                style::white(0.04),
            );
        }
        let alpha = if dragged {
            0.34
        } else if hovered {
            0.26
        } else {
            0.16
        };
        renderer.fill_quad(
            renderer::Quad {
                bounds: self.thumb(bounds),
                border: Border {
                    radius: (WIDTH / 2.0 - INSET).into(),
                    ..Border::default()
                },
                ..renderer::Quad::default()
            },
            style::white(alpha),
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        if tree.state.downcast_ref::<State>().grab.is_some() {
            mouse::Interaction::Grabbing
        } else if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Grab
        } else {
            mouse::Interaction::default()
        }
    }
}

impl<'a, Message: 'a> From<Scrollbar<'a, Message>> for Element<'a, Message> {
    fn from(s: Scrollbar<'a, Message>) -> Self {
        Element::new(s)
    }
}
