//! Native reader pane: renders an entry's HTML as GTK widgets.
//!
//! Text runs become wrapping `GtkLabel`s with Pango markup (links included),
//! images become `GtkPicture`s loaded in the background, and things that need
//! a browser (iframes, videos) become cards that open in the default browser.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gst::prelude::*;
use gtk::{gdk, glib, graphene, pango};
use scraper::{ElementRef, Html, Node};
use url::Url;

use crate::feeds::Entry;
use crate::icons::Icons;

const CSS: &str = r#"
.reader-text { font-size: 12.5pt; line-height: 1.45; }
.reader-title { font-size: 22pt; font-weight: 800; line-height: 1.2; }
.reader-title link, .reader-title link:hover { color: inherit; text-decoration-line: none; }
.reader-feed { font-size: 9pt; font-weight: 700; letter-spacing: 1px; }
.reader-h1 { font-size: 18pt; font-weight: 800; }
.reader-h2 { font-size: 16pt; font-weight: 700; }
.reader-h3 { font-size: 14pt; font-weight: 700; }
.reader-h4 { font-size: 12.5pt; font-weight: 700; }
.reader-caption { font-size: 10.5pt; }
.reader-quote { border-left: 3px solid alpha(currentColor, 0.25); padding-left: 14px; }
.reader-code { background: alpha(currentColor, 0.06); border-radius: 8px; padding: 10px 14px; font-size: 11pt; }
.reader-image { border-radius: 10px; }
.reader-play { border-radius: 9999px; padding: 14px; }
.reader-embed { padding: 12px 14px; }
"#;

/// Bigger images are downscaled before upload; plenty for a ~760px column at 2x.
const MAX_IMAGE_PX: i32 = 1600;
const IMAGE_CACHE_CAP: usize = 96;

thread_local! {
    /// Session-only cache so flipping between entries doesn't re-download images.
    static IMAGE_CACHE: RefCell<HashMap<String, gdk::Texture>> = RefCell::default();
}

pub struct Reader {
    pub widget: gtk::ScrolledWindow,
    clamp: adw::Clamp,
    state: Rc<State>,
}

struct State {
    scrolled: gtk::ScrolledWindow,
    content: RefCell<Option<gtk::Box>>,
    anchors: RefCell<HashMap<String, gtk::Widget>>,
    /// Entry link without fragment, to recognise in-page anchors.
    page: RefCell<Option<String>>,
    /// Audio pipelines in the current entry, stopped when switching entries.
    players: RefCell<Vec<glib::WeakRef<gst::Element>>>,
    on_open: Box<dyn Fn(&str)>,
    icons: Rc<Icons>,
}

impl State {
    fn open(&self, uri: &str) {
        if let Some((page, frag)) = uri.split_once('#') {
            let same_page = page.is_empty() || self.page.borrow().as_deref() == Some(page);
            if same_page && !frag.is_empty() && self.scroll_to(frag) {
                return;
            }
        }
        (self.on_open)(uri);
    }

    fn scroll_to(&self, id: &str) -> bool {
        let anchors = self.anchors.borrow();
        let (Some(target), Some(content)) = (anchors.get(id), self.content.borrow().clone()) else {
            return false;
        };
        let Some(p) = target.compute_point(&content, &graphene::Point::new(0.0, 0.0)) else { return false };
        self.scrolled.vadjustment().set_value((p.y() as f64 - 16.0).max(0.0));
        true
    }
}

impl Reader {
    pub fn new(icons: Rc<Icons>, on_open: impl Fn(&str) + 'static) -> Self {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().expect("display"),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let clamp = adw::Clamp::builder().maximum_size(760).tightening_threshold(560).build();
        // Natural vertical policy: pictures can shrink (minimum height 0), so the
        // default "minimum" policy would squash every image to nothing.
        let viewport = gtk::Viewport::builder()
            .vscroll_policy(gtk::ScrollablePolicy::Natural)
            .child(&clamp)
            .build();
        let widget = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .focusable(true)
            .child(&viewport)
            .build();
        let state = Rc::new(State {
            scrolled: widget.clone(),
            content: RefCell::default(),
            anchors: RefCell::default(),
            page: RefCell::default(),
            players: RefCell::default(),
            on_open: Box::new(on_open),
            icons,
        });
        Self { widget, clamp, state }
    }

    pub fn show(&self, entry: &Entry) {
        // Stop any podcast that was playing in the previous entry.
        for p in self.state.players.borrow_mut().drain(..) {
            if let Some(p) = p.upgrade() {
                let _ = p.set_state(gst::State::Null);
            }
        }
        // If focus is inside the old article (e.g. text was selected), move it to the
        // scroller first; otherwise GTK hands focus to the first label of the new
        // article, selecting all of its text.
        if let Some(focus) = self.widget.root().and_then(|r| r.focus()) {
            if focus.is_ancestor(&self.widget) {
                self.widget.grab_focus();
            }
        }
        self.state.anchors.borrow_mut().clear();
        *self.state.page.borrow_mut() = entry.link.as_deref().map(|l| l.split('#').next().unwrap_or(l).to_string());

        let content = build_entry(&self.state, entry);
        self.clamp.set_child(Some(&content));
        *self.state.content.borrow_mut() = Some(content);
        self.widget.vadjustment().set_value(0.0);
    }
}

fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

fn markup_escape(s: &str) -> String {
    glib::markup_escape_text(s).to_string()
}

fn text_label(state: &Rc<State>, markup: &str, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::builder()
        .use_markup(true)
        .label(markup)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .xalign(0.0)
        .selectable(true)
        .build();
    for c in classes {
        label.add_css_class(c);
    }
    // Justify body paragraphs only; headings, captions and titles stay ragged-right.
    if classes == ["reader-text"] {
        label.set_justify(gtk::Justification::Fill);
    }
    let weak = Rc::downgrade(state);
    label.connect_activate_link(move |_, uri| {
        if let Some(state) = weak.upgrade() {
            state.open(uri);
        }
        glib::Propagation::Stop
    });
    label
}

fn build_entry(state: &Rc<State>, entry: &Entry) -> gtk::Box {
    let root = vbox(14);
    root.set_margin_top(28);
    root.set_margin_bottom(64);
    root.set_margin_start(24);
    root.set_margin_end(24);

    // ---- header ----
    let feed = gtk::Label::builder()
        .label(entry.feed_name.to_uppercase())
        .xalign(0.0)
        .wrap(true)
        // In a row beside the icon, a wrapping label only gets its minimum
        // (longest-word) width unless it expands.
        .hexpand(true)
        .css_classes(["reader-feed", "accent"])
        .build();
    let title_markup = match &entry.link {
        Some(l) => format!(r#"<a href="{}">{}</a>"#, markup_escape(l), markup_escape(&entry.title)),
        None => markup_escape(&entry.title),
    };
    let title = text_label(state, &title_markup, &["reader-title"]);
    title.set_selectable(false);
    let icon = gtk::Image::builder().pixel_size(18).build();
    state.icons.apply(&icon, &entry.feed_name);
    let feed_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    feed_row.append(&icon);
    feed_row.append(&feed);
    let header = vbox(6);
    header.append(&feed_row);
    header.append(&title);
    if let Some(d) = entry.date {
        let date = d.with_timezone(&chrono::Local).format("%A, %B %-d, %Y · %-I:%M %p").to_string();
        header.append(&gtk::Label::builder().label(date).xalign(0.0).css_classes(["dim-label"]).build());
    }
    root.append(&header);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    // ---- feed-level media ----
    if let Some(thumb) = &entry.thumbnail {
        if !entry.body_html.contains("<img") {
            let is_video = entry.link.as_deref().is_some_and(|l| youtube_id(l).is_some());
            // Podcast cover art is usually a huge square; keep it modest.
            let max = if entry.audio.is_some() { Some(260) } else { None };
            root.append(&picture(state, thumb.clone(), entry.link.clone(), is_video, max));
        }
    }
    if let Some(audio) = &entry.audio {
        root.append(&audio_player(state, audio));
    }

    // ---- body ----
    let body = vbox(14);
    let base = entry.link.as_deref().and_then(|l| Url::parse(l).ok());
    let mut b = Builder::new(state, base, body.clone());
    let doc = Html::parse_fragment(&entry.body_html);
    b.walk(doc.root_element());
    b.flush();
    if body.first_child().is_none() {
        body.append(
            &gtk::Label::builder()
                .label("This entry has no content.")
                .xalign(0.0)
                .css_classes(["dim-label", "reader-text"])
                .build(),
        );
    }
    root.append(&body);
    root
}

// ---------------------------------------------------------------------------
// HTML → widgets
// ---------------------------------------------------------------------------

struct Builder<'s> {
    state: &'s Rc<State>,
    base: Option<Url>,
    /// Containers being filled; blocks go into the last one.
    stack: Vec<gtk::Box>,
    /// Pango markup for the paragraph being accumulated.
    markup: String,
    has_text: bool,
    pending_space: bool,
    /// Open inline tags (open markup, close markup), re-opened after a flush.
    open: Vec<(String, &'static str)>,
    /// Innermost enclosing link, so images inside <a> are clickable.
    link: Option<String>,
    /// Extra CSS classes for labels flushed while inside e.g. a heading.
    classes: Vec<&'static str>,
    /// Element ids waiting for the next appended widget (footnote targets etc.).
    pending_ids: Vec<String>,
}

impl<'s> Builder<'s> {
    fn new(state: &'s Rc<State>, base: Option<Url>, root: gtk::Box) -> Self {
        Self {
            state,
            base,
            stack: vec![root],
            markup: String::new(),
            has_text: false,
            pending_space: false,
            open: Vec::new(),
            link: None,
            classes: Vec::new(),
            pending_ids: Vec::new(),
        }
    }

    fn resolve(&self, href: &str) -> Option<String> {
        let href = href.trim();
        if href.is_empty() || href.starts_with("javascript:") || href.starts_with("data:") {
            return None;
        }
        match &self.base {
            Some(base) => base.join(href).ok().map(String::from),
            None => Url::parse(href).ok().map(String::from),
        }
    }

    fn append(&mut self, widget: &impl IsA<gtk::Widget>) {
        self.stack.last().expect("container").append(widget);
        let mut anchors = self.state.anchors.borrow_mut();
        for id in self.pending_ids.drain(..) {
            anchors.entry(id).or_insert_with(|| widget.clone().upcast());
        }
    }

    /// Emit the accumulated text (if any) as a label.
    fn flush(&mut self) {
        if self.has_text {
            let mut m = std::mem::take(&mut self.markup);
            while m.ends_with('\n') {
                m.pop();
            }
            for (_, close) in self.open.iter().rev() {
                m.push_str(close);
            }
            let mut classes = vec!["reader-text"];
            if !self.classes.is_empty() {
                classes = self.classes.clone();
            }
            let label = text_label(self.state, &m, &classes);
            self.append(&label);
        }
        self.markup = self.open.iter().map(|(o, _)| o.as_str()).collect();
        self.has_text = false;
        self.pending_space = false;
    }

    fn text(&mut self, t: &str) {
        for c in t.chars() {
            if c.is_whitespace() {
                self.pending_space = true;
                continue;
            }
            if self.pending_space && self.has_text && !self.markup.ends_with('\n') {
                self.markup.push(' ');
            }
            self.pending_space = false;
            match c {
                '&' => self.markup.push_str("&amp;"),
                '<' => self.markup.push_str("&lt;"),
                '>' => self.markup.push_str("&gt;"),
                c => self.markup.push(c),
            }
            self.has_text = true;
        }
    }

    fn inline(&mut self, el: ElementRef, open: String, close: &'static str) {
        // A space before the tag belongs outside it.
        if self.pending_space && self.has_text {
            self.markup.push(' ');
            self.pending_space = false;
        }
        self.markup.push_str(&open);
        self.open.push((open, close));
        self.walk(el);
        self.open.pop();
        self.markup.push_str(close);
    }

    fn block(&mut self, el: ElementRef) {
        self.flush();
        self.walk(el);
        self.flush();
    }

    fn with_classes(&mut self, classes: &[&'static str], el: ElementRef) {
        self.flush();
        let saved = std::mem::replace(&mut self.classes, classes.to_vec());
        self.walk(el);
        self.flush();
        self.classes = saved;
    }

    fn in_container(&mut self, container: gtk::Box, el: ElementRef) {
        self.flush();
        self.append(&container);
        self.stack.push(container);
        self.walk(el);
        self.flush();
        self.stack.pop();
    }

    fn walk(&mut self, el: ElementRef) {
        for child in el.children() {
            match child.value() {
                Node::Text(t) => self.text(t),
                Node::Element(_) => {
                    if let Some(e) = ElementRef::wrap(child) {
                        self.element(e);
                    }
                }
                _ => {}
            }
        }
    }

    fn element(&mut self, el: ElementRef) {
        let e = el.value();
        let name = e.name();
        if let Some(id) = e.id() {
            self.pending_ids.push(id.to_string());
        }
        if name == "a" {
            if let Some(n) = e.attr("name") {
                self.pending_ids.push(n.to_string());
            }
        }
        let has_class = |c: &str| e.classes().any(|x| x == c);

        // Substack: YouTube embeds are a div with the video id in data-attrs.
        if has_class("youtube-wrap") {
            if let Some(id) = e.attr("data-attrs").and_then(|a| json_str_field(a, "videoId")) {
                self.flush();
                let w = youtube_card(self.state, &id);
                self.append(&w);
                return;
            }
        }
        // Substack image overlay buttons / subscribe forms: chrome that needs their JS.
        if has_class("image-link-expand") || has_class("subscription-widget-wrap") {
            return;
        }

        match name {
            "script" | "style" | "noscript" | "button" | "form" | "input" | "svg" | "select" | "textarea"
            | "template" | "head" | "title" | "meta" | "link" | "object" | "embed" | "source" | "track" | "map"
            | "canvas" => {}
            "br" => {
                if self.has_text {
                    self.markup.push('\n');
                }
                self.pending_space = false;
            }
            "b" | "strong" => self.inline(el, "<b>".into(), "</b>"),
            "i" | "em" | "cite" | "var" | "dfn" => self.inline(el, "<i>".into(), "</i>"),
            "u" | "ins" => self.inline(el, "<u>".into(), "</u>"),
            "s" | "del" | "strike" => self.inline(el, "<s>".into(), "</s>"),
            "sub" => self.inline(el, "<sub>".into(), "</sub>"),
            "sup" => self.inline(el, "<sup>".into(), "</sup>"),
            "small" => self.inline(el, "<small>".into(), "</small>"),
            "code" | "kbd" | "samp" | "tt" => self.inline(
                el,
                r##"<span font_family="monospace" background="#888888" bgalpha="22%">"##.into(),
                "</span>",
            ),
            "a" => {
                let href = e.attr("href").and_then(|h| self.resolve(h));
                let nested = self.open.iter().any(|(_, c)| *c == "</a>");
                match href {
                    Some(h) if !nested => {
                        let saved = self.link.replace(h.clone());
                        self.inline(el, format!(r#"<a href="{}">"#, markup_escape(&h)), "</a>");
                        self.link = saved;
                    }
                    _ => self.walk(el),
                }
            }
            "img" => self.image(el),
            "h1" => self.with_classes(&["reader-h1"], el),
            "h2" => self.with_classes(&["reader-h2"], el),
            "h3" => self.with_classes(&["reader-h3"], el),
            "h4" | "h5" | "h6" => self.with_classes(&["reader-h4"], el),
            "figcaption" => self.with_classes(&["reader-caption", "dim-label"], el),
            "ul" | "ol" | "menu" => self.list(el, name == "ol"),
            "blockquote" => {
                let quote = vbox(14);
                quote.add_css_class("reader-quote");
                self.in_container(quote, el);
            }
            "pre" => self.pre(el),
            "hr" => {
                self.flush();
                self.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            }
            "table" => self.table(el),
            "iframe" => {
                self.flush();
                if let Some(src) = e.attr("src").and_then(|s| self.resolve(s)) {
                    let w = match youtube_id(&src) {
                        Some(id) => youtube_card(self.state, &id),
                        None => link_card(self.state, "Embedded content", &src),
                    };
                    self.append(&w);
                }
            }
            "video" => {
                self.flush();
                let src = e.attr("src").map(str::to_string).or_else(|| first_source(el)).and_then(|s| self.resolve(&s));
                let poster = e.attr("poster").and_then(|p| self.resolve(p));
                if let Some(src) = src {
                    let w = match poster {
                        Some(p) => picture(self.state, p, Some(src), true, None),
                        None => link_card(self.state, "Video", &src),
                    };
                    self.append(&w);
                }
            }
            "audio" => {
                self.flush();
                let src = e.attr("src").map(str::to_string).or_else(|| first_source(el)).and_then(|s| self.resolve(&s));
                if let Some(src) = src {
                    let w = audio_player(self.state, &src);
                    self.append(&w);
                }
            }
            "p" | "div" | "section" | "article" | "header" | "footer" | "main" | "aside" | "nav" | "center"
            | "details" | "summary" | "dl" | "dt" | "dd" | "address" | "figure" | "picture" | "hgroup" | "li"
            | "tr" | "tbody" | "thead" | "caption" => self.block(el),
            _ => self.walk(el),
        }
    }

    fn image(&mut self, el: ElementRef) {
        let e = el.value();
        let Some(src) = e
            .attr("src")
            .or_else(|| e.attr("data-src"))
            .and_then(|s| self.resolve(s))
        else {
            return;
        };
        let dim = |n: &str| e.attr(n).and_then(|v| v.trim_end_matches("px").parse::<i32>().ok());
        let (w, h) = (dim("width"), dim("height"));
        // Tracking pixels.
        if w.is_some_and(|w| w <= 2) || h.is_some_and(|h| h <= 2) {
            return;
        }
        self.flush();
        // Respect explicit small widths (icons, inline figures); otherwise fill the column.
        let max = w.filter(|w| *w < 600);
        let pic = picture(self.state, src, self.link.clone(), false, max);
        if let Some(alt) = e.attr("alt").filter(|a| !a.trim().is_empty()) {
            pic.set_tooltip_text(Some(alt));
        }
        self.append(&pic);
    }

    fn list(&mut self, el: ElementRef, ordered: bool) {
        self.flush();
        let list = vbox(8);
        list.set_margin_start(4);
        self.append(&list);
        self.stack.push(list);
        let mut n: i64 = el.value().attr("start").and_then(|s| s.parse().ok()).unwrap_or(1);
        for child in el.children() {
            let Some(c) = ElementRef::wrap(child) else {
                if let Node::Text(t) = child.value() {
                    self.text(t);
                }
                continue;
            };
            if c.value().name() != "li" {
                self.element(c);
                continue;
            }
            self.flush();
            let marker = gtk::Label::builder()
                .label(if ordered { format!("{n}.") } else { "•".to_string() })
                .valign(gtk::Align::Start)
                .xalign(1.0)
                .width_chars(if ordered { 3 } else { 2 })
                .css_classes(["reader-text", "dim-label"])
                .build();
            let item = vbox(8);
            item.set_hexpand(true);
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.append(&marker);
            row.append(&item);
            self.append(&row);
            self.stack.push(item);
            self.walk(c);
            self.flush();
            self.stack.pop();
            n += 1;
        }
        self.flush();
        self.stack.pop();
    }

    fn pre(&mut self, el: ElementRef) {
        self.flush();
        let text: String = el.text().collect();
        let label = gtk::Label::builder()
            .label(text.trim_end_matches('\n'))
            .xalign(0.0)
            .selectable(true)
            .css_classes(["monospace"])
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(&label)
            .css_classes(["reader-code"])
            .build();
        self.append(&scroller);
    }

    fn table(&mut self, el: ElementRef) {
        self.flush();
        let grid = gtk::Grid::builder().column_spacing(18).row_spacing(8).build();
        let mut row = 0;
        // Rows may sit directly in <table> or in thead/tbody/tfoot.
        let mut rows: Vec<ElementRef> = Vec::new();
        for child in el.children().filter_map(ElementRef::wrap) {
            match child.value().name() {
                "tr" => rows.push(child),
                "thead" | "tbody" | "tfoot" => {
                    rows.extend(child.children().filter_map(ElementRef::wrap).filter(|r| r.value().name() == "tr"))
                }
                _ => {}
            }
        }
        for tr in rows {
            let mut col = 0;
            for cell in tr.children().filter_map(ElementRef::wrap) {
                let header = cell.value().name() == "th";
                if !header && cell.value().name() != "td" {
                    continue;
                }
                let cell_box = vbox(6);
                cell_box.set_valign(gtk::Align::Start);
                grid.attach(&cell_box, col, row, 1, 1);
                self.stack.push(cell_box);
                if header {
                    self.with_classes(&["reader-text", "heading"], cell);
                } else {
                    self.block(cell);
                }
                self.stack.pop();
                col += 1;
            }
            row += 1;
        }
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(&grid)
            .build();
        self.append(&scroller);
    }
}

fn first_source(el: ElementRef) -> Option<String> {
    el.children()
        .filter_map(ElementRef::wrap)
        .find(|c| c.value().name() == "source")
        .and_then(|c| c.value().attr("src").map(str::to_string))
}

/// Pull `"key":"value"` out of a JSON-ish attribute without a JSON parser.
fn json_str_field(json: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let start = json.find(&pat)? + pat.len();
    let end = json[start..].find('"')?;
    Some(json[start..start + end].to_string()).filter(|s| !s.is_empty())
}

fn youtube_id(url: &str) -> Option<String> {
    let u = Url::parse(url).ok()?;
    let host = u.host_str()?.trim_start_matches("www.").trim_start_matches("m.");
    let id = match host {
        "youtube.com" | "youtube-nocookie.com" => {
            let mut seg = u.path_segments()?;
            match seg.next()? {
                "embed" | "shorts" | "live" | "v" => seg.next().map(str::to_string),
                "watch" => u.query_pairs().find(|(k, _)| k == "v").map(|(_, v)| v.into_owned()),
                _ => None,
            }
        }
        "youtu.be" => u.path_segments()?.next().map(str::to_string),
        _ => None,
    }?;
    Some(id).filter(|i| !i.is_empty() && i.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
}

// ---------------------------------------------------------------------------
// Widgets
// ---------------------------------------------------------------------------

fn clickable(state: &Rc<State>, widget: &impl IsA<gtk::Widget>, uri: String) {
    widget.set_cursor_from_name(Some("pointer"));
    widget.set_tooltip_text(Some(&uri));
    let click = gtk::GestureClick::new();
    let weak: Weak<State> = Rc::downgrade(state);
    click.connect_released(move |g, _, _, _| {
        g.set_state(gtk::EventSequenceState::Claimed);
        if let Some(state) = weak.upgrade() {
            state.open(&uri);
        }
    });
    widget.add_controller(click);
}

/// An image loaded in the background. Hidden until it arrives (and forever if it fails).
fn picture(state: &Rc<State>, url: String, link: Option<String>, play_badge: bool, max_width: Option<i32>) -> gtk::Widget {
    let pic = gtk::Picture::builder().can_shrink(true).content_fit(gtk::ContentFit::Contain).build();
    let overlay = gtk::Overlay::builder().child(&pic).build();
    if play_badge {
        let badge = gtk::Image::builder()
            .icon_name("media-playback-start-symbolic")
            .pixel_size(28)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .css_classes(["osd", "reader-play"])
            .build();
        overlay.add_overlay(&badge);
    }
    let frame = gtk::Box::builder()
        .halign(gtk::Align::Center)
        .overflow(gtk::Overflow::Hidden)
        .css_classes(["reader-image"])
        .visible(false)
        .build();
    frame.append(&overlay);
    if let Some(link) = link {
        clickable(state, &frame, link);
    }
    load_texture(url, &frame, &pic);

    match max_width {
        Some(max) => {
            let clamp = adw::Clamp::builder().maximum_size(max).tightening_threshold(max).child(&frame).build();
            // Keep the clamp hidden along with the frame so a failed image leaves no gap.
            frame.bind_property("visible", &clamp, "visible").sync_create().build();
            clamp.upcast()
        }
        None => frame.upcast(),
    }
}

fn load_texture(url: String, frame: &gtk::Box, pic: &gtk::Picture) {
    if let Some(tex) = IMAGE_CACHE.with(|c| c.borrow().get(&url).cloned()) {
        pic.set_paintable(Some(&tex));
        frame.set_visible(true);
        return;
    }
    let (tx, rx) = async_channel::bounded(1);
    let fetch_url = url.clone();
    std::thread::spawn(move || {
        let _ = tx.send_blocking(decode_image(&fetch_url));
    });
    let (frame, pic) = (frame.downgrade(), pic.downgrade());
    glib::spawn_future_local(async move {
        let Ok(Ok(tex)) = rx.recv().await else { return };
        IMAGE_CACHE.with(|c| {
            let mut c = c.borrow_mut();
            if c.len() >= IMAGE_CACHE_CAP {
                c.clear();
            }
            c.insert(url, tex.clone());
        });
        if let (Some(frame), Some(pic)) = (frame.upgrade(), pic.upgrade()) {
            frame.set_visible(true);
            pic.set_paintable(Some(&tex));
        }
    });
}

/// Download + decode off the main thread; only width is capped so tall images stay sharp.
fn decode_image(url: &str) -> Result<gdk::Texture, String> {
    crate::images::decode(url, MAX_IMAGE_PX, i32::MAX)
}

fn youtube_card(state: &Rc<State>, id: &str) -> gtk::Widget {
    picture(
        state,
        format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"),
        Some(format!("https://www.youtube.com/watch?v={id}")),
        true,
        None,
    )
}

/// A card for content we can't render inline; opens in the browser.
fn link_card(state: &Rc<State>, title: &str, url: &str) -> gtk::Widget {
    let host = Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    let icon = gtk::Image::builder().icon_name("adw-external-link-symbolic").pixel_size(20).build();
    let text = vbox(2);
    text.append(&gtk::Label::builder().label(title).xalign(0.0).css_classes(["heading"]).build());
    text.append(
        &gtk::Label::builder()
            .label(host)
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.append(&icon);
    row.append(&text);
    let card = gtk::Box::builder().css_classes(["card", "activatable", "reader-embed"]).build();
    card.append(&row);
    clickable(state, &card, url.to_string());
    card.upcast()
}

/// Owns a GStreamer pipeline; shuts it down when the player widgets go away.
struct Player {
    url: String,
    pipeline: RefCell<Option<gst::Element>>,
    playing: std::cell::Cell<bool>,
}

impl Drop for Player {
    fn drop(&mut self) {
        if let Some(p) = self.pipeline.get_mut().take() {
            let _ = p.set_state(gst::State::Null);
        }
    }
}

fn fmt_time(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// Minimal audio player (play/pause, seek bar, times) on a classic GStreamer
/// `playbin`. Nothing is fetched until Play is pressed.
fn audio_player(state: &Rc<State>, url: &str) -> gtk::Widget {
    let play = gtk::Button::builder()
        .icon_name("media-playback-start-symbolic")
        .tooltip_text("Play")
        .valign(gtk::Align::Center)
        .css_classes(["circular", "suggested-action"])
        .build();
    let pos = gtk::Label::builder().label("0:00").width_chars(7).xalign(1.0).css_classes(["numeric", "dim-label"]).build();
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 1.0);
    scale.set_hexpand(true);
    scale.set_sensitive(false);
    let dur = gtk::Label::builder().label("--:--").width_chars(7).xalign(0.0).css_classes(["numeric", "dim-label"]).build();
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.append(&play);
    row.append(&pos);
    row.append(&scale);
    row.append(&dur);
    let card = gtk::Box::builder().css_classes(["card", "reader-embed"]).build();
    card.append(&row);
    let error = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).css_classes(["error"]).build();
    let holder = vbox(6);
    holder.append(&card);
    holder.append(&error);

    let player = Rc::new(Player { url: url.to_string(), pipeline: RefCell::default(), playing: Default::default() });

    let set_playing = {
        let play = play.downgrade();
        move |player: &Player, playing: bool| {
            player.playing.set(playing);
            if let Some(play) = play.upgrade() {
                play.set_icon_name(if playing { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" });
                play.set_tooltip_text(Some(if playing { "Pause" } else { "Play" }));
            }
        }
    };

    play.connect_clicked({
        let player = player.clone();
        let weak_state = Rc::downgrade(state);
        let (scale, pos, dur, error) = (scale.downgrade(), pos.downgrade(), dur.downgrade(), error.downgrade());
        let set_playing = set_playing.clone();
        move |_| {
            if player.pipeline.borrow().is_none() {
                let pipeline = match gst::ElementFactory::make("playbin").property("uri", &player.url).build() {
                    Ok(p) => p,
                    Err(e) => {
                        if let Some(error) = error.upgrade() {
                            error.set_text(&format!("Can't play audio: {e}"));
                            error.set_visible(true);
                        }
                        return;
                    }
                };
                // Some enclosures are video; never pop up a video window.
                if let Ok(sink) = gst::ElementFactory::make("fakesink").build() {
                    pipeline.set_property("video-sink", &sink);
                }
                if let Some(st) = weak_state.upgrade() {
                    st.players.borrow_mut().push(pipeline.downgrade());
                }
                *player.pipeline.borrow_mut() = Some(pipeline);

                // Poll position/duration and bus messages while the widgets live.
                let weak_player = Rc::downgrade(&player);
                let (scale, pos, dur, error) = (scale.clone(), pos.clone(), dur.clone(), error.clone());
                let set_playing = set_playing.clone();
                glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
                    let (Some(player), Some(scale), Some(pos), Some(dur)) =
                        (weak_player.upgrade(), scale.upgrade(), pos.upgrade(), dur.upgrade())
                    else {
                        return glib::ControlFlow::Break;
                    };
                    let Some(p) = player.pipeline.borrow().clone() else { return glib::ControlFlow::Break };
                    if let Some(d) = p.query_duration::<gst::ClockTime>() {
                        let d = d.seconds();
                        if d > 0 && (scale.adjustment().upper() - d as f64).abs() > 0.5 {
                            scale.set_range(0.0, d as f64);
                            scale.set_sensitive(true);
                            dur.set_text(&fmt_time(d));
                        }
                    }
                    if let Some(t) = p.query_position::<gst::ClockTime>() {
                        scale.set_value(t.seconds() as f64);
                        pos.set_text(&fmt_time(t.seconds()));
                    }
                    if let Some(bus) = p.bus() {
                        while let Some(msg) = bus.pop_filtered(&[gst::MessageType::Error, gst::MessageType::Eos]) {
                            match msg.view() {
                                gst::MessageView::Error(e) => {
                                    let _ = p.set_state(gst::State::Null);
                                    set_playing(&player, false);
                                    if let Some(error) = error.upgrade() {
                                        error.set_text(&format!("Playback failed: {}", e.error()));
                                        error.set_visible(true);
                                    }
                                }
                                gst::MessageView::Eos(_) => {
                                    let _ = p.set_state(gst::State::Paused);
                                    let _ = p.seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::ZERO);
                                    set_playing(&player, false);
                                }
                                _ => {}
                            }
                        }
                    }
                    glib::ControlFlow::Continue
                });
            }
            let Some(p) = player.pipeline.borrow().clone() else { return };
            let playing = !player.playing.get();
            let _ = p.set_state(if playing { gst::State::Playing } else { gst::State::Paused });
            set_playing(&player, playing);
        }
    });

    scale.connect_change_value({
        let player = player.clone();
        move |_, _, value| {
            if let Some(p) = player.pipeline.borrow().as_ref() {
                let _ = p.seek_simple(
                    gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                    gst::ClockTime::from_seconds(value.max(0.0) as u64),
                );
            }
            glib::Propagation::Proceed
        }
    });

    holder.upcast()
}
