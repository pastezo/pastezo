//! What the UI needs to know about the user's OS: which one it is, the
//! preferred language and the date/time conventions from the OS settings.

pub struct SystemInfo {
    /// "macos", "windows", "linux", "android", "ios".
    pub os: &'static str,
    /// Preferred UI languages, most preferred first (BCP 47, e.g. "ru-RU").
    pub languages: Vec<String>,
    /// Region from the OS settings (e.g. "RU"); drives date and time formats.
    pub region: Option<String>,
    /// `Some(true)` for 12-hour clock, `Some(false)` for 24-hour, `None` when
    /// the OS setting is not available and the UI derives it from `time_locale`
    /// or the language.
    pub hour12: Option<bool>,
    /// Locale that formats times when it differs from the UI language
    /// (Linux `LC_TIME`), BCP 47.
    pub time_locale: Option<String>,
}

pub fn system_info() -> SystemInfo {
    let (region, hour12, time_locale) = platform::conventions();
    SystemInfo {
        os: std::env::consts::OS,
        languages: sys_locale::get_locales().collect(),
        region,
        hour12,
        time_locale,
    }
}

type Conventions = (Option<String>, Option<bool>, Option<String>);

#[cfg(target_os = "macos")]
mod platform {
    use objc2_foundation::{NSDateFormatter, NSLocale, NSString};

    pub fn conventions() -> super::Conventions {
        let locale = NSLocale::currentLocale();
        #[allow(deprecated)] // the replacement, `regionCode`, needs macOS 13; we support 10.15+
        let region = locale.countryCode().map(|r| r.to_string());
        // macOS has its own "24-hour time" switch; the "j" template resolves to
        // the hour symbol the user actually sees ("h" + "a" means 12-hour).
        let hour12 = NSDateFormatter::dateFormatFromTemplate_options_locale(
            &NSString::from_str("j"),
            0,
            Some(&locale),
        )
        .map(|f| f.to_string().contains('a'));
        (region, hour12, None)
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use windows_sys::Win32::Globalization::{
        GetLocaleInfoEx, LOCALE_SISO3166CTRYNAME, LOCALE_STIMEFORMAT,
    };

    /// Reads a value of the user's regional settings (Settings → Time & language).
    fn user_locale_info(kind: u32) -> Option<String> {
        let mut buf = [0u16; 128];
        // null locale name = LOCALE_NAME_USER_DEFAULT
        let len = unsafe { GetLocaleInfoEx(std::ptr::null(), kind, buf.as_mut_ptr(), buf.len() as i32) };
        (len > 1).then(|| String::from_utf16_lossy(&buf[..len as usize - 1]))
    }

    pub fn conventions() -> super::Conventions {
        let region = user_locale_info(LOCALE_SISO3166CTRYNAME);
        // "HH:mm:ss" is 24-hour, "h:mm:ss tt" is 12-hour
        let hour12 = user_locale_info(LOCALE_STIMEFORMAT).map(|f| !f.contains('H'));
        (region, hour12, None)
    }
}

#[cfg(target_os = "linux")]
mod platform {
    /// "en_GB.UTF-8" / "en_GB@euro" -> "en-GB"; "C" and "POSIX" mean "not set".
    fn posix_to_bcp47(value: &str) -> Option<String> {
        let tag = value.split(['.', '@']).next()?.replace('_', "-");
        (!tag.is_empty() && tag != "C" && tag != "POSIX").then_some(tag)
    }

    pub fn conventions() -> super::Conventions {
        let time_locale = ["LC_ALL", "LC_TIME", "LANG"]
            .iter()
            .filter_map(|v| std::env::var(v).ok())
            .find(|v| !v.is_empty())
            .and_then(|v| posix_to_bcp47(&v));
        let region = time_locale
            .as_deref()
            .and_then(|t| t.split('-').nth(1))
            .filter(|r| r.len() == 2)
            .map(str::to_uppercase);
        (region, None, time_locale)
    }
}

// Android / iOS: the language comes from sys-locale; the 24-hour switch
// (Android `DateFormat.is24HourFormat`) needs a Kotlin/Swift plugin — planned.
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod platform {
    pub fn conventions() -> super::Conventions {
        (None, None, None)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_system_settings() {
        let s = super::system_info();
        println!(
            "os={} languages={:?} region={:?} hour12={:?} time_locale={:?}",
            s.os, s.languages, s.region, s.hour12, s.time_locale
        );
        assert!(!s.languages.is_empty());
        #[cfg(target_os = "macos")]
        assert!(s.region.is_some() && s.hour12.is_some());
    }
}
