//! The smallest iced window: one line of text. It measures the toolkit's own
//! idle memory, without any Reqlite code. Set REQLITE_SPIKE_WINDOW=WxH to size it.

fn view(_: &()) -> iced::Element<'_, ()> {
    iced::widget::text("hello").into()
}

fn main() -> iced::Result {
    let size = std::env::var("REQLITE_SPIKE_WINDOW")
        .ok()
        .and_then(|v| v.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))))
        .unwrap_or((1000.0, 800.0));
    iced::application(|| (), |_: &mut (), _: ()| {}, view)
        .window_size(size)
        .run()
}
