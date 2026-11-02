//! Guessing the code page of subtitle text that isn't UTF-8, and converting it. libavcodec only
//! takes UTF-8, and old .srt files come in whatever the author's Windows used: 1252 in Western
//! Europe, 1251 for Cyrillic, Shift-JIS in Japan and so on. Code pages keep their Windows
//! numbers; encoding_rs does the decoding, with the WHATWG versions of each (the same tables, and
//! for 936, 949 and 950 their supersets GB18030, UHC and Big5-HKSCS).
//!
//! A language tag (from "movie.ru.srt") settles it when it names one. Otherwise every candidate
//! decodes the text and the most plausible result wins: words in one script, Latin words with
//! some plain ASCII in them, and for the CJK code pages strict decoding into CJK characters.

const UTF8: u32 = 65001;

/// The encoding behind a Windows code page number, None for one wushang doesn't know.
fn encoding(cp: u32) -> Option<&'static encoding_rs::Encoding> {
    use encoding_rs::*;
    Some(match cp {
        UTF8 => UTF_8,
        874 => WINDOWS_874,
        932 => SHIFT_JIS,
        936 => GBK,
        949 => EUC_KR,
        950 => BIG5,
        1250 => WINDOWS_1250,
        1251 => WINDOWS_1251,
        1252 => WINDOWS_1252,
        1253 => WINDOWS_1253,
        1254 => WINDOWS_1254,
        1255 => WINDOWS_1255,
        1256 => WINDOWS_1256,
        1257 => WINDOWS_1257,
        1258 => WINDOWS_1258,
        _ => return None,
    })
}

/// The code page Windows would use for the user's first preferred language, what GetACP gives
/// there: 1252 for English, 1251 for Russian and so on.
fn system_code_page() -> u32 {
    static CP: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *CP.get_or_init(|| {
        let languages = objc2_foundation::NSLocale::preferredLanguages();
        let first = languages.iter().next().map(|l| l.to_string());
        // "en-IN", "zh-Hant-TW": the language comes first; traditional Chinese has a script.
        let tag = first.as_deref().unwrap_or("en");
        let language = tag.split(['-', '_']).next().unwrap_or("en");
        let language = if language == "zh" && tag.contains("Hant") {
            "cht"
        } else {
            language
        };
        for_language(language).unwrap_or(1252)
    })
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Script {
    Latin,
    Cyrillic,
    Greek,
    Hebrew,
    Arabic,
    Cjk,
}

/// The Windows code pages tried, with the script their letters are in. Thai (874) is left to
/// language tags: its vowel marks let it decode other scripts into plausible-looking text.
const CANDIDATES: [(u32, Script); 12] = [
    (1252, Script::Latin),
    (1250, Script::Latin),
    // Ahead of 1251, so they win ties: caseless Hebrew and Arabic read as lowercase Cyrillic,
    // while Russian read as either turns capitals into marks that count against it.
    (1255, Script::Hebrew),
    (1256, Script::Arabic),
    (1251, Script::Cyrillic),
    (1253, Script::Greek),
    (1254, Script::Latin),
    (1257, Script::Latin),
    (936, Script::Cjk),
    (932, Script::Cjk),
    (949, Script::Cjk),
    (950, Script::Cjk),
];

/// The code page of `text`, UTF-8 when it already is. `language` is an ISO 639 code such as
/// "ru" or "rus", when the file name or stream gives one.
pub fn detect(text: &[u8], language: Option<&str>) -> u32 {
    if std::str::from_utf8(text).is_ok() {
        return UTF8;
    }
    if let Some(cp) = language.and_then(for_language)
        && decode(text, cp, true).is_some()
    {
        return cp;
    }
    // The system's own code page goes first, so it wins ties.
    let system = system_code_page();
    let mut order: Vec<(u32, Script)> = CANDIDATES.to_vec();
    order.sort_by_key(|&(cp, _)| cp != system);
    let score = |cp: u32, script: Script| {
        let text = decode(text, cp, true)?;
        Some(match script {
            Script::Cjk => cjk_plausibility(&text, cp),
            _ => plausibility(&text, script),
        })
    };
    let best = (order.into_iter())
        .filter_map(|(cp, script)| Some((cp, score(cp, script)?)))
        .fold(None, |best: Option<(u32, i32)>, (cp, score)| match best {
            Some((_, b)) if b >= score => best,
            _ => Some((cp, score)),
        });
    // Without real evidence for any of them, the system's code page is the best bet.
    match best {
        Some((cp, score)) if score > 0 => cp,
        _ if decode(text, system, true).is_some() => system,
        _ => 1252,
    }
}

/// `text` in code page `cp` as a string; with `strict`, None when it isn't valid in it.
pub fn decode(text: &[u8], cp: u32, strict: bool) -> Option<String> {
    let encoding = encoding(cp)?;
    if strict {
        encoding
            .decode_without_bom_handling_and_without_replacement(text)
            .map(|s| s.into_owned())
    } else {
        Some(encoding.decode_without_bom_handling(text).0.into_owned())
    }
}

/// Punctuation and symbols subtitles use outside ASCII; any other non-ASCII mark or symbol
/// suggests the wrong code page.
const PUNCTUATION: &str = "\u{a0}«»¿¡–—…‘’‚“”„•°€£©®™·×";

/// How much `text` reads like real words in `script`: non-ASCII letters in plausible words,
/// minus those in implausible ones and stray marks. Counting letters rather than words keeps a
/// decoding that breaks words apart from scoring higher.
fn plausibility(text: &str, script: Script) -> i32 {
    let stray = (text.chars())
        .filter(|&c| !c.is_ascii() && !c.is_alphabetic() && !PUNCTUATION.contains(c))
        .count() as i32;
    let mut score = -stray;
    for word in text.split(|c: char| !c.is_alphabetic()) {
        if word.is_ascii() {
            continue;
        }
        let ascii = word.chars().any(|c| c.is_ascii());
        let one_script = word
            .chars()
            .filter(|c| !c.is_ascii())
            .all(|c| script_of(c) == Some(script));
        // Latin words have plain letters in them ("été"); other scripts don't mix with them.
        let mixing = if script == Script::Latin {
            ascii
        } else {
            !ascii
        };
        // "Ïðèâåò" is fine, "ïÐèÂ" is not: no capital after a small letter inside a word.
        let chars: Vec<char> = word.chars().collect();
        let cased = chars
            .windows(2)
            .all(|w| !(w[0].is_lowercase() && w[1].is_uppercase()));
        // Vowel points and harakat follow a letter; a word can't start with one.
        let based = !matches!(u32::from(chars[0]), 0x300..=0x36F | 0x591..=0x5C7 | 0x64B..=0x65F);
        let letters = word.chars().filter(|c| !c.is_ascii()).count() as i32;
        score += if one_script && mixing && cased && based {
            letters
        } else {
            -letters
        };
    }
    score
}

/// How much `text` reads like Japanese (932), Chinese (936, 950) or Korean (949). The same bytes
/// are often valid in several of them, so each language's tell counts: kana for Japanese, short
/// space-separated hangul words without hanja for Korean.
// ponytail: Big5 and GBK can't be told apart this way, so traditional Chinese needs a "cht"
// tag; a character frequency table would settle it if those files turn up.
fn cjk_plausibility(text: &str, cp: u32) -> i32 {
    let mut score = 0;
    // Non-CJK characters that aren't ASCII or punctuation, and CJK glued to Latin letters
    // ("It抯" is a misread "It’s"), count against.
    let mut prev = ' ';
    for c in text.chars() {
        let cjk = cjk_kind(c).is_some();
        let full_width = matches!(u32::from(c), 0x3000..=0x303F | 0xFF00..=0xFFEF);
        if !cjk && !c.is_ascii() && !full_width && !PUNCTUATION.contains(c) {
            score -= 1;
        }
        if (cjk && prev.is_ascii_alphabetic())
            || (cjk_kind(prev).is_some() && c.is_ascii_alphabetic())
        {
            score -= 2;
        }
        prev = c;
    }
    // Words are runs of CJK characters; each one is judged whole.
    for word in text.split(|c: char| cjk_kind(c).is_none()) {
        let kinds: Vec<Cjk> = word.chars().filter_map(cjk_kind).collect();
        let len = kinds.len() as i32;
        let count = |k: Cjk| kinds.iter().filter(|&&x| x == k).count() as i32;
        let (kana, hangul) = (count(Cjk::Kana), count(Cjk::Hangul));
        score += match cp {
            // Korean: words of a few syllables between spaces, no hanja in subtitles.
            949 if hangul == len && len <= 5 => 2 * len,
            932 if hangul == 0 => len + kana,
            936 | 950 if kana == 0 && hangul == 0 => len,
            _ => -len,
        };
    }
    score
}

#[derive(Clone, Copy, PartialEq)]
enum Cjk {
    Kana,
    Han,
    Hangul,
}

fn cjk_kind(c: char) -> Option<Cjk> {
    Some(match u32::from(c) {
        0x3040..=0x30FF => Cjk::Kana,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF => Cjk::Han,
        0xAC00..=0xD7AF => Cjk::Hangul,
        _ => return None,
    })
}

fn script_of(c: char) -> Option<Script> {
    Some(match u32::from(c) {
        0xC0..=0x24F if c.is_alphabetic() => Script::Latin,
        0x370..=0x3FF => Script::Greek,
        0x400..=0x4FF => Script::Cyrillic,
        0x590..=0x5FF => Script::Hebrew,
        0x600..=0x6FF => Script::Arabic,
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF => Script::Cjk,
        _ => return None,
    })
}

/// The legacy Windows code page for a language, by ISO 639-1 or -2 code.
fn for_language(language: &str) -> Option<u32> {
    Some(match language.to_ascii_lowercase().as_str() {
        "ru" | "rus" | "uk" | "ukr" | "bg" | "bul" | "be" | "bel" | "mk" | "mkd" | "mac" | "sr"
        | "srp" => 1251,
        "pl" | "pol" | "cs" | "ces" | "cze" | "sk" | "slk" | "slo" | "hu" | "hun" | "ro"
        | "ron" | "rum" | "hr" | "hrv" | "sl" | "slv" | "bs" | "bos" | "sq" | "sqi" | "alb" => 1250,
        "el" | "ell" | "gre" => 1253,
        "tr" | "tur" | "az" | "aze" => 1254,
        "he" | "heb" | "yi" | "yid" => 1255,
        "ar" | "ara" | "fa" | "fas" | "per" | "ur" | "urd" => 1256,
        "lt" | "lit" | "lv" | "lav" | "et" | "est" => 1257,
        "vi" | "vie" => 1258,
        "th" | "tha" => 874,
        "ja" | "jpn" => 932,
        "zh" | "zho" | "chi" | "chs" => 936,
        "cht" => 950,
        "ko" | "kor" => 949,
        "en" | "eng" | "fr" | "fra" | "fre" | "de" | "deu" | "ger" | "es" | "spa" | "it"
        | "ita" | "pt" | "por" | "nl" | "nld" | "dut" | "sv" | "swe" | "da" | "dan" | "no"
        | "nor" | "fi" | "fin" | "is" | "isl" | "ice" => 1252,
        _ => return None,
    })
}

/// The language tag at the end of a subtitle file's stem: "ru" in "movie.ru.srt".
pub fn language_of(path: &std::path::Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let (_, tag) = stem.rsplit_once(['.', '_', '-'])?;
    for_language(tag).map(|_| tag.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `text` in code page `cp`, the way an old subtitle file would have it.
    fn encode(text: &str, cp: u32) -> Vec<u8> {
        let (bytes, _, unmappable) = encoding(cp).unwrap().encode(text);
        assert!(!unmappable, "{text} has no code page {cp} form");
        bytes.into_owned()
    }

    #[test]
    fn guesses_the_code_page_from_the_text() {
        let samples = [
            ("C'est l'été, ça va très bien à Noël.", 1252),
            ("Zażółć gęślą jaźń, proszę pana.", 1250),
            ("Привет, как дела? Всё хорошо, спасибо.", 1251),
            ("Καλημέρα, τι κάνεις; Είμαι καλά.", 1253),
            ("こんにちは、元気ですか？ありがとう。", 932),
            ("你好，今天天气很好，我们去公园吧。", 936),
            ("안녕하세요, 오늘 날씨가 좋네요.", 949),
            ("Schöne Grüße aus München, gute Nacht.", 1252),
            ("Добрий вечір, як справи? Дякую, все гаразд.", 1251),
            ("Да.", 1251),
            ("שלום, מה שלומך? תודה רבה.", 1255),
            ("مرحبا، كيف حالك؟ شكرا جزيلا.", 1256),
        ];
        for (text, cp) in samples {
            let bytes = encode(text, cp);
            assert_eq!(detect(&bytes, None), cp, "{text}");
            assert_eq!(
                decode(&bytes, detect(&bytes, None), false).as_deref(),
                Some(text)
            );
        }
        assert_eq!(detect("plain ascii and UTF-8: été".as_bytes(), None), UTF8);
    }

    #[test]
    fn a_language_tag_decides() {
        // Bytes valid in 1250 and 1252 alike; the tag picks.
        let bytes = encode("Šťastný nový rok", 1250);
        assert_eq!(detect(&bytes, Some("cze")), 1250);
        let path = std::path::Path::new("/Users/me/Movies/Film.2020.ru.srt");
        assert_eq!(language_of(path).as_deref(), Some("ru"));
        assert_eq!(language_of(std::path::Path::new("Film.2020.srt")), None);
    }

    #[test]
    fn the_system_code_page_follows_the_preferred_language() {
        assert!(encoding(system_code_page()).is_some());
        assert_eq!(decode(b"caf\xe9", 1252, true).as_deref(), Some("café"));
        assert_eq!(decode(b"\x82\xa0", 932, true).as_deref(), Some("あ"));
        // A lone lead byte isn't Shift-JIS; loosely it decodes with a replacement.
        assert_eq!(decode(b"a\x82", 932, true), None);
        assert_eq!(decode(b"a\x82", 932, false).as_deref(), Some("a\u{fffd}"));
        assert_eq!(decode(b"x", 12345, false), None);
    }
}
