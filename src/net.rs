//! Network streams: URL handling, the HTTP fetch behind podcast feeds and the radio
//! directory, and parsers for both. Playback itself goes through FFmpeg's protocols
//! (see `session.rs`); this module is everything around it.
//!
//! A stream is stored as a `PathBuf` holding its URL wherever the rest of the app
//! expects a file path; [`is_url_path`] tells the two apart.

use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::Duration;

/// Where the radio directory comes from: the community run Radio Browser, no key needed.
const RADIO_DIRECTORY: &str = "https://de1.api.radio-browser.info/xml/stations/topvote/100";
/// Feeds and directories bigger than this are not worth parsing.
const MAX_FETCH: usize = 4 << 20;

/// The schemes FFmpeg can open as a stream here, the same as the Windows build's slim FFmpeg
/// (`scripts/build-ffmpeg.sh`) carries protocols for, so a saved library means the same on both.
const SCHEMES: [&str; 6] = ["http", "https", "rtsp", "udp", "rtp", "tcp"];

/// Which shelf of the Network page a saved address sits on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NetKind {
    Stream,
    Podcast,
    Radio,
}

impl NetKind {
    pub fn key(self) -> &'static str {
        match self {
            NetKind::Stream => "stream",
            NetKind::Podcast => "podcast",
            NetKind::Radio => "radio",
        }
    }

    pub fn from_key(key: &str) -> Option<NetKind> {
        match key {
            "stream" => Some(NetKind::Stream),
            "podcast" => Some(NetKind::Podcast),
            "radio" => Some(NetKind::Radio),
            _ => None,
        }
    }
}

/// Whether `s` is a stream address wushang can open, like `https://example.com/live.m3u8`.
pub fn is_url(s: &str) -> bool {
    let Some((scheme, rest)) = s.split_once("://") else {
        return false;
    };
    !rest.is_empty() && SCHEMES.iter().any(|k| scheme.eq_ignore_ascii_case(k))
}

/// Whether a feed or the radio directory may hand wushang `s` to play: http or https only, and
/// nothing a file name could carry (a `file://` link to someone's share, a webcam's `dshow://`).
/// Tabs and newlines would also split the saved library's records.
fn is_web_url(s: &str) -> bool {
    let web = ["http://", "https://"]
        .iter()
        .any(|p| s.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p)));
    web && host_of(s).is_some() && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Whether a path the app carries around is really a stream address.
pub fn is_url_path(path: &Path) -> bool {
    path.to_str().is_some_and(is_url)
}

/// Whether a path is no file at all: a stream or a capture device (`capture.rs`). Such paths are
/// never made absolute, probed for thumbnails or opened on the UI thread.
pub fn is_address(path: &Path) -> bool {
    is_url_path(path) || crate::capture::is_capture_path(path) || crate::disc::is_disc_path(path)
}

/// Checks what the user typed as a stream address and tidies it: a bare host gets
/// `http://` in front, as in VLC. The error is fit to show under the field.
pub fn normalize_url(raw: &str) -> Result<String, String> {
    let url = raw.trim();
    if url.is_empty() {
        return Err("Enter a stream address first.".into());
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("That address has spaces or special characters in it.".into());
    }
    if is_url(url) {
        return match host_of(url) {
            Some(_) => Ok(url.to_owned()),
            None => Err("That address has no server in it.".into()),
        };
    }
    if let Some((scheme, _)) = url.split_once("://") {
        return Err(format!("wushang can't open {scheme} addresses."));
    }
    // No scheme: a dotted host, optionally with a port and a path.
    let host = url.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    if host.contains('.')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-.".contains(c))
    {
        return normalize_url(&format!("http://{url}"));
    }
    Err("That doesn't look like a stream address.".into())
}

/// The server part of an address: `example.com`, without scheme, credentials or port.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next()?
    } else {
        authority.split(':').next()?
    };
    (!host.is_empty()).then(|| host.to_owned())
}

/// A name for an address: the last part of its path, decoded, else its server.
/// `https://example.com/a/My%20Show.mp4?x=1` is `My Show.mp4`.
pub fn url_name(url: &str) -> String {
    let no_query = url.split(['?', '#']).next().unwrap_or(url);
    // Only the path counts, so `user:password@host` never becomes the name on screen.
    let path = match no_query.split_once("://") {
        Some((_, rest)) => rest.split_once('/').map_or("", |(_, path)| path),
        None => no_query,
    };
    let last = path.rsplit('/').next().unwrap_or("");
    let name = percent_decode(last);
    if name.is_empty() || name == url {
        host_of(url).unwrap_or_else(|| url.to_owned())
    } else {
        name
    }
}

/// The address with any password hidden, for logs and error messages.
pub fn redact(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let Some(at) = rest.find('@') else {
        return url.to_owned();
    };
    let (userinfo, tail) = rest.split_at(at);
    // An '@' in the path is not credentials.
    if userinfo.contains(['/', '?', '#']) {
        return url.to_owned();
    }
    match userinfo.split_once(':') {
        Some((user, _)) => format!("{scheme}://{user}:***{tail}"),
        None => url.to_owned(),
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(h), Some(l)) = (
                bytes.get(i + 1).and_then(|b| hex(*b)),
                bytes.get(i + 2).and_then(|b| hex(*b)),
            )
        {
            out.push(h << 4 | l);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `&amp;`, `&quot;`, `&#233;` and friends, as feeds and directories write them.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let Some(end) = after.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = after;
            continue;
        };
        let entity = &after[..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#')?.parse().ok())
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => out.push(c),
            None => {
                out.push('&');
                out.push_str(entity);
                out.push(';');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// A feed's date as a day: RSS's "Mon, 26 Oct 2026 08:00:00 GMT" is "26 Oct 2026", Atom's
/// "2026-10-26T08:00:00Z" is "2026-10-26". Anything else stays as the feed wrote it.
pub fn short_date(date: &str) -> String {
    let date = date.trim();
    if let Some((day, _)) = date.split_once('T')
        && day.len() == 10
        && day.as_bytes()[4] == b'-'
    {
        return day.to_owned();
    }
    let after_weekday = date.split_once(", ").map_or(date, |(_, rest)| rest);
    let words: Vec<&str> = after_weekday.split_whitespace().collect();
    match words[..] {
        [day, month, year, ..] if day.parse::<u8>().is_ok() && year.parse::<u16>().is_ok() => {
            format!("{day} {month} {year}")
        }
        _ => date.to_owned(),
    }
}

/// One playable podcast episode.
#[derive(Clone, Debug, PartialEq)]
pub struct Episode {
    pub title: String,
    pub url: String,
    /// The date as the feed wrote it, when it did.
    pub date: Option<String>,
}

/// A parsed podcast feed.
#[derive(Clone, Debug, PartialEq)]
pub struct Feed {
    pub title: String,
    pub episodes: Vec<Episode>,
}

/// Parses an RSS or Atom feed. Only items with a media enclosure (or a media link)
/// come out, in feed order; relative addresses resolve against the feed's own.
pub fn parse_feed(text: &str, feed_url: &str) -> Feed {
    let title = tag_text(text, "title").unwrap_or_default();
    let mut episodes = Vec::new();
    for block in blocks(text, "item")
        .into_iter()
        .chain(blocks(text, "entry"))
    {
        let Some(url) = enclosure_url(&block) else {
            continue;
        };
        // Some feeds leave spaces in their links unescaped.
        let url = resolve(feed_url, &url).replace(' ', "%20");
        if !is_web_url(&url) {
            continue;
        }
        let title = tag_text(&block, "title")
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| url_name(&url));
        let date = ["pubDate", "published", "updated"]
            .iter()
            .find_map(|t| tag_text(&block, t));
        episodes.push(Episode { title, url, date });
    }
    Feed { title, episodes }
}

/// One station of the radio directory.
#[derive(Clone, Debug, PartialEq)]
pub struct Station {
    pub name: String,
    pub url: String,
    pub tags: String,
    pub country: String,
}

/// Parses the radio directory's XML: one `<station>` per station, attributes in any
/// order. `url_resolved` wins over `url`, as it is the address that last worked.
pub fn parse_stations(text: &str) -> Vec<Station> {
    let mut out: Vec<Station> = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("<station") {
        let after = &rest[at + 8..];
        // The root is <stations>; only a real station tag counts.
        if !after.starts_with([' ', '\t', '\r', '\n', '/', '>']) {
            rest = after;
            continue;
        }
        let Some(end) = after.find('>') else { break };
        let tag = &after[..end];
        rest = &after[end + 1..];
        let name = attr(tag, "name").unwrap_or_default();
        let url = attr(tag, "url_resolved")
            .filter(|u| !u.is_empty())
            .or_else(|| attr(tag, "url"))
            .unwrap_or_default();
        if name.is_empty() || !is_web_url(&url) || out.iter().any(|s: &Station| s.url == url) {
            continue;
        }
        out.push(Station {
            name,
            url,
            tags: attr(tag, "tags").unwrap_or_default(),
            country: attr(tag, "country").unwrap_or_default(),
        });
    }
    out
}

/// An `<item>`'s media address: an enclosure, a Media RSS content, or an Atom
/// enclosure link, in that order.
fn enclosure_url(block: &str) -> Option<String> {
    for tag in ["enclosure", "media:content"] {
        if let Some(at) = block.find(&format!("<{tag}"))
            && let Some(url) = attr(&block[at..], "url")
            && !url.is_empty()
        {
            return Some(url);
        }
    }
    // Atom: <link rel="enclosure" href="..."/>.
    let mut rest = block;
    while let Some(at) = rest.find("<link") {
        let after = &rest[at..];
        let end = after.find('>').unwrap_or(after.len());
        let tag = &after[..end];
        if attr(tag, "rel").is_some_and(|r| r == "enclosure")
            && let Some(url) = attr(tag, "href")
            && !url.is_empty()
        {
            return Some(url);
        }
        rest = &after[end.min(after.len())..];
        if end == after.len() {
            break;
        }
    }
    None
}

/// Every `<tag ...> ... </tag>` block, without the tags themselves.
fn blocks(text: &str, tag: &str) -> Vec<String> {
    let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start..];
        // <items> or <entryfoo> are not this tag.
        let boundary = after[open.len()..].starts_with([' ', '\t', '\r', '\n', '>', '/']);
        if !boundary {
            rest = &after[open.len()..];
            continue;
        }
        let Some(body_at) = after.find('>') else {
            break;
        };
        let body = &after[body_at + 1..];
        let Some(end) = body.find(&close) else { break };
        out.push(body[..end].to_owned());
        rest = &body[end + close.len()..];
    }
    out
}

/// The text of the first `<tag>` in `text`, markup inside stripped, entities decoded.
fn tag_text(text: &str, tag: &str) -> Option<String> {
    let at = text.find(&format!("<{tag}"))?;
    let after = &text[at..];
    if !after[tag.len() + 1..].starts_with([' ', '\t', '\r', '\n', '>', '/']) {
        return None;
    }
    let body_at = after.find('>')? + 1;
    let body = &after[body_at..];
    let end = body.find(&format!("</{tag}>"))?;
    let inner = &body[..end];
    let inner = inner
        .strip_prefix("<![CDATA[")
        .and_then(|s| s.strip_suffix("]]>"))
        .unwrap_or(inner);
    // Drop any markup inside, keeping its text.
    let mut plain = String::with_capacity(inner.len());
    let mut markup = false;
    for c in inner.chars() {
        match c {
            '<' => markup = true,
            '>' => markup = false,
            c if !markup => plain.push(c),
            _ => {}
        }
    }
    let plain = decode_entities(plain.trim());
    (!plain.is_empty()).then_some(plain)
}

/// One attribute of a tag, either quote style, entities decoded.
fn attr(tag: &str, name: &str) -> Option<String> {
    let tag = tag.split('>').next()?;
    let at = tag.find(&format!("{name}="))?;
    // The name must start the attribute, not end a longer one (data-url=).
    if at > 0 && !tag.as_bytes()[at - 1].is_ascii_whitespace() {
        return None;
    }
    let after = &tag[at + name.len() + 1..];
    let quote = after.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let value = &after[1..after[1..].find(quote)? + 1];
    Some(decode_entities(value))
}

/// Resolves a possibly relative address against the feed it came from.
fn resolve(base: &str, href: &str) -> String {
    let href = href.trim();
    if is_url(href) || href.contains("://") {
        return href.to_owned();
    }
    let Some((scheme, rest)) = base.split_once("://") else {
        return href.to_owned();
    };
    let authority = rest.split('/').next().unwrap_or("");
    if let Some(path) = href.strip_prefix("//") {
        return format!("{scheme}://{path}");
    }
    if href.starts_with('/') {
        return format!("{scheme}://{authority}{href}");
    }
    let dir = base.rsplit_once('/').map_or(base, |(d, _)| d);
    format!("{dir}/{href}")
}

/// Fetches `url` (http or https) and returns the body as text. Follows redirects,
/// honours the proxy in the environment, and gives up after `timeout` per stage.
pub fn fetch_text(url: &str, timeout: Duration) -> Result<String, String> {
    fetch(url, timeout, MAX_FETCH).map(|body| String::from_utf8_lossy(&body).into_owned())
}

/// Fetches `url` (http or https) as bytes, up to `max` of them, like [`fetch_text`].
pub fn fetch(url: &str, timeout: Duration, max: usize) -> Result<Vec<u8>, String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("{} is not a web address", redact(url)))?;
    let secure = match scheme.to_ascii_lowercase().as_str() {
        "http" => false,
        "https" => true,
        _ => return Err(format!("only http and https load, not {scheme}")),
    };
    let (authority, path) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    // A user name and password in the address are never sent anywhere.
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let host = host_of(url).unwrap_or_default();
    let path = path.split('#').next().unwrap_or(path);
    let path = if path.starts_with('?') {
        format!("/{path}")
    } else {
        path.to_owned()
    };
    let stage = Some(timeout.clamp(Duration::from_secs(1), Duration::from_secs(60)));

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .user_agent(concat!("wushang/", env!("CARGO_PKG_VERSION")))
        // Feeds and directories redirect between hosts; follow, but never from https down to
        // http, where anyone on the network could swap what comes back (an update, say).
        .https_only(secure)
        .max_redirects(10)
        .timeout_resolve(stage)
        .timeout_connect(stage)
        .timeout_send_request(stage)
        .timeout_recv_response(stage)
        // Per read, as WinHTTP's receive timeout was: a slow but steady download goes on.
        .timeout_per_read(stage)
        .build()
        .into();
    let mut response = agent
        .get(format!("{scheme}://{authority}{path}"))
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(status) => {
                format!("the server at {host} answered HTTP {status}")
            }
            ureq::Error::RequireHttpsOnly(_) => {
                format!("{host} sent wushang from https to plain http")
            }
            e => format!("cannot reach {host}: {e}"),
        })?;
    // One more than allowed, so a body of exactly `max` bytes still loads.
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    let body = response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|e| match e {
            ureq::Error::BodyExceedsLimit(_) => "that download is too large".to_owned(),
            e => format!("network: {e}"),
        })?;
    if body.len() > max {
        return Err("that download is too large".into());
    }
    Ok(body)
}

/// What a finished fetch means for the caller.
pub enum Fetched {
    Feed {
        feed: String,
        result: Result<Feed, String>,
    },
    Stations(Result<Vec<Station>, String>),
}

enum Request {
    Feed(String, Duration),
    Stations(Duration),
}

/// One background thread fetching podcast feeds and the radio directory, so the
/// window never waits on the network. Results come back in request order.
pub struct Fetcher {
    send: Sender<Request>,
    ready: Receiver<Fetched>,
}

impl Fetcher {
    pub fn new() -> Fetcher {
        let (send, ask) = channel::<Request>();
        let (done, ready) = channel::<Fetched>();
        thread::spawn(move || {
            while let Ok(request) = ask.recv() {
                let fetched = match request {
                    Request::Feed(url, timeout) => Fetched::Feed {
                        result: fetch_text(&url, timeout).and_then(|text| {
                            let feed = parse_feed(&text, &url);
                            if feed.episodes.is_empty() {
                                Err("that feed has no playable episodes".into())
                            } else {
                                Ok(feed)
                            }
                        }),
                        feed: url,
                    },
                    Request::Stations(timeout) => Fetched::Stations(
                        fetch_text(RADIO_DIRECTORY, timeout).map(|t| parse_stations(&t)),
                    ),
                };
                if done.send(fetched).is_err() {
                    break;
                }
            }
        });
        Fetcher { send, ready }
    }

    pub fn feed(&self, url: &str, timeout: Duration) {
        _ = self.send.send(Request::Feed(url.to_owned(), timeout));
    }

    pub fn stations(&self, timeout: Duration) {
        _ = self.send.send(Request::Stations(timeout));
    }

    /// Finished fetches since the last call.
    pub fn take_ready(&self) -> Vec<Fetched> {
        std::iter::from_fn(|| self.ready.try_recv().ok()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn urls_are_recognised_and_tidied() {
        assert!(is_url("https://example.com/live.m3u8"));
        assert!(is_url("RTSP://camera.local/stream"));
        assert!(!is_url(r"C:\video\file.mp4"));
        assert!(!is_url("example.com/file.mp4"));
        assert!(!is_url("mailto:dj@example.com"));
        assert!(is_url_path(&PathBuf::from("udp://239.0.0.1:1234")));
        assert!(!is_url_path(&PathBuf::from(r"D:\v\a.mkv")));

        assert_eq!(
            normalize_url("example.com/radio.mp3").unwrap(),
            "http://example.com/radio.mp3"
        );
        assert_eq!(
            normalize_url(" https://EXAMPLE.com/a ").unwrap(),
            "https://EXAMPLE.com/a"
        );
        assert!(normalize_url("").is_err());
        assert!(normalize_url("https://").is_err());
        assert!(normalize_url("gopher://example.com/x").is_err());
        assert!(normalize_url("two words.mp4").is_err());
        assert!(normalize_url("justtext").is_err());
    }

    #[test]
    fn names_hosts_and_redaction() {
        assert_eq!(
            url_name("https://example.com/a/My%20Show.mp4?tok=1"),
            "My Show.mp4"
        );
        assert_eq!(url_name("https://example.com/"), "example.com");
        assert_eq!(url_name("https://user:pw@example.com:8080/x"), "x");
        assert_eq!(url_name("rtsp://admin:secret@cam.local:554"), "cam.local");
        assert_eq!(
            host_of("https://user@example.com:8080/x").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            redact("https://user:secret@example.com/live"),
            "https://user:***@example.com/live"
        );
        assert_eq!(redact("https://example.com/a@b"), "https://example.com/a@b");
    }

    const RSS: &str = r#"<?xml version="1.0"?>
<rss><channel>
<title><![CDATA[The Daily &amp; Show]]></title>
<item><title>First &lt;Episode&gt;</title>
  <enclosure url="ep1.mp3" type="audio/mpeg"/>
  <pubDate>Mon, 20 Oct 2026 04:00:00 GMT</pubDate></item>
<item><title>No media here</title><link>https://example.com/post</link></item>
<item><title>Second</title>
  <media:content url="https://cdn.example.com/ep2.mp3"/>
</item>
</channel></rss>"#;

    #[test]
    fn rss_episodes_come_out_playable_in_order() {
        let feed = parse_feed(RSS, "https://example.com/pod/feed.xml");
        assert_eq!(feed.title, "The Daily & Show");
        assert_eq!(
            feed.episodes,
            [
                Episode {
                    title: "First <Episode>".into(),
                    url: "https://example.com/pod/ep1.mp3".into(),
                    date: Some("Mon, 20 Oct 2026 04:00:00 GMT".into()),
                },
                Episode {
                    title: "Second".into(),
                    url: "https://cdn.example.com/ep2.mp3".into(),
                    date: None,
                },
            ]
        );
        assert!(
            parse_feed("<html>nope</html>", "https://x.example/f")
                .episodes
                .is_empty()
        );
    }

    #[test]
    fn feed_dates_shorten_to_the_day() {
        assert_eq!(short_date("Mon, 26 Oct 2026 08:00:00 GMT"), "26 Oct 2026");
        assert_eq!(short_date("6 Oct 2026 08:00 +0000"), "6 Oct 2026");
        assert_eq!(short_date("2026-10-20T04:00:00Z"), "2026-10-20");
        assert_eq!(short_date("sometime last week"), "sometime last week");
    }

    #[test]
    fn atom_enclosures_and_relative_addresses() {
        let atom = r#"<feed xmlns="http://www.w3.org/2005/Atom">
<title>Atom Cast</title>
<entry><title>One</title>
<link rel="alternate" href="https://example.com/1"/>
<link rel="enclosure" href="/media/one.mp3" type="audio/mpeg"/>
<updated>2026-10-20T04:00:00Z</updated></entry>
</feed>"#;
        let feed = parse_feed(atom, "https://example.com/feed");
        assert_eq!(feed.title, "Atom Cast");
        assert_eq!(feed.episodes.len(), 1);
        assert_eq!(feed.episodes[0].url, "https://example.com/media/one.mp3");
        assert_eq!(
            feed.episodes[0].date.as_deref(),
            Some("2026-10-20T04:00:00Z")
        );
    }

    #[test]
    fn radio_directory_attributes_in_any_order() {
        let xml = r#"<stations>
<station url="http://a.example/lo" name="Lo Fi" country="Germany" tags="chill"/>
<station countrycode="US" url_resolved="https://b.example/hi" name="Rock &amp; Roll" url="http://b.example/hi" tags="rock" country="USA"/>
<station name="Dup" url_resolved="http://a.example/lo"/>
<station name="" url="http://c.example/x"/>
</stations>"#;
        assert_eq!(
            parse_stations(xml),
            [
                Station {
                    name: "Lo Fi".into(),
                    url: "http://a.example/lo".into(),
                    tags: "chill".into(),
                    country: "Germany".into(),
                },
                Station {
                    name: "Rock & Roll".into(),
                    url: "https://b.example/hi".into(),
                    tags: "rock".into(),
                    country: "USA".into(),
                },
            ]
        );
        assert!(parse_stations("<stations/>").is_empty());
    }

    #[test]
    fn feeds_and_stations_only_hand_over_web_addresses() {
        let rss = r#"<rss><channel><title>Bad</title>
<item><title>Share</title><enclosure url="file://evil/share/x.mp3"/></item>
<item><title>Webcam</title><enclosure url="dshow://video=Camera"/></item>
<item><title>Split</title><enclosure url="tcp://evil:9000/&#10;favorite&#9;\\evil\s"/></item>
<item><title>Spaced</title><enclosure url="https://cdn.example/My Show.mp3"/></item>
</channel></rss>"#;
        let urls: Vec<_> = parse_feed(rss, "https://example.com/feed")
            .episodes
            .into_iter()
            .map(|e| e.url)
            .collect();
        assert_eq!(urls, ["https://cdn.example/My%20Show.mp3"]);
        let xml = r#"<stations>
<station name="Share" url="file://evil/share/x"/>
<station name="Split" url="http://a.example/&#10;root&#9;music&#9;\\evil\s"/>
</stations>"#;
        assert!(parse_stations(xml).is_empty());
    }

    /// A one-thread web server on a free local port: each request gets what `answer` makes of
    /// its path, written as is.
    fn serve(answer: fn(&str) -> Vec<u8>) -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                _ = reader.read_line(&mut line);
                let path = line.split(' ').nth(1).unwrap_or("/").to_owned();
                // The rest of the request, up to its blank line.
                while reader.read_line(&mut line).is_ok_and(|n| n > 2) {}
                _ = reader.get_mut().write_all(&answer(&path));
            }
        });
        address
    }

    #[test]
    fn fetches_with_limits_and_says_what_went_wrong() {
        let base = serve(|path| match path {
            "/feed?x=1" => "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello"
                .to_string()
                .into_bytes(),
            "/moved" => {
                b"HTTP/1.1 302 Found\r\nLocation: /feed?x=1\r\nContent-Length: 0\r\n\r\n".to_vec()
            }
            "/big" => {
                let body = "x".repeat(2000);
                format!("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{body}").into_bytes()
            }
            "/stall" => {
                thread::sleep(Duration::from_secs(3));
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec()
            }
            _ => {
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
            }
        });
        let second = Duration::from_secs(1);
        assert_eq!(
            fetch_text(&format!("{base}/feed?x=1"), second).as_deref(),
            Ok("hello")
        );
        // Redirects are followed; a user name in the address is never sent.
        let with_user = base.replace("http://", "http://me:secret@");
        assert_eq!(
            fetch_text(&format!("{with_user}/moved"), second).as_deref(),
            Ok("hello")
        );
        assert_eq!(
            fetch_text(&format!("{base}/gone"), second),
            Err("the server at 127.0.0.1 answered HTTP 404".into())
        );
        assert_eq!(
            fetch(&format!("{base}/big"), second, 1000),
            Err("that download is too large".into())
        );
        assert_eq!(
            fetch(&format!("{base}/big"), second, 2000).map(|b| b.len()),
            Ok(2000)
        );
        let stalled = fetch_text(&format!("{base}/stall"), second).unwrap_err();
        assert!(stalled.starts_with("cannot reach 127.0.0.1"), "{stalled}");
        assert!(fetch_text("ftp://example.com/x", second).is_err());
        assert!(fetch_text("not an address", second).is_err());
    }

    /// The real podcast directory and radio directory, and GitHub's release answer. Needs the
    /// internet: `cargo test live -- --ignored`.
    #[test]
    #[ignore]
    fn live_feeds_and_radio_directory() {
        let timeout = Duration::from_secs(20);
        let fetcher = Fetcher::new();
        fetcher.feed("https://feeds.npr.org/510289/podcast.xml", timeout);
        fetcher.stations(timeout);
        let mut done = Vec::new();
        while done.len() < 2 {
            done.extend(fetcher.take_ready());
            thread::sleep(Duration::from_millis(50));
        }
        for fetched in done {
            match fetched {
                Fetched::Feed { result, .. } => {
                    let feed = result.unwrap();
                    assert!(
                        !feed.title.is_empty() && feed.episodes.len() > 5,
                        "{feed:?}"
                    );
                    assert!(feed.episodes.iter().all(|e| is_web_url(&e.url)));
                }
                Fetched::Stations(stations) => {
                    let stations = stations.unwrap();
                    assert!(stations.len() > 50, "{} stations", stations.len());
                    assert!(stations.iter().all(|s| is_web_url(&s.url)));
                }
            }
        }
        // GitHub sends its release JSON gzipped when asked; it comes back as text.
        let json = fetch_text(
            "https://api.github.com/repos/ShreyanshVaibhaw/wushang/releases/latest",
            timeout,
        )
        .unwrap();
        assert!(json.contains("\"tag_name\""), "{json}");
        // An https address that redirects to plain http is refused.
        let down = fetch_text(
            "https://httpbin.org/redirect-to?url=http%3A%2F%2Fexample.com%2F",
            timeout,
        );
        assert!(down.is_err(), "{down:?}");
    }
}
