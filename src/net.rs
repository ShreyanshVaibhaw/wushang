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

use windows::Win32::Networking::WinHttp::*;
use windows::core::{HSTRING, PCWSTR};

/// Where the radio directory comes from: the community run Radio Browser, no key needed.
const RADIO_DIRECTORY: &str = "https://de1.api.radio-browser.info/xml/stations/topvote/100";
/// Feeds and directories bigger than this are not worth parsing.
const MAX_FETCH: usize = 4 << 20;

/// The schemes FFmpeg can open as a stream here, and that the slim build carries protocols for.
/// See `scripts/build-ffmpeg.sh`: rtmp, mms, srt and ftp would need more of FFmpeg.
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

/// Whether `s` is a stream address myvlc can open, like `https://example.com/live.m3u8`.
pub fn is_url(s: &str) -> bool {
    let Some((scheme, rest)) = s.split_once("://") else {
        return false;
    };
    !rest.is_empty() && SCHEMES.iter().any(|k| scheme.eq_ignore_ascii_case(k))
}

/// Whether a path the app carries around is really a stream address.
pub fn is_url_path(path: &Path) -> bool {
    path.to_str().is_some_and(is_url)
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
        return Err(format!("myvlc can't open {scheme} addresses."));
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
    let last = no_query.rsplit('/').next().unwrap_or("");
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
        let url = resolve(feed_url, &url);
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
        if name.is_empty() || url.is_empty() || out.iter().any(|s: &Station| s.url == url) {
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
/// honours the system proxy, and gives up after `timeout` per stage.
pub fn fetch_text(url: &str, timeout: Duration) -> Result<String, String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("{} is not a web address", redact(url)))?;
    let secure = match scheme.to_ascii_lowercase().as_str() {
        "http" => false,
        "https" => true,
        _ => return Err(format!("feeds load over http or https, not {scheme}")),
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => {
            (h, p.parse().map_err(|_| "bad port".to_owned())?)
        }
        _ => (authority, if secure { 443 } else { 80 }),
    };
    let host = host.trim_matches(['[', ']']);
    let ms = (timeout.as_millis()).clamp(1_000, 60_000) as i32;

    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let agent = HSTRING::from(concat!("myvlc/", env!("CARGO_PKG_VERSION")));
    unsafe {
        struct Handle(*mut std::ffi::c_void);
        impl Drop for Handle {
            fn drop(&mut self) {
                _ = unsafe { WinHttpCloseHandle(self.0) };
            }
        }
        let session = WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        if session.is_null() {
            return Err(format!("network: {}", windows::core::Error::from_thread()));
        }
        let session = Handle(session);
        WinHttpSetTimeouts(session.0, ms, ms, ms, ms).map_err(|e| format!("network: {e}"))?;
        let connect = WinHttpConnect(session.0, PCWSTR(wide(host).as_ptr()), port, 0);
        if connect.is_null() {
            return Err(format!("network: {}", windows::core::Error::from_thread()));
        }
        let connect = Handle(connect);
        let flags = if secure {
            WINHTTP_FLAG_SECURE
        } else {
            WINHTTP_OPEN_REQUEST_FLAGS(0)
        };
        let request = WinHttpOpenRequest(
            connect.0,
            PCWSTR(wide("GET").as_ptr()),
            PCWSTR(wide(path).as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            flags,
        );
        if request.is_null() {
            return Err(format!("network: {}", windows::core::Error::from_thread()));
        }
        let request = Handle(request);
        // Feeds and directories redirect between hosts; follow like a browser.
        _ = WinHttpSetOption(
            Some(request.0 as *const _),
            WINHTTP_OPTION_REDIRECT_POLICY,
            Some(&WINHTTP_OPTION_REDIRECT_POLICY_ALWAYS.to_ne_bytes()),
        );
        WinHttpSendRequest(request.0, None, None, 0, 0, 0)
            .and_then(|()| WinHttpReceiveResponse(request.0, std::ptr::null_mut()))
            .map_err(|e| format!("cannot reach {}: {e}", host_of(url).unwrap_or_default()))?;
        let (mut status, mut len) = (0u32, 4u32);
        let mut index = 0u32;
        _ = WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut len,
            &mut index,
        );
        if !(200..300).contains(&status) {
            return Err(format!("the server at {host} answered HTTP {status}"));
        }
        let mut body = Vec::new();
        let mut buf = [0u8; 16 * 1024];
        loop {
            let mut read = 0u32;
            WinHttpReadData(
                request.0,
                buf.as_mut_ptr() as *mut _,
                buf.len() as u32,
                &mut read,
            )
            .map_err(|e| format!("network: {e}"))?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&buf[..read as usize]);
            if body.len() > MAX_FETCH {
                return Err("that feed is too large to read".into());
            }
        }
        Ok(String::from_utf8_lossy(&body).into_owned())
    }
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
}
