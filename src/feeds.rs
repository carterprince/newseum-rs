//! Feed list persistence (`~/.config/newseum/feeds.csv`) and network fetching.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use chrono::{DateTime, Utc};

const USER_AGENT: &str = concat!("newseum/", env!("CARGO_PKG_VERSION"), " (desktop RSS reader)");
const YOUTUBE_FEED: &str = "https://www.youtube.com/feeds/videos.xml";

/// The URL to store for a feed the user entered, and a name for it if it has one.
pub struct Resolved {
    pub url: String,
    pub title: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Feed {
    pub name: String,
    pub url: String,
}

/// A single article, flattened from whatever the feed gave us.
#[derive(Clone, Debug)]
pub struct Entry {
    pub feed_name: String,
    pub title: String,
    pub link: Option<String>,
    pub date: Option<DateTime<Utc>>,
    /// HTML body to show in the reader pane (already escaped if it was plain text).
    pub body_html: String,
    pub thumbnail: Option<String>,
    /// Audio enclosure (podcasts).
    pub audio: Option<String>,
}

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    base.join("newseum").join("feeds.csv")
}

pub fn load_feeds() -> Result<Vec<Feed>, String> {
    let path = config_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .trim(csv::Trim::All)
        .from_path(&path)
        .map_err(|e| format!("Could not open {}: {e}", path.display()))?;
    let mut feeds = Vec::new();
    for rec in rdr.records() {
        let rec = rec.map_err(|e| format!("Could not parse {}: {e}", path.display()))?;
        let name = rec.get(0).unwrap_or("").to_string();
        let url = rec.get(1).unwrap_or("").to_string();
        if !url.is_empty() {
            feeds.push(Feed { name, url });
        }
    }
    Ok(feeds)
}

pub fn save_feeds(feeds: &[Feed]) -> Result<(), String> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    }
    // Write to a temp file and rename so a crash never leaves a half-written list.
    let tmp = path.with_extension("csv.tmp");
    {
        let mut w = csv::WriterBuilder::new()
            .has_headers(false)
            .from_path(&tmp)
            .map_err(|e| format!("Could not write {}: {e}", tmp.display()))?;
        for f in feeds {
            w.write_record([&f.name, &f.url]).map_err(|e| e.to_string())?;
        }
        w.flush().map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("Could not replace {}: {e}", path.display()))
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .user_agent(USER_AGENT)
            .timeout_global(Some(Duration::from_secs(15)))
            .http_status_as_error(false)
            .build()
            .into()
    })
}

/// GET a URL. No retries: a rate-limited or slow feed simply fails this refresh.
pub fn download(url: &str) -> Result<Vec<u8>, String> {
    let mut resp = agent().get(url).call().map_err(|e| e.to_string())?;
    let status = resp.status();
    if status.as_u16() == 429 {
        return Err("rate limited by the server".into());
    }
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    resp.body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| e.to_string())
}

/// Elements some feeds (notably YouTube) emit empty, which feed-rs rejects.
const OFTEN_EMPTY: &[&str] = &[
    "media:description",
    "media:title",
    "media:keywords",
    "description",
    "summary",
    "title",
    "itunes:summary",
    "itunes:subtitle",
    "content:encoded",
];

/// Remove `<tag/>`, `<tag ...></tag>` and `<tag>   </tag>` for the tags above.
fn strip_empty_elements(xml: &str) -> String {
    let mut s = xml.to_string();
    for tag in OFTEN_EMPTY {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        let mut out = String::with_capacity(s.len());
        let mut rest = s.as_str();
        while let Some(pos) = rest.find(&open) {
            let after = &rest[pos + open.len()..];
            let boundary = after.chars().next().is_some_and(|c| c == '>' || c == '/' || c.is_whitespace());
            let Some(gt) = after.find('>').filter(|_| boundary) else {
                out.push_str(&rest[..pos + open.len()]);
                rest = after;
                continue;
            };
            let end_of_open = pos + open.len() + gt + 1;
            if after[..gt].ends_with('/') {
                out.push_str(&rest[..pos]);
                rest = &rest[end_of_open..];
                continue;
            }
            let tail = &rest[end_of_open..];
            let trimmed = tail.trim_start();
            if trimmed.starts_with(&close) {
                out.push_str(&rest[..pos]);
                rest = &trimmed[close.len()..];
            } else {
                out.push_str(&rest[..end_of_open]);
                rest = tail;
            }
        }
        out.push_str(rest);
        s = out;
    }
    s
}

fn parse(bytes: &[u8]) -> Result<feed_rs::model::Feed, String> {
    match feed_rs::parser::parse(bytes) {
        Ok(f) => Ok(f),
        Err(first) => {
            let cleaned = strip_empty_elements(&String::from_utf8_lossy(bytes));
            feed_rs::parser::parse(cleaned.as_bytes()).map_err(|_| format!("Not a valid feed: {first}"))
        }
    }
}

/// Feed-level metadata used to find an icon.
#[derive(Clone, Debug)]
pub struct FeedMeta {
    /// Icon or logo the feed itself advertises.
    pub icon: Option<String>,
    /// The feed's website (falls back to the feed URL), for favicon lookup.
    pub site: String,
}

pub struct Fetched {
    pub entries: Vec<Entry>,
    pub meta: FeedMeta,
}

/// Download and parse a feed. Blocking; call from a worker thread.
pub fn fetch(feed: &Feed) -> Result<Fetched, String> {
    let bytes = download(&feed.url)?;
    let parsed = parse(&bytes)?;
    let icon = parsed.icon.as_ref().or(parsed.logo.as_ref()).map(|i| i.uri.trim().to_string()).filter(|u| u.starts_with("http"));
    let site = parsed
        .links
        .iter()
        .find(|l| l.rel.as_deref().is_none_or(|r| r == "alternate") && l.href.starts_with("http"))
        .map(|l| l.href.clone())
        .unwrap_or_else(|| feed.url.clone());
    let entries = parsed.entries.into_iter().map(|e| convert(&feed.name, e)).collect();
    Ok(Fetched { entries, meta: FeedMeta { icon, site } })
}

/// Check that a URL serves a parseable feed. YouTube channel links are turned into
/// the channel's videos-only (no Shorts) playlist feed first.
pub fn resolve(url: &str) -> Result<Resolved, String> {
    let id = match youtube_channel(url) {
        None => return load_resolved(url, feed_title),
        Some(YouTubeChannel::Id(id)) => id,
        Some(YouTubeChannel::OnPage) => {
            let page = download(url)?;
            channel_id_in_page(&String::from_utf8_lossy(&page))
                .ok_or("couldn't find a YouTube channel on that page")?
        }
    };
    // Every playlist feed is titled "Videos", so name it after its author.
    // Channels with no regular videos may have no playlist; use the full feed.
    load_resolved(&format!("{YOUTUBE_FEED}?playlist_id=UULF{}", &id[2..]), feed_author)
        .or_else(|_| load_resolved(&format!("{YOUTUBE_FEED}?channel_id={id}"), feed_title))
}

fn load_resolved(url: &str, name: fn(&feed_rs::model::Feed) -> Option<String>) -> Result<Resolved, String> {
    let parsed = parse(&download(url)?)?;
    Ok(Resolved { url: url.to_string(), title: name(&parsed) })
}

fn feed_title(feed: &feed_rs::model::Feed) -> Option<String> {
    feed.title.as_ref().map(|t| t.content.trim().to_string()).filter(|t| !t.is_empty())
}

fn feed_author(feed: &feed_rs::model::Feed) -> Option<String> {
    feed.authors.first()?.name.as_deref().map(str::trim).filter(|a| !a.is_empty()).map(str::to_string)
}

enum YouTubeChannel {
    Id(String),
    /// The channel ID has to be read from the page at this URL (e.g. `/@handle`).
    OnPage,
}

fn is_channel_id(id: &str) -> bool {
    id.len() == 24 && id.starts_with("UC") && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Recognise the ways of linking to a YouTube channel, including its feeds.
fn youtube_channel(url: &str) -> Option<YouTubeChannel> {
    let parsed = url::Url::parse(url).ok()?;
    if !matches!(parsed.host_str()?, "youtube.com" | "www.youtube.com" | "m.youtube.com") {
        return None;
    }
    let mut path = parsed.path_segments()?;
    let first = path.next()?;
    if first == "feeds" {
        let (key, value) = parsed.query_pairs().find(|(k, _)| k == "channel_id" || k == "playlist_id")?;
        let id = match &*key {
            "channel_id" => value.to_string(),
            _ => format!("UC{}", value.strip_prefix("UULF")?),
        };
        return is_channel_id(&id).then_some(YouTubeChannel::Id(id));
    }
    if first == "channel" {
        if let Some(id) = path.next().filter(|id| is_channel_id(id)) {
            return Some(YouTubeChannel::Id(id.to_string()));
        }
    }
    (first.starts_with('@') || matches!(first, "channel" | "c" | "user")).then_some(YouTubeChannel::OnPage)
}

/// The page's own channel ID. Other IDs on the page may belong to other channels.
fn channel_id_in_page(html: &str) -> Option<String> {
    ["\"externalId\":\"", "feeds/videos.xml?channel_id="].iter().find_map(|marker| {
        let start = html.find(marker)? + marker.len();
        let id = html[start..].get(..24)?;
        is_channel_id(id).then(|| id.to_string())
    })
}

fn is_html(ct: &impl std::fmt::Display) -> bool {
    let s = ct.to_string();
    s.contains("html")
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn plain_to_html(s: &str) -> String {
    s.trim()
        .split("\n\n")
        .map(|p| format!("<p>{}</p>", linkify(p).replace('\n', "<br>")))
        .collect()
}

/// Escape plain text, turning bare http(s) URLs into links.
fn linkify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("http://").into_iter().chain(rest.find("https://")).min() {
        out.push_str(&escape(&rest[..pos]));
        let tail = &rest[pos..];
        let end = tail.find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"')).unwrap_or(tail.len());
        // Don't swallow trailing punctuation like "see https://x.com/foo."
        let url = tail[..end].trim_end_matches(['.', ',', ')', ';', ':', '!', '?', '\'']);
        let url = if url.is_empty() { &tail[..end] } else { url };
        out.push_str(&format!(r#"<a href="{0}">{0}</a>"#, escape(url)));
        rest = &tail[url.len()..];
    }
    out.push_str(&escape(rest));
    out
}

fn text_to_html(t: &feed_rs::model::Text) -> String {
    if is_html(&t.content_type) {
        t.content.clone()
    } else {
        plain_to_html(&t.content)
    }
}

fn convert(feed_name: &str, e: feed_rs::model::Entry) -> Entry {
    let title = e
        .title
        .as_ref()
        .map(|t| t.content.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "(untitled)".to_string());

    let link = e
        .links
        .iter()
        .find(|l| l.rel.as_deref().is_none_or(|r| r == "alternate"))
        .or_else(|| e.links.first())
        .map(|l| l.href.clone());

    let body_html = e
        .content
        .as_ref()
        .and_then(|c| {
            c.body.as_ref().filter(|b| !b.trim().is_empty()).map(|b| {
                if is_html(&c.content_type) { b.clone() } else { plain_to_html(b) }
            })
        })
        .or_else(|| e.summary.as_ref().filter(|s| !s.content.trim().is_empty()).map(text_to_html))
        .or_else(|| {
            e.media
                .iter()
                .find_map(|m| m.description.as_ref().filter(|d| !d.content.trim().is_empty()))
                .map(text_to_html)
        })
        .unwrap_or_default();

    let thumbnail = e
        .media
        .iter()
        .flat_map(|m| m.thumbnails.iter())
        .next()
        .map(|t| t.image.uri.clone());

    let audio = e
        .media
        .iter()
        .flat_map(|m| m.content.iter())
        .find(|c| c.content_type.as_ref().is_some_and(|t| t.to_string().starts_with("audio/")))
        .and_then(|c| c.url.as_ref())
        .map(|u| u.to_string())
        .or_else(|| {
            e.links
                .iter()
                .find(|l| l.media_type.as_deref().is_some_and(|t| t.starts_with("audio/")))
                .map(|l| l.href.clone())
        });

    Entry {
        feed_name: feed_name.to_string(),
        title,
        link,
        date: e.published.or(e.updated),
        body_html,
        thumbnail,
        audio,
    }
}

/// Compact relative time: "now", "5m", "3h", "2d", "3w", "4mo", "2y".
pub fn relative_date(date: Option<DateTime<Utc>>) -> String {
    let Some(d) = date else { return String::new() };
    let secs = (Utc::now() - d).num_seconds().max(0);
    let (m, h, day) = (60, 3600, 86400);
    match secs {
        s if s < m => "now".into(),
        s if s < h => format!("{}m", s / m),
        s if s < day => format!("{}h", s / h),
        s if s < 7 * day => format!("{}d", s / day),
        s if s < 30 * day => format!("{}w", s / (7 * day)),
        s if s < 365 * day => format!("{}mo", s / (30 * day)),
        s => format!("{}y", s / (365 * day)),
    }
}

#[cfg(test)]
mod tests {
    use super::{channel_id_in_page, resolve, youtube_channel, YouTubeChannel};

    const ID: &str = "UCpsrJufAUNXwot-znspubbg";

    fn known_id(url: &str) -> Option<String> {
        match youtube_channel(url) {
            Some(YouTubeChannel::Id(id)) => Some(id),
            _ => None,
        }
    }

    #[test]
    fn recognises_channel_ids_in_urls() {
        for url in [
            format!("https://www.youtube.com/channel/{ID}"),
            format!("https://m.youtube.com/channel/{ID}/videos"),
            format!("https://www.youtube.com/feeds/videos.xml?channel_id={ID}"),
            "https://www.youtube.com/feeds/videos.xml?playlist_id=UULFpsrJufAUNXwot-znspubbg".to_string(),
        ] {
            assert_eq!(known_id(&url).as_deref(), Some(ID), "{url}");
        }
    }

    #[test]
    fn looks_up_channel_pages() {
        for url in ["https://www.youtube.com/@untimelyreflections/", "https://youtube.com/c/foo", "https://youtube.com/user/foo"] {
            assert!(matches!(youtube_channel(url), Some(YouTubeChannel::OnPage)), "{url}");
        }
    }

    #[test]
    fn leaves_other_urls_alone() {
        for url in [
            "https://example.com/@someone",
            "https://www.youtube.com/watch?v=abc",
            "https://www.youtube.com/feeds/videos.xml?playlist_id=PLabc",
            "https://notyoutube.com/@someone",
        ] {
            assert!(youtube_channel(url).is_none(), "{url}");
        }
    }

    #[test]
    fn finds_channel_id_in_page() {
        assert_eq!(channel_id_in_page(&format!(r#"x"externalId":"{ID}",y"#)).as_deref(), Some(ID));
        assert_eq!(channel_id_in_page(&format!("feeds/videos.xml?channel_id={ID}\"")).as_deref(), Some(ID));
        assert_eq!(channel_id_in_page(r#""externalId":"nope""#), None);
    }

    /// `cargo test resolves_youtube -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn resolves_youtube_channel_page() {
        let r = resolve("https://www.youtube.com/@untimelyreflections/").unwrap();
        println!("{:?} {}", r.title, r.url);
        assert_eq!(r.url, "https://www.youtube.com/feeds/videos.xml?playlist_id=UULFpsrJufAUNXwot-znspubbg");
        assert_eq!(r.title.as_deref(), Some("essentialsalts"));
    }

    /// Network smoke test against the user's real feed list: `cargo test -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn fetch_all_configured_feeds() {
        for feed in super::load_feeds().unwrap() {
            match super::fetch(&feed).map(|f| f.entries) {
                Ok(entries) => {
                    let with_body = entries.iter().filter(|e| !e.body_html.is_empty()).count();
                    let dated = entries.iter().filter(|e| e.date.is_some()).count();
                    let audio = entries.iter().filter(|e| e.audio.is_some()).count();
                    let thumb = entries.iter().filter(|e| e.thumbnail.is_some()).count();
                    println!(
                        "OK   {:<36} {:>4} entries  body:{with_body:<4} dated:{dated:<4} audio:{audio:<4} thumb:{thumb:<4} first: {}",
                        feed.name,
                        entries.len(),
                        entries.first().map(|e| e.title.as_str()).unwrap_or("-")
                    );
                }
                Err(e) => println!("FAIL {:<36} {e}", feed.name),
            }
        }
    }
}
