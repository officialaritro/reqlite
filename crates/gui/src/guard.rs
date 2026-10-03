//! Keeps app shortcuts out of text inputs. macOS sends the character with a
//! Cmd key press, and iced's `text_input` types any character it gets except
//! for Cmd+C, X, V and A. So Cmd+S in the URL field would add an "s".
//!
//! [`guard`] wraps a widget and drops the key presses that are app shortcuts
//! before the widget sees them. The window still gets them, so the shortcut
//! itself works. Every other event and operation passes through.

use super::{Msg, shortcut};
use iced::advanced::clipboard;
use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::operation::Focusable;
use iced::advanced::widget::{Id, Operation, Tree, tree};
use iced::advanced::{Clipboard, Shell, Widget, overlay, renderer};
use iced::{Element, Event, Length, Rectangle, Size, Theme, Vector, keyboard, mouse};
use reqlite_gui::present::is_curl;

pub struct Guard<'a> {
    inner: Element<'a, Msg>,
    /// Looks at the clipboard text when a paste lands in the focused field.
    on_paste: Option<fn(&str) -> Option<Msg>>,
}

pub fn guard<'a>(inner: impl Into<Element<'a, Msg>>) -> Guard<'a> {
    Guard {
        inner: inner.into(),
        on_paste: None,
    }
}

impl Guard<'_> {
    /// When a paste (Cmd/Ctrl+V) lands in the focused field, `f` sees the whole
    /// clipboard text. `Some(msg)` takes the paste: the message is published
    /// and the field gets nothing. `None` leaves the paste to the field. The
    /// text is read here because the field drops line breaks from what it
    /// pastes, and a cURL command from a browser has them.
    pub(crate) fn on_paste(mut self, f: fn(&str) -> Option<Msg>) -> Self {
        self.on_paste = Some(f);
        self
    }
}

/// What the URL field does with a paste.
pub(crate) struct PasteProbe;

impl PasteProbe {
    /// A cURL command is imported. Anything else is left to the field.
    pub(crate) fn curl(text: &str) -> Option<Msg> {
        is_curl(text).then(|| Msg::PasteCurl(text.to_string()))
    }
}

/// Finds out whether the wrapped field has the keyboard focus.
struct FocusProbe(bool);

impl Operation for FocusProbe {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn focusable(&mut self, _id: Option<&Id>, _bounds: Rectangle, state: &mut dyn Focusable) {
        self.0 |= state.is_focused();
    }
}

impl Widget<Msg, Theme, iced::Renderer> for Guard<'_> {
    fn size(&self) -> Size<Length> {
        self.inner.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.inner.as_widget().size_hint()
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.inner)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.inner));
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::stateless()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.inner
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.inner
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Msg>,
        viewport: &Rectangle,
    ) {
        if let Event::Keyboard(keyboard::Event::KeyPressed {
            key,
            modifiers,
            physical_key,
            ..
        }) = event
        {
            if shortcut(key, *modifiers).is_some() {
                return;
            }
            let paste =
                modifiers.command() && !modifiers.alt() && key.to_latin(*physical_key) == Some('v');
            if let (true, Some(on_paste)) = (paste, self.on_paste) {
                let mut probe = FocusProbe(false);
                self.inner.as_widget_mut().operate(
                    &mut tree.children[0],
                    layout,
                    renderer,
                    &mut probe,
                );
                let msg = probe
                    .0
                    .then(|| clipboard.read(clipboard::Kind::Standard))
                    .flatten()
                    .and_then(|text| on_paste(&text));
                if let Some(msg) = msg {
                    shell.publish(msg);
                    shell.capture_event();
                    return;
                }
            }
        }
        self.inner.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.inner.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.inner.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Msg, Theme, iced::Renderer>> {
        self.inner.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a> From<Guard<'a>> for Element<'a, Msg> {
    fn from(g: Guard<'a>) -> Self {
        Element::new(g)
    }
}
