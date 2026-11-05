mod feeds;
mod feeds_dialog;
mod icons;
mod images;
mod reader;
mod window;

use adw::prelude::*;
use gtk::{gio, glib};

pub const APP_ID: &str = "io.github.carterprince.Newseum";

fn main() -> glib::ExitCode {
    if let Err(e) = gst::init() {
        eprintln!("newseum: GStreamer unavailable, podcast playback disabled: {e}");
    }
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::empty())
        .build();

    app.connect_activate(|app| {
        if let Some(win) = app.active_window() {
            win.present();
            return;
        }
        let win = window::Window::new(app);
        win.window.present();
        win.refresh();
        // Signal handlers only hold weak refs to `win`, so the GTK window must own
        // the strong one; it is released when the window is destroyed.
        let keep_alive = std::cell::RefCell::new(Some(win.clone()));
        win.window.connect_destroy(move |_| {
            keep_alive.borrow_mut().take();
        });
    });

    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate({
        let app = app.clone();
        move |_, _| app.quit()
    });
    app.add_action(&quit);
    app.set_accels_for_action("app.quit", &["<Ctrl>q"]);
    app.set_accels_for_action("window.close", &["<Ctrl>w"]);
    app.set_accels_for_action("win.refresh", &["<Ctrl>r", "F5"]);
    app.set_accels_for_action("win.open-in-browser", &["<Ctrl>o"]);
    app.set_accels_for_action("win.manage-feeds", &["<Ctrl>comma"]);

    app.run()
}
