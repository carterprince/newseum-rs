//! Main window: merged article list on the left, reader pane on the right.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::feeds::{self, Entry};
use crate::icons::Icons;
use crate::reader::Reader;
use crate::{feeds_dialog, APP_ID};

pub struct Window {
    pub window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    split: adw::NavigationSplitView,
    store: gio::ListStore,
    filter_model: gtk::FilterListModel,
    selection: gtk::SingleSelection,
    list_stack: gtk::Stack,
    list_title: adw::WindowTitle,
    progress: gtk::ProgressBar,
    progress_detail: gtk::Label,
    refresh_btn: gtk::Button,
    content_stack: gtk::Stack,
    content_title: adw::WindowTitle,
    reader: Reader,
    icons: Rc<Icons>,
    current: RefCell<Option<Rc<Entry>>>,
    loading: Cell<bool>,
}

fn entry_of(obj: &glib::Object) -> Rc<Entry> {
    obj.downcast_ref::<glib::BoxedAnyObject>()
        .expect("list item is BoxedAnyObject")
        .borrow::<Rc<Entry>>()
        .clone()
}

fn launch_uri(window: &impl IsA<gtk::Window>, uri: &str) {
    gtk::UriLauncher::new(uri).launch(Some(window), None::<&gio::Cancellable>, |res| {
        if let Err(e) = res {
            eprintln!("newseum: could not open link: {e}");
        }
    });
}

impl Window {
    pub fn new(app: &adw::Application) -> Rc<Self> {
        // ---------- article list ----------
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let query: Rc<RefCell<Vec<String>>> = Rc::default();
        let filter = gtk::CustomFilter::new({
            let query = query.clone();
            move |obj| {
                let q = query.borrow();
                if q.is_empty() {
                    return true;
                }
                let e = entry_of(obj);
                let hay = format!("{} {}", e.feed_name, e.title).to_lowercase();
                q.iter().all(|w| hay.contains(w.as_str()))
            }
        });
        let filter_model = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let selection = gtk::SingleSelection::builder()
            .model(&filter_model)
            .autoselect(false)
            .can_unselect(true)
            .build();

        let icons = Icons::new();
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(12)
                .margin_top(7)
                .margin_bottom(7)
                .margin_start(4)
                .margin_end(4)
                .build();
            let icon = gtk::Image::builder().pixel_size(16).build();
            let feed = gtk::Label::builder()
                .xalign(0.0)
                .width_chars(15)
                .max_width_chars(15)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["caption-heading", "accent"])
                .build();
            let feed_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            feed_box.append(&icon);
            feed_box.append(&feed);
            let title = gtk::Label::builder()
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            let date = gtk::Label::builder()
                .xalign(1.0)
                .width_chars(4)
                .css_classes(["caption", "dim-label", "numeric"])
                .build();
            row.append(&feed_box);
            row.append(&title);
            row.append(&date);
            item.set_child(Some(&row));
        });
        factory.connect_bind({
            let icons = icons.clone();
            move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let entry = entry_of(&item.item().unwrap());
            let row = item.child().unwrap();
            let feed_box = row.first_child().unwrap();
            let icon = feed_box.first_child().unwrap().downcast::<gtk::Image>().unwrap();
            let feed = icon.next_sibling().unwrap().downcast::<gtk::Label>().unwrap();
            let title = feed_box.next_sibling().unwrap().downcast::<gtk::Label>().unwrap();
            icons.apply(&icon, &entry.feed_name);
            let date = title.next_sibling().unwrap().downcast::<gtk::Label>().unwrap();
            feed.set_text(&entry.feed_name);
            feed.set_tooltip_text(Some(&entry.feed_name));
            title.set_text(&entry.title);
            title.set_tooltip_text(Some(&entry.title));
            date.set_text(&feeds::relative_date(entry.date));
            if let Some(d) = entry.date {
                let local = d.with_timezone(&chrono::Local);
                date.set_tooltip_text(Some(&local.format("%Y-%m-%d %H:%M").to_string()));
            } else {
                date.set_tooltip_text(None);
            }
        }});

        let list_view = gtk::ListView::builder()
            .model(&selection)
            .factory(&factory)
            .css_classes(["navigation-sidebar"])
            .build();
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&list_view)
            .vexpand(true)
            .build();

        // ---------- loading overlay ----------
        let progress = gtk::ProgressBar::builder().width_request(280).build();
        let progress_detail = gtk::Label::builder()
            .css_classes(["dim-label", "caption"])
            .wrap(true)
            .justify(gtk::Justification::Center)
            .max_width_chars(40)
            .build();
        let loading_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .valign(gtk::Align::Center)
            .halign(gtk::Align::Center)
            .build();
        loading_box.append(&adw::Spinner::builder().width_request(48).height_request(48).build());
        loading_box.append(&gtk::Label::builder().label("Fetching Feeds").css_classes(["title-2"]).build());
        loading_box.append(&progress);
        loading_box.append(&progress_detail);

        let manage_btn = gtk::Button::builder()
            .label("Manage Feeds")
            .halign(gtk::Align::Center)
            .action_name("win.manage-feeds")
            .css_classes(["pill", "suggested-action"])
            .build();
        let empty = adw::StatusPage::builder()
            .icon_name("application-rss+xml-symbolic")
            .title("No Feeds")
            .description(format!("Add some feeds to get started.\nThey are stored in {}", feeds::config_path().display()))
            .child(&manage_btn)
            .build();
        let no_results = adw::StatusPage::builder()
            .icon_name("edit-find-symbolic")
            .title("No Results")
            .build();

        let list_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        list_stack.add_named(&loading_box, Some("loading"));
        list_stack.add_named(&scrolled, Some("list"));
        list_stack.add_named(&empty, Some("empty"));
        list_stack.add_named(&no_results, Some("no-results"));

        // ---------- sidebar header + search ----------
        let list_title = adw::WindowTitle::new("Newseum", "");
        let refresh_btn = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Refresh (Ctrl+R)")
            .action_name("win.refresh")
            .build();
        let search_btn = gtk::ToggleButton::builder()
            .icon_name("system-search-symbolic")
            .tooltip_text("Search (Ctrl+F)")
            .build();
        let menu = gio::Menu::new();
        menu.append(Some("_Manage Feeds"), Some("win.manage-feeds"));
        menu.append(Some("_About Newseum"), Some("win.about"));
        let menu_btn = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .tooltip_text("Main Menu")
            .primary(true)
            .build();
        let side_header = adw::HeaderBar::builder().title_widget(&list_title).build();
        side_header.pack_start(&refresh_btn);
        side_header.pack_end(&menu_btn);
        side_header.pack_end(&search_btn);

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Search articles")
            .hexpand(true)
            .build();
        let search_bar = gtk::SearchBar::builder().child(&search_entry).build();
        search_bar.connect_entry(&search_entry);
        search_btn
            .bind_property("active", &search_bar, "search-mode-enabled")
            .bidirectional()
            .sync_create()
            .build();

        let side_toolbar = adw::ToolbarView::new();
        side_toolbar.add_top_bar(&side_header);
        side_toolbar.add_top_bar(&search_bar);
        side_toolbar.set_content(Some(&list_stack));

        // ---------- reader pane ----------
        // Links clicked in the reader open in the browser. The window doesn't exist
        // yet, so the handler looks it up through a cell filled in below.
        let parent: Rc<RefCell<Option<glib::WeakRef<adw::ApplicationWindow>>>> = Rc::default();
        let reader = Reader::new(icons.clone(), {
            let parent = parent.clone();
            move |uri| {
                if let Some(win) = parent.borrow().as_ref().and_then(|w| w.upgrade()) {
                    launch_uri(&win, uri);
                }
            }
        });
        let placeholder = adw::StatusPage::builder()
            .icon_name("x-office-document-symbolic")
            .title("No Article Selected")
            .build();
        let content_stack = gtk::Stack::new();
        content_stack.add_named(&placeholder, Some("placeholder"));
        content_stack.add_named(&reader.widget, Some("reader"));

        let content_title = adw::WindowTitle::new("", "");
        let open_btn = gtk::Button::builder()
            .icon_name("adw-external-link-symbolic")
            .tooltip_text("Open in Browser (Ctrl+O)")
            .action_name("win.open-in-browser")
            .build();
        let content_header = adw::HeaderBar::builder().title_widget(&content_title).build();
        content_header.pack_end(&open_btn);
        let content_toolbar = adw::ToolbarView::new();
        content_toolbar.add_top_bar(&content_header);
        content_toolbar.set_content(Some(&content_stack));

        // ---------- split view + window ----------
        let split = adw::NavigationSplitView::builder()
            .sidebar(&adw::NavigationPage::new(&side_toolbar, "Newseum"))
            .content(&adw::NavigationPage::new(&content_toolbar, "Article"))
            .sidebar_width_fraction(0.42)
            .min_sidebar_width(340.0)
            .max_sidebar_width(680.0)
            .build();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&split));

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Newseum")
            .icon_name(APP_ID)
            .default_width(1360)
            .default_height(860)
            .width_request(360)
            .height_request(360)
            .content(&toasts)
            .build();
        search_bar.set_key_capture_widget(Some(&window));
        *parent.borrow_mut() = Some(window.downgrade());

        let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            760.0,
            adw::LengthUnit::Sp,
        ));
        bp.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(bp);

        let this = Rc::new(Self {
            window,
            toasts,
            split,
            store,
            filter_model,
            selection,
            list_stack,
            list_title,
            progress,
            progress_detail,
            refresh_btn,
            content_stack,
            content_title,
            reader,
            icons,
            current: RefCell::default(),
            loading: Cell::new(false),
        });

        // ---------- signals ----------
        search_entry.connect_search_changed({
            let this = Rc::downgrade(&this);
            move |entry| {
                *query.borrow_mut() = entry
                    .text()
                    .to_lowercase()
                    .split_whitespace()
                    .map(str::to_string)
                    .collect();
                filter.changed(gtk::FilterChange::Different);
                if let Some(this) = this.upgrade() {
                    this.update_list_state();
                }
            }
        });

        this.selection.connect_selected_item_notify({
            let this = Rc::downgrade(&this);
            move |sel| {
                let (Some(this), Some(item)) = (this.upgrade(), sel.selected_item()) else { return };
                this.show_entry(entry_of(&item));
            }
        });

        this.setup_actions();
        this.update_list_state();
        this
    }

    fn setup_actions(self: &Rc<Self>) {
        let add = |name: &str, f: Box<dyn Fn(&Rc<Self>)>| {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(this) = weak.upgrade() {
                    f(&this);
                }
            });
            self.window.add_action(&action);
        };
        add("refresh", Box::new(|this| this.refresh()));
        add("open-in-browser", Box::new(|this| {
            if let Some(link) = this.current.borrow().as_ref().and_then(|e| e.link.clone()) {
                launch_uri(&this.window, &link);
            }
        }));
        add("manage-feeds", Box::new(|this| {
            let weak = Rc::downgrade(this);
            feeds_dialog::present(&this.window, move || {
                if let Some(this) = weak.upgrade() {
                    this.refresh();
                }
            });
        }));
        add("about", Box::new(|this| {
            adw::AboutDialog::builder()
                .application_name("Newseum")
                .application_icon(APP_ID)
                .version(env!("CARGO_PKG_VERSION"))
                .comments("A simple feed reader")
                .license_type(gtk::License::MitX11)
                .build()
                .present(Some(&this.window));
        }));
        if let Some(a) = self.window.lookup_action("open-in-browser") {
            a.downcast::<gio::SimpleAction>().unwrap().set_enabled(false);
        }
    }

    fn set_action_enabled(&self, name: &str, enabled: bool) {
        if let Some(a) = self.window.lookup_action(name) {
            a.downcast::<gio::SimpleAction>().unwrap().set_enabled(enabled);
        }
    }

    fn update_list_state(&self) {
        if self.loading.get() {
            return;
        }
        let total = self.store.n_items();
        let shown = self.filter_model.n_items();
        let page = if total == 0 {
            "empty"
        } else if shown == 0 {
            "no-results"
        } else {
            "list"
        };
        self.list_stack.set_visible_child_name(page);
        self.list_title.set_subtitle(&match (total, shown) {
            (0, _) => String::new(),
            (t, s) if t == s => format!("{t} articles"),
            (t, s) => format!("{s} of {t} articles"),
        });
    }

    fn show_entry(&self, entry: Rc<Entry>) {
        self.content_title.set_title(&entry.feed_name);
        self.content_title.set_subtitle(&feeds::relative_date(entry.date));
        self.set_action_enabled("open-in-browser", entry.link.is_some());
        self.reader.show(&entry);
        *self.current.borrow_mut() = Some(entry);
        self.content_stack.set_visible_child_name("reader");
        self.split.set_show_content(true);
    }

    pub fn refresh(self: &Rc<Self>) {
        if self.loading.get() {
            return;
        }
        let feed_list = match feeds::load_feeds() {
            Ok(f) => f,
            Err(e) => {
                self.toasts.add_toast(adw::Toast::new(&e));
                return;
            }
        };
        if feed_list.is_empty() {
            self.store.remove_all();
            self.update_list_state();
            return;
        }

        self.loading.set(true);
        self.refresh_btn.set_sensitive(false);
        self.set_action_enabled("refresh", false);
        self.progress.set_fraction(0.0);
        let total = feed_list.len();
        self.progress_detail.set_text(&format!("0 of {total} feeds"));
        self.list_title.set_subtitle("Refreshing…");
        self.list_stack.set_visible_child_name("loading");

        let (tx, rx) = async_channel::unbounded();
        let mut pending: BTreeSet<String> = BTreeSet::new();
        for feed in feed_list {
            pending.insert(feed.name.clone());
            let tx = tx.clone();
            std::thread::spawn(move || {
                let result = feeds::fetch(&feed);
                let _ = tx.send_blocking((feed.name, result));
            });
        }
        drop(tx);

        let this = self.clone();
        glib::spawn_future_local(async move {
            let mut entries: Vec<Entry> = Vec::new();
            let mut failures: Vec<(String, String)> = Vec::new();
            let mut metas: Vec<(String, feeds::FeedMeta)> = Vec::new();
            let mut done = 0usize;
            while let Ok((name, result)) = rx.recv().await {
                done += 1;
                pending.remove(&name);
                match result {
                    Ok(fetched) => {
                        entries.extend(fetched.entries);
                        metas.push((name, fetched.meta));
                    }
                    Err(err) => failures.push((name, err)),
                }
                this.progress.set_fraction(done as f64 / total as f64);
                let mut detail = format!("{done} of {total} feeds");
                if !pending.is_empty() && pending.len() <= 3 {
                    let names: Vec<_> = pending.iter().map(String::as_str).collect();
                    detail.push_str(&format!("\nWaiting for {}", names.join(", ")));
                }
                this.progress_detail.set_text(&detail);
            }

            // Newest first; undated entries sink to the bottom.
            entries.sort_by(|a, b| b.date.cmp(&a.date));
            let objs: Vec<glib::BoxedAnyObject> =
                entries.into_iter().map(|e| glib::BoxedAnyObject::new(Rc::new(e))).collect();
            this.selection.set_selected(gtk::INVALID_LIST_POSITION);
            this.store.splice(0, this.store.n_items(), &objs);

            this.loading.set(false);
            this.refresh_btn.set_sensitive(true);
            this.set_action_enabled("refresh", true);
            this.update_list_state();

            // Icons load after the list is up, so they never delay it.
            for (name, meta) in &metas {
                this.icons.request(name, meta);
            }

            if !failures.is_empty() {
                for (name, err) in &failures {
                    eprintln!("newseum: failed to fetch {name}: {err}");
                }
                failures.sort();
                let names: Vec<_> = failures.iter().map(|(n, _)| glib::markup_escape_text(n).to_string()).collect();
                let toast = adw::Toast::builder()
                    .title(if failures.len() == 1 {
                        format!("Couldn't fetch {}", names[0])
                    } else {
                        format!("Couldn't fetch {} feeds: {}", failures.len(), names.join(", "))
                    })
                    .timeout(8)
                    .build();
                this.toasts.add_toast(toast);
            }
        });
    }
}
