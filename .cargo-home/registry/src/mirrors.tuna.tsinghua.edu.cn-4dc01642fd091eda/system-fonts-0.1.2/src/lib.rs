//! System font discovery and locale-based preset selection.
//!
//! Detects the current system locale, maps it to a [`FontRegion`], and resolves
//! a prioritized list of installed fonts. On wasm, font discovery is not supported
//! and the `find_*` functions return empty results.
//!
//! ```no_run
//! use system_fonts::{find_for_system_locale, FontStyle};
//!
//! let (_locale, region, fonts) = find_for_system_locale(FontStyle::Sans);
//! println!("region={region:?}, fonts={}", fonts.len());
//! ```
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use fontdb::{Database, Family, Query, Source};
#[cfg(not(target_arch = "wasm32"))]
use std::collections::HashSet;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::OnceLock;

/// Font preference used when selecting system fonts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontStyle {
    Sans,
    Serif,
}

/// Writing system/locale region used to decide fallback priority.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontRegion {
    Korean,
    Japanese,
    SimplifiedChinese,
    TraditionalChinese,
    Cyrillic,
    Latin,
    Unknown,
}

/// A preset represents a prioritized group of candidate font families.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum FontPreset {
    Latin,
    Korean,
    SimplifiedChinese,
    TraditionalChinese,
    Japanese,
    Cyrillic,
    /// Custom font family names, in priority order.
    Custom(Vec<String>),
}

/// A resolved system font entry usable by UI code.
///
/// `family` is the human-readable family name used for lookup.
/// `key` is a unique identifier suitable as a UI font key within the current process.
/// It is not guaranteed to be stable across machines or across runs.
#[derive(Clone, Debug)]
pub struct FoundFont {
    pub family: String,
    pub key: String,
    pub source: FoundFontSource,
}

/// Font bytes source resolved from the system font database.
///
/// `Path` points to an on-disk font file.
/// `Bytes` contains the font data copied into memory (can be large).
#[derive(Clone, Debug)]
pub enum FoundFontSource {
    Path(PathBuf),
    Bytes(Arc<[u8]>),
}

/// Returns the current system locale string (e.g. `"ko-KR"`, `"en-US"`).
///
/// On WASM, reads `navigator.language` from the browser.
#[cfg(not(target_arch = "wasm32"))]
pub fn system_locale() -> Option<String> {
    sys_locale::get_locale()
}

#[cfg(target_arch = "wasm32")]
pub fn system_locale() -> Option<String> {
    web_sys::window()
        .and_then(|w| w.navigator().language())
}

/// Maps a locale string (BCP-47 or POSIX style) to a [`FontRegion`].
///
/// ```
/// use system_fonts::{region_from_locale, FontRegion};
///
/// assert_eq!(region_from_locale("ko-KR"), FontRegion::Korean);
/// assert_eq!(region_from_locale("ko_KR.UTF-8"), FontRegion::Korean);
/// assert_eq!(region_from_locale("zh-Hant-TW"), FontRegion::TraditionalChinese);
/// assert_eq!(region_from_locale("zh_CN"), FontRegion::SimplifiedChinese);
/// assert_eq!(region_from_locale("ru-RU"), FontRegion::Cyrillic);
/// ```
pub fn region_from_locale(locale: &str) -> FontRegion {
    let mut s = locale.trim().to_ascii_lowercase().replace('_', "-");
    if let Some((head, _)) = s.split_once('.') {
        s = head.to_string();
    }

    if s.contains("-cyrl") {
        return FontRegion::Cyrillic;
    }
    if s.contains("-latn") {
        return FontRegion::Latin;
    }

    if s.starts_with("ko") {
        return FontRegion::Korean;
    }
    if s.starts_with("ja") {
        return FontRegion::Japanese;
    }
    if s.starts_with("zh") {
        if s.contains("hant") || s.contains("-tw") || s.contains("-hk") || s.contains("-mo") {
            return FontRegion::TraditionalChinese;
        }
        return FontRegion::SimplifiedChinese;
    }

    if s.starts_with("ru")
        || s.starts_with("uk")
        || s.starts_with("be")
        || s.starts_with("bg")
        || s.starts_with("mk")
        || s.starts_with("sr")
        || s.starts_with("kk")
        || s.starts_with("ky")
        || s.starts_with("tg")
        || s.starts_with("mn")
    {
        return FontRegion::Cyrillic;
    }

    if s.starts_with("en") || s.starts_with("fr") || s.starts_with("de") {
        return FontRegion::Latin;
    }

    FontRegion::Unknown
}

/// Returns the default preset priority list for a region (highest priority first).
///
/// ```
/// use system_fonts::{presets_for_region, FontRegion, FontPreset};
///
/// let presets = presets_for_region(FontRegion::Korean);
/// assert!(matches!(presets.first(), Some(FontPreset::Korean)));
/// ```
pub fn presets_for_region(region: FontRegion) -> Vec<FontPreset> {
    match region {
        FontRegion::Korean => vec![
            FontPreset::Korean,
            FontPreset::Japanese,
            FontPreset::SimplifiedChinese,
            FontPreset::TraditionalChinese,
            FontPreset::Latin,
        ],
        FontRegion::Japanese => vec![
            FontPreset::Japanese,
            FontPreset::Korean,
            FontPreset::SimplifiedChinese,
            FontPreset::TraditionalChinese,
            FontPreset::Latin,
        ],
        FontRegion::SimplifiedChinese => vec![
            FontPreset::SimplifiedChinese,
            FontPreset::TraditionalChinese,
            FontPreset::Korean,
            FontPreset::Japanese,
            FontPreset::Latin,
        ],
        FontRegion::TraditionalChinese => vec![
            FontPreset::TraditionalChinese,
            FontPreset::SimplifiedChinese,
            FontPreset::Korean,
            FontPreset::Japanese,
            FontPreset::Latin,
        ],
        FontRegion::Cyrillic => vec![
            FontPreset::Cyrillic,
            FontPreset::Latin,
            FontPreset::Korean,
            FontPreset::SimplifiedChinese,
            FontPreset::TraditionalChinese,
            FontPreset::Japanese,
        ],
        FontRegion::Latin | FontRegion::Unknown => vec![
            FontPreset::Latin,
            FontPreset::Cyrillic,
            FontPreset::Korean,
            FontPreset::SimplifiedChinese,
            FontPreset::TraditionalChinese,
            FontPreset::Japanese,
        ],
    }
}

/// Resolves installed system fonts from presets, ordered by priority.
///
/// On wasm, always returns an empty list.
///
/// ```no_run
/// use system_fonts::{find_from_presets, FontPreset, FontStyle};
///
/// let fonts = find_from_presets([FontPreset::Korean, FontPreset::Latin], FontStyle::Sans);
/// println!("fonts={}", fonts.len());
/// ```
#[cfg(not(target_arch = "wasm32"))]
pub fn find_from_presets<I>(presets_in_priority: I, style: FontStyle) -> Vec<FoundFont>
where
    I: IntoIterator<Item = FontPreset>,
{
    let db = font_db();

    let mut targets: Vec<String> = Vec::new();
    for preset in presets_in_priority {
        match style {
            FontStyle::Serif => {
                targets.extend(preset_targets_serif(&preset));
                targets.extend(preset_targets_sans(&preset));
            }
            FontStyle::Sans => {
                targets.extend(preset_targets_sans(&preset));
            }
        }
    }

    let mut seen_family = HashSet::<String>::new();
    let mut out = Vec::<FoundFont>::new();

    for (i, family_name) in targets.into_iter().enumerate() {
        if !seen_family.insert(family_name.clone()) {
            continue;
        }

        if let Some(found) = resolve_one_family(db, &family_name, i) {
            out.push(found);
        }
    }

    out
}

#[cfg(target_arch = "wasm32")]
pub fn find_from_presets<I>(_presets_in_priority: I, _style: FontStyle) -> Vec<FoundFont>
where
    I: IntoIterator<Item = FontPreset>,
{
    vec![]
}

/// Resolves fonts for the given locale string. On wasm, returns an empty font list.
///
/// ```no_run
/// use system_fonts::{find_for_locale, FontStyle};
///
/// let (region, fonts) = find_for_locale("ja-JP", FontStyle::Sans);
/// println!("region={region:?}, fonts={}", fonts.len());
/// ```
#[cfg(not(target_arch = "wasm32"))]
pub fn find_for_locale(locale: &str, style: FontStyle) -> (FontRegion, Vec<FoundFont>) {
    let region = region_from_locale(locale);
    let presets = presets_for_region(region);
    (region, find_from_presets(presets, style))
}

#[cfg(target_arch = "wasm32")]
pub fn find_for_locale(locale: &str, _style: FontStyle) -> (FontRegion, Vec<FoundFont>) {
    (region_from_locale(locale), vec![])
}

/// Resolves fonts for the current system locale. On wasm, returns an empty font list.
///
/// ```no_run
/// use system_fonts::{find_for_system_locale, FontStyle};
///
/// let (_loc, region, fonts) = find_for_system_locale(FontStyle::Sans);
/// println!("region={region:?}, fonts={}", fonts.len());
/// ```
#[cfg(not(target_arch = "wasm32"))]
pub fn find_for_system_locale(style: FontStyle) -> (Option<String>, FontRegion, Vec<FoundFont>) {
    let locale = system_locale();
    let (region, fonts) = match locale.as_deref() {
        Some(loc) if !loc.trim().is_empty() => find_for_locale(loc, style),
        _ => {
            let fallback = "en-US";
            find_for_locale(fallback, style)
        }
    };
    (locale, region, fonts)
}

#[cfg(target_arch = "wasm32")]
pub fn find_for_system_locale(_style: FontStyle) -> (Option<String>, FontRegion, Vec<FoundFont>) {
    (None, FontRegion::Unknown, vec![])
}

#[cfg(not(target_arch = "wasm32"))]
static FONT_DB: OnceLock<Database> = OnceLock::new();

#[cfg(not(target_arch = "wasm32"))]
fn font_db() -> &'static Database {
    FONT_DB.get_or_init(|| {
        let mut db = Database::new();
        db.load_system_fonts();
        db
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn resolve_one_family(db: &Database, family_name: &str, uniq: usize) -> Option<FoundFont> {
    let families = [Family::Name(family_name)];
    let query = Query {
        families: &families,
        ..Default::default()
    };

    let id = db.query(&query)?;
    let face = db.face(id)?;

    let source = match &face.source {
        Source::File(path) => FoundFontSource::Path(path.to_path_buf()),
        Source::Binary(bytes) => {
            let v: Vec<u8> = bytes.as_ref().as_ref().to_vec();
            FoundFontSource::Bytes(Arc::from(v.into_boxed_slice()))
        }
        _ => return None,
    };

    let key = format!("system:{}:{}", family_name, uniq);

    Some(FoundFont {
        family: family_name.to_string(),
        key,
        source,
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn preset_targets_sans(p: &FontPreset) -> Vec<String> {
    match p {
        FontPreset::Latin => vec![
            "Noto Sans".into(),
            "Segoe UI".into(),
            "Arial".into(),
            "SF Pro Text".into(),
            "Helvetica Neue".into(),
            "DejaVu Sans".into(),
            "Liberation Sans".into(),
            "Roboto".into(),
        ],
        FontPreset::Korean => vec![
            "Noto Sans KR".into(),
            "Noto Sans CJK KR".into(),
            "Malgun Gothic".into(),
            "Apple SD Gothic Neo".into(),
            "NanumGothic".into(),
        ],
        FontPreset::SimplifiedChinese => vec![
            "Noto Sans SC".into(),
            "Noto Sans CJK SC".into(),
            "Microsoft YaHei".into(),
            "PingFang SC".into(),
            "SimHei".into(),
            "SimSun".into(),
        ],
        FontPreset::TraditionalChinese => vec![
            "Noto Sans TC".into(),
            "Noto Sans CJK TC".into(),
            "Microsoft JhengHei".into(),
            "PingFang TC".into(),
        ],
        FontPreset::Japanese => vec![
            "Noto Sans JP".into(),
            "Noto Sans CJK JP".into(),
            "Yu Gothic".into(),
            "Hiragino Sans".into(),
            "Meiryo".into(),
        ],
        FontPreset::Cyrillic => vec![
            "Noto Sans".into(),
            "DejaVu Sans".into(),
            "Segoe UI".into(),
            "Arial".into(),
            "Tahoma".into(),
            "Times New Roman".into(),
        ],
        FontPreset::Custom(list) => list.clone(),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn preset_targets_serif(p: &FontPreset) -> Vec<String> {
    match p {
        FontPreset::Latin => vec![
            "Noto Serif".into(),
            "Times New Roman".into(),
            "Georgia".into(),
            "Liberation Serif".into(),
            "DejaVu Serif".into(),
            "Times".into(),
        ],
        FontPreset::Korean => vec![
            "Noto Serif KR".into(),
            "Noto Serif CJK KR".into(),
            "Batang".into(),
            "AppleMyungjo".into(),
            "NanumMyeongjo".into(),
        ],
        FontPreset::SimplifiedChinese => vec![
            "Noto Serif SC".into(),
            "Noto Serif CJK SC".into(),
            "Songti SC".into(),
            "SimSun".into(),
        ],
        FontPreset::TraditionalChinese => vec![
            "Noto Serif TC".into(),
            "Noto Serif CJK TC".into(),
            "Songti TC".into(),
            "PMingLiU".into(),
        ],
        FontPreset::Japanese => vec![
            "Noto Serif JP".into(),
            "Noto Serif CJK JP".into(),
            "Yu Mincho".into(),
            "Hiragino Mincho ProN".into(),
            "MS Mincho".into(),
        ],
        FontPreset::Cyrillic => vec![
            "Noto Serif".into(),
            "Times New Roman".into(),
            "Georgia".into(),
            "Liberation Serif".into(),
            "DejaVu Serif".into(),
        ],
        FontPreset::Custom(list) => list.clone(),
    }
}
