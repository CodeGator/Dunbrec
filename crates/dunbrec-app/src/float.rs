//! Instrument windows that can leave the main window.
//!
//! An instrument is normally a floating window inside the main window.
//! Its "Pop out" button turns it into a window of its own that can be
//! dragged anywhere on the desktop, onto another monitor included; "Dock"
//! brings it back. Which instruments are popped out is remembered between
//! sessions.

use egui::{Align, Context, Id, Layout, Pos2, Ui, ViewportBuilder, ViewportId};

/// The instruments that can be popped out, by window title.
pub const OSCILLOSCOPE: &str = "Oscilloscope";
pub const MULTIMETER: &str = "Multimeter";
pub const LOGIC_ANALYSER: &str = "Logic analyser";
pub const BODE_PLOTTER: &str = "Bode plotter";
pub const DC_TRANSFER: &str = "DC transfer";
pub const CURVE_TRACER: &str = "Curve tracer";
pub const FUNCTION_GENERATOR: &str = "Function generator";
pub const MCU_DEBUGGER: &str = "MCU debugger";
pub const SERIAL_MONITOR: &str = "Serial monitor";

/// Every instrument above.
pub const ALL: [&str; 9] = [
    OSCILLOSCOPE,
    MULTIMETER,
    LOGIC_ANALYSER,
    BODE_PLOTTER,
    DC_TRANSFER,
    CURVE_TRACER,
    FUNCTION_GENERATOR,
    MCU_DEBUGGER,
    SERIAL_MONITOR,
];

/// How an instrument's window starts out.
pub struct Spec {
    pub title: &'static str,
    pub size: [f32; 2],
    pub resizable: bool,
    /// Where the floating window starts; `None` lets egui choose.
    pub pos: Option<Pos2>,
}

fn popped_key(title: &str) -> Id {
    Id::new(("popped", title))
}

/// Whether the instrument called `title` is in a window of its own.
pub fn is_popped(ctx: &Context, title: &str) -> bool {
    ctx.data_mut(|d| d.get_persisted::<bool>(popped_key(title)))
        .unwrap_or(false)
}

/// Whether any instrument is in a window of its own.
pub fn any_popped(ctx: &Context) -> bool {
    ALL.iter().any(|title| is_popped(ctx, title))
}

/// Bring every popped-out instrument back inside the main window: the way
/// out if one ever misbehaves as a window of its own.
pub fn dock_all(ctx: &Context) {
    for title in ALL {
        if is_popped(ctx, title) {
            ctx.data_mut(|d| d.insert_persisted(popped_key(title), false));
        }
    }
}

/// Draw the instrument while `open` is true. Closing the window, either
/// kind, sets `open` to false.
pub fn window(ctx: &Context, spec: Spec, open: &mut bool, body: impl FnOnce(&mut Ui)) {
    // A backend that cannot make real windows keeps everything inside.
    let can_pop = !ctx.embed_viewports();
    let mut popped = can_pop && is_popped(ctx, spec.title);
    let was = popped;
    if popped {
        popped_out(ctx, &spec, open, &mut popped, body);
    } else {
        inside(ctx, &spec, open, &mut popped, can_pop, body);
    }
    if popped != was {
        ctx.data_mut(|d| d.insert_persisted(popped_key(spec.title), popped));
    }
}

fn inside(
    ctx: &Context,
    spec: &Spec,
    open: &mut bool,
    popped: &mut bool,
    can_pop: bool,
    body: impl FnOnce(&mut Ui),
) {
    let mut window_open = *open;
    let mut window = egui::Window::new(spec.title)
        .open(&mut window_open)
        .default_size(spec.size)
        .resizable(spec.resizable);
    if let Some(pos) = spec.pos {
        window = window.default_pos(pos);
    }
    window.show(ctx, |ui| {
        if can_pop {
            *popped |= pop_button(
                ui,
                "⧉ Pop out",
                "Move to a window of its own, which can leave the main window",
            );
        }
        body(ui);
    });
    *open = window_open;
}

fn popped_out(
    ctx: &Context,
    spec: &Spec,
    open: &mut bool,
    popped: &mut bool,
    body: impl FnOnce(&mut Ui),
) {
    let mut body = Some(body);
    let builder = ViewportBuilder::default()
        .with_title(format!("{} — Dunbrec", spec.title))
        .with_inner_size(spec.size)
        .with_resizable(spec.resizable);
    let mut dock_back = false;
    let mut closed = false;
    ctx.show_viewport_immediate(ViewportId::from_hash_of(spec.title), builder, |ctx, _| {
        egui::CentralPanel::default().show(ctx, |ui| {
            dock_back |= pop_button(ui, "⧉ Dock", "Put this back inside the main window");
            if let Some(body) = body.take() {
                body(ui);
            }
        });
        closed |= ctx.input(|i| i.viewport().close_requested());
    });
    if closed {
        *open = false;
    }
    if dock_back {
        *popped = false;
    }
}

/// A small button at the top right; true when clicked.
fn pop_button(ui: &mut Ui, label: &str, hint: &str) -> bool {
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.small_button(label).on_hover_text(hint).clicked()
        })
        .inner
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Spec {
        Spec {
            title: "Test instrument",
            size: [200.0, 100.0],
            resizable: true,
            pos: None,
        }
    }

    #[test]
    fn an_open_instrument_draws_its_body_and_stays_open() {
        let ctx = Context::default();
        let mut open = true;
        let mut drawn = 0;
        let _ = ctx.run(Default::default(), |ctx| {
            window(ctx, spec(), &mut open, |_| drawn += 1);
        });
        assert_eq!(drawn, 1);
        assert!(open);
    }

    #[test]
    fn docking_all_brings_every_popped_out_instrument_back() {
        let ctx = Context::default();
        assert!(!any_popped(&ctx));
        for title in ALL {
            ctx.data_mut(|d| d.insert_persisted(popped_key(title), true));
        }
        assert!(any_popped(&ctx));
        assert!(is_popped(&ctx, OSCILLOSCOPE));
        dock_all(&ctx);
        assert!(!any_popped(&ctx));
        // Every title is distinct, so docking one cannot dock another.
        let mut titles = ALL.to_vec();
        titles.sort();
        titles.dedup();
        assert_eq!(titles.len(), ALL.len());
    }

    #[test]
    fn without_real_windows_nothing_is_popped_out() {
        // A context that embeds viewports (as in tests and on some
        // backends) keeps every instrument inside, even if it was saved as
        // popped out.
        let ctx = Context::default();
        assert!(ctx.embed_viewports());
        ctx.data_mut(|d| d.insert_persisted(popped_key("Test instrument"), true));
        let mut open = true;
        let mut drawn = 0;
        let _ = ctx.run(Default::default(), |ctx| {
            window(ctx, spec(), &mut open, |_| drawn += 1);
        });
        assert_eq!(drawn, 1);
        assert!(open);
    }
}
