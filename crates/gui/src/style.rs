//! The look of the app: a dark palette and "glass" controls.
//!
//! The values come from Zeron (https://github.com/zeronsh/zeron, MIT), see
//! THIRD_PARTY_NOTICES.md. A glass control is a vertical gradient lit from
//! above, a 1 px rim and a soft drop shadow. It is plain paint: no blur and no
//! extra GPU memory. Only the window background is blurred, by the OS.

use iced::widget::{button, container, overlay::menu, pick_list, text_editor, text_input};
use iced::{Background, Border, Color, Font, Gradient, Shadow, Theme, Vector, font, gradient};

pub const SANS: Font = Font::with_name("Geist");
pub const MEDIUM: Font = Font {
    weight: font::Weight::Medium,
    ..SANS
};
pub const MONO: Font = Font::with_name("Geist Mono");

/// Bundled faces (SIL OFL 1.1, `assets/fonts/OFL.txt`).
pub const FONTS: [&[u8]; 3] = [
    include_bytes!("../assets/fonts/Geist.ttf"),
    include_bytes!("../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../assets/fonts/GeistMono.ttf"),
];

pub const BG: Color = Color::from_rgb8(0x06, 0x06, 0x06);
pub const TEXT: Color = Color::from_rgb8(0xe5, 0xe5, 0xe5);
pub const MUTED: Color = Color::from_rgb8(0xa1, 0xa1, 0xa1);
pub const FAINT: Color = Color::from_rgb8(0x73, 0x73, 0x73);
pub const ACCENT: Color = Color::from_rgb8(0x7c, 0x86, 0xff);
const ACCENT_STRONG: Color = Color::from_rgb8(0x61, 0x5f, 0xff);
pub const DANGER: Color = Color::from_rgb8(0xff, 0x64, 0x67);
const DANGER_STRONG: Color = Color::from_rgb8(0xc7, 0x4b, 0x47);
pub const WARNING: Color = Color::from_rgb8(0xff, 0xb9, 0x00);
pub const SUCCESS: Color = Color::from_rgb8(0x00, 0xd4, 0x92);
pub const BLUE: Color = Color::from_rgb8(0x4c, 0x9f, 0xff);
pub const PURPLE: Color = Color::from_rgb8(0xbb, 0x8a, 0xef);
pub const JSON_KEY: Color = Color::from_rgb8(0x9c, 0xa4, 0xff);
pub const JSON_STRING: Color = Color::from_rgb8(0x7e, 0xe2, 0xb8);
pub const JSON_NUMBER: Color = Color::from_rgb8(0xff, 0xb8, 0x6b);
pub const JSON_LITERAL: Color = PURPLE;

/// How much of the window background stays over the blurred desktop.
pub const GLASS_ALPHA: f32 = 0.80;

pub const RADIUS: f32 = 8.0;

pub fn white(alpha: f32) -> Color {
    Color::from_rgba(1.0, 1.0, 1.0, alpha)
}

fn black(alpha: f32) -> Color {
    Color::from_rgba(0.0, 0.0, 0.0, alpha)
}

fn mix(from: Color, to: Color, t: f32) -> Color {
    Color::from_rgba(
        from.r + (to.r - from.r) * t,
        from.g + (to.g - from.g) * t,
        from.b + (to.b - from.b) * t,
        from.a + (to.a - from.a) * t,
    )
}

pub fn theme() -> Theme {
    Theme::custom(
        "Reqlite",
        iced::theme::Palette {
            background: BG,
            text: TEXT,
            primary: ACCENT,
            success: SUCCESS,
            warning: WARNING,
            danger: DANGER,
        },
    )
}

/// The window background: see-through over blur, solid without it.
pub fn window(glass: bool) -> iced::theme::Style {
    iced::theme::Style {
        background_color: if glass {
            BG.scale_alpha(GLASS_ALPHA)
        } else {
            BG
        },
        text_color: TEXT,
    }
}

/// Top to bottom.
fn vertical(top: Color, bottom: Color) -> Background {
    Background::Gradient(Gradient::Linear(
        gradient::Linear::new(std::f32::consts::PI)
            .add_stop(0.0, top)
            .add_stop(1.0, bottom),
    ))
}

fn hairline(color: Color) -> Border {
    Border {
        color,
        width: 1.0,
        radius: RADIUS.into(),
    }
}

fn drop(alpha: f32, y: f32, blur: f32) -> Shadow {
    Shadow {
        color: black(alpha),
        offset: Vector::new(0.0, y),
        blur_radius: blur,
    }
}

/// A raised neutral plate: Save, the method picker, the active tab.
fn plate(lift: f32) -> (Background, Border, Shadow) {
    (
        vertical(white(0.08 + lift), white(0.05 + lift)),
        hairline(white(0.09 + lift)),
        drop(0.16, 1.0, 2.0),
    )
}

pub fn neutral(_: &Theme, status: button::Status) -> button::Style {
    let lift = match status {
        button::Status::Hovered => 0.03,
        button::Status::Pressed => -0.02,
        _ => 0.0,
    };
    let (background, border, shadow) = plate(lift);
    let disabled = status == button::Status::Disabled;
    button::Style {
        background: Some(background),
        text_color: if disabled { FAINT } else { TEXT },
        border,
        shadow: if disabled { Shadow::default() } else { shadow },
        snap: true,
    }
}

fn accent_with(base: Color, status: button::Status) -> button::Style {
    let glow = match status {
        button::Status::Hovered => 1.0,
        button::Status::Pressed => 0.4,
        _ => 0.0,
    };
    let top = mix(base, white(1.0), 0.06 + 0.06 * glow);
    let alpha = if status == button::Status::Disabled {
        0.4
    } else {
        1.0
    };
    button::Style {
        background: Some(vertical(top.scale_alpha(alpha), base.scale_alpha(alpha))),
        text_color: white(0.98 * alpha),
        border: hairline(mix(base, white(1.0), 0.35).scale_alpha(0.35 * alpha)),
        shadow: Shadow {
            color: base.scale_alpha(0.3 * glow),
            offset: Vector::new(0.0, 2.0 + 2.0 * glow),
            blur_radius: 6.0 + 14.0 * glow,
        },
        snap: true,
    }
}

/// Send.
pub fn accent(_: &Theme, status: button::Status) -> button::Style {
    accent_with(ACCENT_STRONG, status)
}

/// Cancel, while a send runs.
pub fn stop(_: &Theme, status: button::Status) -> button::Style {
    accent_with(DANGER_STRONG, status)
}

fn field_border(focused: bool, hovered: bool) -> Border {
    hairline(if focused {
        ACCENT.scale_alpha(0.6)
    } else if hovered {
        white(0.14)
    } else {
        white(0.08)
    })
}

pub fn input(_: &Theme, status: text_input::Status) -> text_input::Style {
    let (focused, hovered) = match status {
        text_input::Status::Focused { is_hovered } => (true, is_hovered),
        text_input::Status::Hovered => (false, true),
        _ => (false, false),
    };
    text_input::Style {
        background: white(0.03).into(),
        border: field_border(focused, hovered),
        icon: MUTED,
        placeholder: FAINT,
        value: TEXT,
        selection: ACCENT.scale_alpha(0.35),
    }
}

pub fn editor(_: &Theme, status: text_editor::Status) -> text_editor::Style {
    let (focused, hovered) = match status {
        text_editor::Status::Focused { is_hovered } => (true, is_hovered),
        text_editor::Status::Hovered => (false, true),
        _ => (false, false),
    };
    text_editor::Style {
        background: white(0.03).into(),
        border: field_border(focused, hovered),
        placeholder: FAINT,
        value: TEXT,
        selection: ACCENT.scale_alpha(0.35),
    }
}

/// The method picker. Its text takes the method's colour.
pub fn method_picker(color: Color) -> impl Fn(&Theme, pick_list::Status) -> pick_list::Style {
    move |_, status| {
        let lift = if matches!(status, pick_list::Status::Active) {
            0.0
        } else {
            0.03
        };
        let (background, border, _) = plate(lift);
        pick_list::Style {
            text_color: color,
            placeholder_color: FAINT,
            handle_color: MUTED,
            background,
            border,
        }
    }
}

pub fn method_menu(_: &Theme) -> menu::Style {
    menu::Style {
        background: Color::from_rgb8(0x16, 0x16, 0x16).into(),
        border: hairline(white(0.14)),
        text_color: TEXT,
        selected_text_color: TEXT,
        selected_background: white(0.08).into(),
        shadow: drop(0.4, 8.0, 24.0),
    }
}

/// Each common method has its own colour, so the eye finds it at once.
pub fn method_color(method: &str) -> Color {
    match method {
        "GET" => SUCCESS,
        "POST" => WARNING,
        "PUT" => BLUE,
        "PATCH" => PURPLE,
        "DELETE" => DANGER,
        _ => MUTED,
    }
}

pub fn status_color(status: u16) -> Color {
    match status {
        200..=299 => SUCCESS,
        300..=399 => BLUE,
        400..=499 => WARNING,
        500..=599 => DANGER,
        _ => MUTED,
    }
}

/// A request tab: a raised plate when active, bare text otherwise.
pub fn tab(_: &Theme, status: button::Status, active: bool) -> button::Style {
    if active {
        let (background, border, shadow) = plate(0.0);
        return button::Style {
            background: Some(background),
            text_color: TEXT,
            border,
            shadow,
            snap: true,
        };
    }
    button::Style {
        background: (status == button::Status::Hovered).then(|| white(0.04).into()),
        text_color: if status == button::Status::Hovered {
            TEXT
        } else {
            MUTED
        },
        border: Border {
            radius: RADIUS.into(),
            ..Border::default()
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

/// The request and response panels: a faint lift off the window.
pub fn panel(_: &Theme) -> container::Style {
    container::Style {
        background: Some(white(0.025).into()),
        border: Border {
            radius: 10.0.into(),
            ..hairline(white(0.08))
        },
        ..container::Style::default()
    }
}

pub fn pill(color: Color) -> container::Style {
    container::Style {
        background: Some(color.scale_alpha(0.12).into()),
        border: Border {
            radius: 999.0.into(),
            ..hairline(color.scale_alpha(0.3))
        },
        ..container::Style::default()
    }
}

pub fn danger_banner(_: &Theme) -> container::Style {
    container::Style {
        background: Some(DANGER.scale_alpha(0.08).into()),
        ..container::Style::default()
    }
    .border(hairline(DANGER.scale_alpha(0.3)))
}
