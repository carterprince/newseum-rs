//! "Manage Feeds" dialog: add, edit, and remove entries in feeds.csv.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::feeds::{self, Feed};

struct State {
    dialog: adw::PreferencesDialog,
    list: gtk::ListBox,
    feeds: RefCell<Vec<Feed>>,
    changed: Cell<bool>,
}

/// Show the dialog. `on_changed` runs once when it closes, if anything was modified.
pub fn present(parent: &impl IsA<gtk::Widget>, on_changed: impl Fn() + 'static) {
    let dialog = adw::PreferencesDialog::builder()
        .title("Manage Feeds")
        .content_width(640)
        .content_height(640)
        .search_enabled(false)
        .build();

    let feeds = match feeds::load_feeds() {
        Ok(f) => f,
        Err(e) => {
            dialog.add_toast(adw::Toast::new(&e));
            Vec::new()
        }
    };

    let add_btn = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("Add Feed")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    let group = adw::PreferencesGroup::builder()
        .title("Feeds")
        .description(feeds::config_path().display().to_string())
        .header_suffix(&add_btn)
        .build();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    group.add(&list);
    let page = adw::PreferencesPage::new();
    page.add(&group);
    dialog.add(&page);

    let state = Rc::new(State { dialog: dialog.clone(), list, feeds: RefCell::new(feeds), changed: Cell::new(false) });
    rebuild(&state);

    add_btn.connect_clicked({
        let state = state.clone();
        move |_| edit_feed(&state, None)
    });

    dialog.connect_closed({
        let state = state.clone();
        move |_| {
            if state.changed.get() {
                on_changed();
            }
        }
    });

    dialog.present(Some(parent));
}

fn persist(state: &Rc<State>) -> bool {
    match feeds::save_feeds(&state.feeds.borrow()) {
        Ok(()) => {
            state.changed.set(true);
            true
        }
        Err(e) => {
            state.dialog.add_toast(adw::Toast::new(&e));
            false
        }
    }
}

fn rebuild(state: &Rc<State>) {
    state.list.remove_all();
    let feeds = state.feeds.borrow();
    if feeds.is_empty() {
        let row = adw::ActionRow::builder().title("No feeds yet").css_classes(["dim-label"]).build();
        state.list.append(&row);
        return;
    }
    for (i, feed) in feeds.iter().enumerate() {
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&feed.name))
            .subtitle(glib::markup_escape_text(&feed.url))
            .subtitle_lines(1)
            .build();

        let edit = gtk::Button::builder()
            .icon_name("document-edit-symbolic")
            .tooltip_text("Edit")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        edit.connect_clicked({
            let state = state.clone();
            move |_| edit_feed(&state, Some(i))
        });

        let delete = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Remove")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        delete.connect_clicked({
            let state = state.clone();
            move |_| remove_feed(&state, i)
        });

        row.add_suffix(&edit);
        row.add_suffix(&delete);
        row.set_activatable_widget(Some(&edit));
        state.list.append(&row);
    }
}

fn remove_feed(state: &Rc<State>, index: usize) {
    let removed = state.feeds.borrow_mut().remove(index);
    if !persist(state) {
        state.feeds.borrow_mut().insert(index, removed);
        return;
    }
    rebuild(state);

    let toast = adw::Toast::builder()
        .title(format!("Removed {}", glib::markup_escape_text(&removed.name)))
        .button_label("Undo")
        .build();
    toast.connect_button_clicked({
        let state = state.clone();
        move |_| {
            let mut feeds = state.feeds.borrow_mut();
            let at = index.min(feeds.len());
            feeds.insert(at, removed.clone());
            drop(feeds);
            persist(&state);
            rebuild(&state);
        }
    });
    state.dialog.add_toast(toast);
}

/// Add (`index == None`) or edit an existing feed. The URL is fetched and parsed before saving.
fn edit_feed(state: &Rc<State>, index: Option<usize>) {
    let original = index.map(|i| state.feeds.borrow()[i].clone());

    let dialog = adw::Dialog::builder()
        .title(if original.is_some() { "Edit Feed" } else { "Add Feed" })
        .content_width(480)
        .build();

    let cancel = gtk::Button::with_label("Cancel");
    let save_label = gtk::Label::new(Some(if original.is_some() { "Save" } else { "Add" }));
    let save_spinner = adw::Spinner::builder().visible(false).build();
    let save_inner = gtk::Box::builder().spacing(8).build();
    save_inner.append(&save_spinner);
    save_inner.append(&save_label);
    let save = gtk::Button::builder().child(&save_inner).css_classes(["suggested-action"]).build();

    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&save);

    let name_row = adw::EntryRow::builder().title("Name").build();
    let url_row = adw::EntryRow::builder().title("Feed URL").input_purpose(gtk::InputPurpose::Url).build();
    if let Some(f) = &original {
        name_row.set_text(&f.name);
        url_row.set_text(&f.url);
    }
    let fields = adw::PreferencesGroup::builder()
        .description("Leave the name empty to use the feed's own title. YouTube channel links become a feed of the channel's videos, without Shorts.")
        .build();
    fields.add(&name_row);
    fields.add(&url_row);

    let error = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .css_classes(["error"])
        .build();

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    body.append(&fields);
    body.append(&error);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));
    dialog.set_default_widget(Some(&save));
    name_row.set_activates_default(true);
    url_row.set_activates_default(true);

    cancel.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });

    let busy = Rc::new(Cell::new(false));
    save.connect_clicked({
        let state = state.clone();
        let dialog = dialog.clone();
        let save = save.clone();
        move |_| {
            if busy.get() {
                return;
            }
            let name = name_row.text().trim().to_string();
            let url = url_row.text().trim().to_string();
            let show_error = |msg: &str| {
                error.set_text(msg);
                error.set_visible(true);
            };
            if url.is_empty() {
                show_error("Please enter a feed URL.");
                return;
            }
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                show_error("The URL must start with http:// or https://.");
                return;
            }
            let duplicate = state
                .feeds
                .borrow()
                .iter()
                .enumerate()
                .any(|(i, f)| Some(i) != index && f.url == url);
            if duplicate {
                show_error("That feed is already in your list.");
                return;
            }

            // Unchanged URL on an existing feed: no need to re-check it.
            if original.as_ref().is_some_and(|f| f.url == url) && !name.is_empty() {
                commit(&state, index, Feed { name, url });
                dialog.close();
                return;
            }

            busy.set(true);
            error.set_visible(false);
            save.set_sensitive(false);
            save_spinner.set_visible(true);
            save_label.set_text("Checking…");

            let (tx, rx) = async_channel::bounded(1);
            let check_url = url.clone();
            std::thread::spawn(move || {
                let _ = tx.send_blocking(feeds::resolve(&check_url));
            });

            let state = state.clone();
            let dialog = dialog.clone();
            let save = save.clone();
            let save_spinner = save_spinner.clone();
            let save_label = save_label.clone();
            let error = error.clone();
            let busy = busy.clone();
            let label_text = if original.is_some() { "Save" } else { "Add" };
            glib::spawn_future_local(async move {
                let result = rx.recv().await.unwrap_or_else(|_| Err("Check failed".into()));
                busy.set(false);
                save.set_sensitive(true);
                save_spinner.set_visible(false);
                save_label.set_text(label_text);
                match result {
                    Ok(resolved) => {
                        let url = resolved.url;
                        let duplicate = state
                            .feeds
                            .borrow()
                            .iter()
                            .enumerate()
                            .any(|(i, f)| Some(i) != index && f.url == url);
                        if duplicate {
                            error.set_text("That feed is already in your list.");
                            error.set_visible(true);
                            return;
                        }
                        let name = if name.is_empty() { resolved.title.unwrap_or_else(|| url.clone()) } else { name };
                        commit(&state, index, Feed { name, url });
                        dialog.close();
                    }
                    Err(e) => {
                        error.set_text(&format!("Couldn't load this feed: {e}"));
                        error.set_visible(true);
                    }
                }
            });
        }
    });

    dialog.present(Some(&state.dialog));
}

fn commit(state: &Rc<State>, index: Option<usize>, feed: Feed) {
    let previous = {
        let mut feeds = state.feeds.borrow_mut();
        match index {
            Some(i) => Some(std::mem::replace(&mut feeds[i], feed)),
            None => {
                feeds.push(feed);
                None
            }
        }
    };
    if !persist(state) {
        let mut feeds = state.feeds.borrow_mut();
        match (index, previous) {
            (Some(i), Some(p)) => feeds[i] = p,
            _ => {
                feeds.pop();
            }
        }
    }
    rebuild(state);
}
