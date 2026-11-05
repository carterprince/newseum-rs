//! Per-feed icons: the feed's own icon/logo if it has one, else its website's
//! favicon (so every YouTube feed gets YouTube's icon). Fetched in the
//! background once per session and kept in memory only.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};
use scraper::{Html, Selector};
use url::Url;

use crate::feeds::{self, FeedMeta};

/// Icons are shown at ≤20px; 64px covers 2x/3x displays.
const ICON_PX: i32 = 64;
const FALLBACK_ICON: &str = "application-rss+xml-symbolic";

#[derive(Default)]
pub struct Icons {
    /// Feed name → icon.
    textures: RefCell<HashMap<String, gdk::Texture>>,
    /// Feeds we've already started (or finished) looking up this session.
    requested: RefCell<HashSet<String>>,
    /// Lookup key (icon URL or site host) → feeds waiting on it, so feeds
    /// sharing a site (e.g. all of YouTube) cost one download.
    jobs: RefCell<HashMap<String, Vec<String>>>,
    /// Image widgets showing feed icons; each carries its feed in `widget-name`.
    images: RefCell<Vec<glib::WeakRef<gtk::Image>>>,
}

impl Icons {
    pub fn new() -> Rc<Self> {
        Rc::default()
    }

    /// Show `feed`'s icon in `image` now, and keep it updated if the icon arrives later.
    pub fn apply(&self, image: &gtk::Image, feed: &str) {
        image.set_widget_name(feed);
        match self.textures.borrow().get(feed) {
            Some(tex) => {
                image.remove_css_class("dim-label");
                image.set_paintable(Some(tex));
            }
            None => {
                image.add_css_class("dim-label");
                image.set_icon_name(Some(FALLBACK_ICON));
            }
        }
        let mut images = self.images.borrow_mut();
        images.retain(|w| w.upgrade().is_some());
        if !images.iter().any(|w| w.upgrade().as_ref() == Some(image)) {
            images.push(image.downgrade());
        }
    }

    /// Start looking up an icon for `feed` unless we already have.
    pub fn request(self: &Rc<Self>, feed: &str, meta: &FeedMeta) {
        if !self.requested.borrow_mut().insert(feed.to_string()) {
            return;
        }
        let key = match &meta.icon {
            Some(icon) => format!("icon:{icon}"),
            None => format!("favicon:{}", host_of(&meta.site).unwrap_or_default()),
        };
        let mut jobs = self.jobs.borrow_mut();
        if let Some(waiting) = jobs.get_mut(&key) {
            waiting.push(feed.to_string());
            return;
        }
        jobs.insert(key.clone(), vec![feed.to_string()]);
        drop(jobs);

        let meta = meta.clone();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(resolve(&meta));
        });
        let this = self.clone();
        glib::spawn_future_local(async move {
            let tex = rx.recv().await.ok().flatten();
            let feeds = this.jobs.borrow_mut().remove(&key).unwrap_or_default();
            let Some(tex) = tex else { return };
            {
                let mut textures = this.textures.borrow_mut();
                for f in &feeds {
                    textures.insert(f.clone(), tex.clone());
                }
            }
            for img in this.images.borrow().iter().filter_map(|w| w.upgrade()) {
                if feeds.iter().any(|f| *f == img.widget_name()) {
                    img.remove_css_class("dim-label");
                    img.set_paintable(Some(&tex));
                }
            }
        });
    }
}

fn host_of(url: &str) -> Option<String> {
    Url::parse(url).ok()?.host_str().map(str::to_string)
}

/// Blocking: find and decode the best icon for a feed.
fn resolve(meta: &FeedMeta) -> Option<gdk::Texture> {
    if let Some(icon) = &meta.icon {
        if let Ok(tex) = crate::images::decode(icon, ICON_PX, ICON_PX) {
            return Some(tex);
        }
    }
    favicon_candidates(&meta.site)
        .iter()
        .find_map(|url| crate::images::decode(url, ICON_PX, ICON_PX).ok())
}

/// Icon URLs advertised by the site's homepage, biggest first, then /favicon.ico.
fn favicon_candidates(site: &str) -> Vec<String> {
    let Ok(site) = Url::parse(site) else { return Vec::new() };
    let Some(host) = site.host_str() else { return Vec::new() };
    let Ok(home) = Url::parse(&format!("{}://{}/", site.scheme(), host)) else { return Vec::new() };

    let mut scored: Vec<(u32, String)> = Vec::new();
    if let Ok(bytes) = feeds::download(home.as_str()) {
        let html = Html::parse_document(&String::from_utf8_lossy(&bytes));
        let sel = Selector::parse("link[rel][href]").expect("valid selector");
        for link in html.select(&sel) {
            let e = link.value();
            let rel = e.attr("rel").unwrap_or("").to_ascii_lowercase();
            let rels: Vec<&str> = rel.split_whitespace().collect();
            if !rels.iter().any(|r| *r == "icon" || *r == "apple-touch-icon" || *r == "apple-touch-icon-precomposed") {
                continue;
            }
            let Some(href) = e.attr("href").and_then(|h| home.join(h.trim()).ok()) else { continue };
            // Prefer the largest declared size; apple-touch icons are usually 180px.
            let declared = e
                .attr("sizes")
                .unwrap_or("")
                .split_whitespace()
                .filter_map(|s| if s == "any" { Some(512) } else { s.split('x').next()?.parse().ok() })
                .max();
            let score = declared.unwrap_or(if rels.iter().any(|r| r.starts_with("apple-touch")) { 180 } else { 16 });
            scored.push((score, href.to_string()));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out: Vec<String> = scored.into_iter().map(|(_, u)| u).collect();
    if let Ok(ico) = home.join("/favicon.ico") {
        out.push(ico.to_string());
    }
    out.dedup();
    out
}
