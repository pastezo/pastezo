//! UI language and date/time formats, both taken from the OS settings.
//!
//! Messages live in `locales/<BCP 47 tag>.json`, ICU MessageFormat syntax;
//! `en.json` is the source of truth (see `locales/README.md`). Every file is
//! embedded, only the chosen language is parsed. Dates and times use ICU4X —
//! the same CLDR data browsers use, so "5 мар. 2025 г." / "9:00 PM" come out
//! exactly as before.

use std::collections::HashMap;

use chrono::{Datelike, Local, TimeZone, Timelike};
use icu_datetime::fieldsets::{E, MD, T, YMD};
use icu_datetime::input::{Date, Time};
use icu_datetime::preferences::HourCycle;
use icu_datetime::{DateTimeFormatterPreferences, FixedCalendarDateTimeFormatter, NoCalendarFormatter};
use icu_locale::Locale;
use icu_plurals::PluralRules;

use crate::messages::{self, Message, Rules, Run, Value};
use crate::system::SystemInfo;

include!(concat!(env!("OUT_DIR"), "/locales.rs"));

const SOURCE: &str = "en";
const RTL: &[&str] = &["ar", "fa", "he", "ur"];

type Gregorian = icu_calendar::Gregorian;

pub struct I18n {
    pub rtl: bool,
    decimal: Option<icu_decimal::DecimalFormatter>,
    messages: HashMap<String, Message>,
    fallback: HashMap<String, Message>,
    rules: Rules,
    time: Option<NoCalendarFormatter<T>>,
    day_month: Option<FixedCalendarDateTimeFormatter<Gregorian, MD>>,
    day_month_year: Option<FixedCalendarDateTimeFormatter<Gregorian, YMD>>,
    weekday: Option<FixedCalendarDateTimeFormatter<Gregorian, E>>,
}

fn load(tag: &str) -> HashMap<String, Message> {
    let Some((_, json)) = LOCALES.iter().find(|(t, _)| *t == tag) else {
        return HashMap::new();
    };
    let raw: HashMap<String, String> = serde_json::from_str(json).unwrap_or_default();
    raw.into_iter()
        .filter_map(|(k, v)| match messages::parse(&v) {
            Ok(m) => Some((k, m)),
            Err(e) => {
                eprintln!("locale {tag}: {k}: {}", e.0);
                None
            }
        })
        .collect()
}

impl I18n {
    pub fn new(sys: &SystemInfo) -> Self {
        let lang = resolve_language(&sys.languages);
        let locale = format_locale(&lang, sys.region.as_deref());
        let base = lang.split('-').next().unwrap_or(&lang);

        let mut prefs = DateTimeFormatterPreferences::from(&locale);
        prefs.hour_cycle = clock_of(sys).map(|h12| if h12 { HourCycle::H12 } else { HourCycle::H23 });

        I18n {
            rtl: RTL.contains(&base),
            decimal: icu_decimal::DecimalFormatter::try_new((&locale).into(), Default::default()).ok(),
            messages: load(&lang),
            fallback: if lang == SOURCE { HashMap::new() } else { load(SOURCE) },
            rules: Rules {
                cardinal: PluralRules::try_new_cardinal((&locale).into()).ok(),
                ordinal: PluralRules::try_new_ordinal((&locale).into()).ok(),
            },
            time: NoCalendarFormatter::try_new(prefs, T::hm()).ok(),
            day_month: FixedCalendarDateTimeFormatter::try_new(prefs, MD::medium()).ok(),
            day_month_year: FixedCalendarDateTimeFormatter::try_new(prefs, YMD::medium()).ok(),
            weekday: FixedCalendarDateTimeFormatter::try_new(prefs, E::medium()).ok(),
        }
    }

    /// A number with up to two decimals, as the user's language writes it
    /// ("1,45", "1.5", "17").
    pub fn decimal(&self, value: f32) -> String {
        let (mut n, mut exp) = ((value as f64 * 100.0).round() as i64, -2i16);
        while exp < 0 && n % 10 == 0 {
            n /= 10;
            exp += 1;
        }
        let mut d = icu_decimal::input::Decimal::from(n);
        d.absolute.multiply_pow10(exp);
        match &self.decimal {
            Some(f) => f.format(&d).to_string(),
            None => d.to_string(),
        }
    }

    /// A whole number with the language's digit grouping ("1 234", "1,234").
    pub fn integer(&self, value: u64) -> String {
        let d = icu_decimal::input::Decimal::from(value);
        match &self.decimal {
            Some(f) => f.format(&d).to_string(),
            None => d.to_string(),
        }
    }

    /// The short weekday of `date` ("Mon", "пн").
    pub fn weekday(&self, date: chrono::NaiveDate) -> String {
        Date::try_new_gregorian(date.year(), date.month() as u8, date.day() as u8)
            .ok()
            .and_then(|d| Some(self.weekday.as_ref()?.format(&d).to_string()))
            .unwrap_or_else(|| date.format("%a").to_string())
    }

    /// Day and month of `date` ("Sep 28", "28 сент.").
    pub fn day_month(&self, date: chrono::NaiveDate) -> String {
        Date::try_new_gregorian(date.year(), date.month() as u8, date.day() as u8)
            .ok()
            .and_then(|d| Some(self.day_month.as_ref()?.format(&d).to_string()))
            .unwrap_or_else(|| date.to_string())
    }

    pub fn day_heading(&self, day: chrono::NaiveDate, today: chrono::NaiveDate) -> String {
        if day == today { return self.t("clip.today", &[]); }
        if Some(day) == today.pred_opt() { return self.t("clip.yesterday", &[]); }
        Date::try_new_gregorian(day.year(), day.month() as u8, day.day() as u8).ok()
            .and_then(|d| self.day_month_year.as_ref().map(|f| f.format(&d).to_string()))
            .unwrap_or_else(|| day.to_string())
    }

    fn message(&self, key: &str) -> Option<&Message> {
        self.messages.get(key).or_else(|| self.fallback.get(key))
    }

    /// Message split into runs, keeping `<b>…</b>` as bold runs.
    pub fn runs(&self, key: &str, args: &[(&str, &str)]) -> Vec<Run> {
        let Some(msg) = self.message(key) else {
            return vec![Run { text: key.to_string(), bold: false }];
        };
        let args: HashMap<&str, Value> = args.iter().map(|(k, v)| (*k, Value::Str(v))).collect();
        messages::format(msg, &args, &self.rules)
    }

    /// Plain text message.
    pub fn t(&self, key: &str, args: &[(&str, &str)]) -> String {
        self.runs(key, args).into_iter().map(|r| r.text).collect()
    }

    /// "Today at 21:49", "Yesterday at …", "5 мар. 2025 г. в …" in the user's
    /// language and clock. `ms` is Unix time in milliseconds.
    pub fn timestamp(&self, ms: i64, now: chrono::DateTime<Local>) -> Vec<Run> {
        let Some(when) = Local.timestamp_millis_opt(ms).single() else {
            return Vec::new();
        };
        let today = now.date_naive();
        let date = when.date_naive();
        let day = if date == today {
            self.t("clip.today", &[])
        } else if Some(date) == today.pred_opt() {
            self.t("clip.yesterday", &[])
        } else {
            let d = Date::try_new_gregorian(date.year(), date.month() as u8, date.day() as u8);
            match (d, date.year() == today.year()) {
                (Ok(d), true) => self.day_month.as_ref().map(|f| f.format(&d).to_string()).unwrap_or_default(),
                (Ok(d), false) => self.day_month_year.as_ref().map(|f| f.format(&d).to_string()).unwrap_or_default(),
                (Err(_), _) => date.to_string(),
            }
        };
        let time = Time::try_new(when.hour() as u8, when.minute() as u8, 0, 0)
            .ok()
            .and_then(|t| Some(self.time.as_ref()?.format(&t).to_string()))
            .unwrap_or_else(|| format!("{:02}:{:02}", when.hour(), when.minute()));
        self.runs("clip.timestamp", &[("day", &day), ("time", &time)])
    }
}

/// 12/24-hour clock: the OS switch if there is one, else the OS time locale
/// (Linux LC_TIME), else the language's default.
fn clock_of(sys: &SystemInfo) -> Option<bool> {
    if sys.hour12.is_some() {
        return sys.hour12;
    }
    let tl: Locale = sys.time_locale.as_deref()?.parse().ok()?;
    // ask CLDR what that locale uses
    let f = NoCalendarFormatter::try_new((&tl).into(), T::hm()).ok()?;
    let sample = f.format(&Time::try_new(13, 0, 0, 0).ok()?).to_string();
    Some(!sample.contains("13"))
}

/// "ru" + "RU" -> "ru-RU": language of the UI, conventions of the user's region.
fn format_locale(lang: &str, region: Option<&str>) -> Locale {
    let with_region = region
        .filter(|_| !lang.split('-').any(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_uppercase())))
        .map(|r| format!("{lang}-{r}"));
    with_region
        .and_then(|t| t.parse().ok())
        .or_else(|| lang.parse().ok())
        .unwrap_or_else(|| SOURCE.parse().unwrap())
}

fn available() -> impl Iterator<Item = &'static str> {
    LOCALES.iter().map(|(t, _)| *t)
}

/// Picks the first preferred OS language we have a translation for.
/// Tags come as BCP 47 ("zh-Hant-TW", "pt-BR", "sr-Latn-RS", "nb-NO").
pub fn resolve_language(preferred: &[String]) -> String {
    let have: Vec<&str> = available().collect();
    for tag in preferred {
        let tag = tag.replace('_', "-");
        let mut parts = tag.split('-');
        let raw = parts.next().unwrap_or_default().to_ascii_lowercase();
        let rest: Vec<&str> = parts.collect();
        let base = match raw.as_str() {
            "iw" => "he".to_string(),
            "in" => "id".to_string(),
            "tl" => "fil".to_string(),
            "no" | "nn" => "nb".to_string(),
            _ => raw,
        };
        let script = rest.iter().find(|s| s.len() == 4).map(|s| s.to_ascii_lowercase());
        let region = rest
            .iter()
            .find(|s| s.len() == 2 || (s.len() == 3 && s.chars().all(|c| c.is_ascii_digit())))
            .map(|s| s.to_ascii_uppercase());

        let mut candidates = Vec::new();
        match base.as_str() {
            "zh" => {
                let traditional = script.as_deref() == Some("hant")
                    || (script.is_none() && matches!(region.as_deref(), Some("TW" | "HK" | "MO")));
                candidates.push(if traditional { "zh-Hant" } else { "zh-Hans" }.to_string());
            }
            // Apple and Google treat plain "pt" as Brazilian Portuguese
            "pt" => candidates.push(if matches!(region.as_deref(), None | Some("BR")) { "pt-BR" } else { "pt-PT" }.to_string()),
            "sr" => candidates.push(if script.as_deref() == Some("latn") { "sr-Latn" } else { "sr" }.to_string()),
            _ => {
                if let Some(r) = &region {
                    candidates.push(format!("{base}-{r}"));
                }
                candidates.push(base.clone());
            }
        }
        if let Some(hit) = candidates.iter().find(|c| have.contains(&c.as_str())) {
            return hit.clone();
        }
    }
    SOURCE.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sys(langs: &[&str], region: Option<&str>, hour12: Option<bool>) -> SystemInfo {
        SystemInfo {
            os: "macos",
            languages: langs.iter().map(|s| s.to_string()).collect(),
            region: region.map(String::from),
            hour12,
            time_locale: None,
        }
    }

    fn text(runs: Vec<Run>) -> String {
        runs.into_iter().map(|r| r.text).collect()
    }

    #[test]
    fn language_resolution() {
        let r = |l: &[&str]| resolve_language(&l.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(r(&["ru-RU"]), "ru");
        assert_eq!(r(&["zh-TW"]), "zh-Hant");
        assert_eq!(r(&["zh-Hans-CN"]), "zh-Hans");
        assert_eq!(r(&["xx-YY", "pt"]), "pt-BR");
        assert_eq!(r(&["pt-PT"]), "pt-PT");
        assert_eq!(r(&["sr-Latn-RS"]), "sr-Latn");
        assert_eq!(r(&["nb-NO"]), "nb");
        assert_eq!(r(&["iw-IL"]), "he");
        assert_eq!(r(&["xx"]), "en");
    }

    #[test]
    fn timestamps_match_the_web_version() {
        let now = Local.with_ymd_and_hms(2026, 9, 27, 22, 0, 0).unwrap();
        let at = |y, mo, d, h, mi| Local.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap().timestamp_millis();

        let ru = I18n::new(&sys(&["ru-RU"], Some("RU"), Some(false)));
        assert_eq!(text(ru.timestamp(at(2026, 9, 27, 21, 59), now)), "Сегодня в 21:59");
        assert_eq!(text(ru.timestamp(at(2026, 9, 26, 22, 0), now)), "Вчера в 22:00");
        assert_eq!(text(ru.timestamp(at(2025, 3, 5, 14, 7), now)), "5 мар. 2025\u{202f}г. в 14:07");

        let en = I18n::new(&sys(&["en-US"], Some("US"), Some(true)));
        assert_eq!(text(en.timestamp(at(2026, 9, 27, 21, 59), now)), "Today at 9:59\u{202f}PM");
        assert_eq!(text(en.timestamp(at(2025, 3, 5, 14, 7), now)), "Mar 5, 2025 at 2:07\u{202f}PM");
        let en24 = I18n::new(&sys(&["en-US"], Some("US"), Some(false)));
        assert_eq!(text(en24.timestamp(at(2026, 9, 27, 21, 59), now)), "Today at 21:59");

        let ja = I18n::new(&sys(&["ja-JP"], Some("JP"), Some(false)));
        assert_eq!(text(ja.timestamp(at(2025, 3, 5, 14, 7), now)), "2025/03/05 14:07"); // ICU4X "medium" in Japanese is numeric
    }

    #[test]
    fn copied_from_is_bold_where_the_app_is() {
        let ja = I18n::new(&sys(&["ja"], None, None));
        let runs = ja.runs("clip.copiedFrom", &[("app", "Safari")]);
        assert_eq!(runs[0], Run { text: "Safari".into(), bold: true });
        assert!(I18n::new(&sys(&["ar"], None, None)).rtl);
    }

    /// Every locale has exactly the keys of en.json, with the same
    /// placeholders and tags, and parses as ICU MessageFormat.
    #[test]
    fn locales_are_complete_and_valid() {
        let source: HashMap<String, String> = serde_json::from_str(LOCALES.iter().find(|(t, _)| *t == SOURCE).unwrap().1).unwrap();
        let sig = |m: &str| messages::signature(&messages::parse(m).unwrap());
        let mut problems = Vec::new();
        for (tag, json) in LOCALES {
            if tag.parse::<Locale>().is_err() {
                problems.push(format!("{tag}: not a BCP 47 tag"));
            }
            let map: HashMap<String, String> = match serde_json::from_str(json) {
                Ok(m) => m,
                Err(e) => {
                    problems.push(format!("{tag}: invalid JSON: {e}"));
                    continue;
                }
            };
            for key in source.keys() {
                if !map.contains_key(key) {
                    problems.push(format!("{tag}: missing {key}"));
                }
            }
            for (key, value) in &map {
                let Some(src) = source.get(key) else {
                    problems.push(format!("{tag}: unknown key {key}"));
                    continue;
                };
                if value.trim().is_empty() {
                    problems.push(format!("{tag}: {key} is empty"));
                    continue;
                }
                match messages::parse(value) {
                    Err(e) => problems.push(format!("{tag}: {key}: {}", e.0)),
                    Ok(m) => {
                        if messages::signature(&m) != sig(src) {
                            problems.push(format!("{tag}: {key}: placeholders/tags differ from {SOURCE}"));
                        }
                    }
                }
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    #[test]
    fn decimals_in_the_users_language() {
        assert_eq!(I18n::new(&sys(&["en"], None, None)).decimal(1.45), "1.45");
        assert_eq!(I18n::new(&sys(&["ru"], None, None)).decimal(1.5), "1,5");
        assert_eq!(I18n::new(&sys(&["de"], None, None)).decimal(17.0), "17");
    }
}
