//! Settings → Statistics: how much was copied today, in the last 7 and 30
//! days, and per day by kind (text, link, image).
//!
//! The agent counts every copy per 15 minutes (`copy_stats` in clip-core);
//! here those buckets are added up into the user's local days, so a day starts
//! at local midnight, daylight saving time included.

use chrono::{DateTime, Days, Local, NaiveDate, TimeZone};
use clip_core::{CopyCount, CopyKind, History};
use slint::{ModelRc, SharedString, VecModel};

use crate::i18n::I18n;
use crate::{SettingsWindow, StatsDay};

/// Days in the long chart ("Last 30 days"); the short one shows the last 7.
const DAYS: usize = 30;
const WEEK: usize = 7;

/// Copies of each kind on one local day.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Day {
    text: u32,
    link: u32,
    image: u32,
}

impl Day {
    fn total(&self) -> u32 {
        self.text + self.link + self.image
    }
}

/// Local midnight of `date` (the first valid moment of it: a DST switch at
/// midnight skips it in a few time zones).
fn start_of(date: NaiveDate) -> i64 {
    Local
        .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .or_else(|| Local.from_local_datetime(&date.and_hms_opt(1, 0, 0).unwrap()).earliest())
        .map_or(0, |t| t.timestamp_millis())
}

/// The last `DAYS` local days, oldest first, ending with `today`.
fn by_day(counts: &[CopyCount], today: NaiveDate) -> [Day; DAYS] {
    let first = today.checked_sub_days(Days::new(DAYS as u64 - 1)).unwrap_or(today);
    // where each day starts, plus the end of today
    let starts: Vec<i64> = (0..=DAYS as u64).map(|i| start_of(first.checked_add_days(Days::new(i)).unwrap_or(first))).collect();
    let mut days = [Day::default(); DAYS];
    for c in counts {
        let Some(i) = (0..DAYS).find(|&i| c.at_ms >= starts[i] && c.at_ms < starts[i + 1]) else { continue };
        let slot = match c.kind {
            CopyKind::Text => &mut days[i].text,
            CopyKind::Link => &mut days[i].link,
            CopyKind::Image => &mut days[i].image,
        };
        *slot += c.count;
    }
    days
}

/// Fills the Statistics tab from the history (a few dozen rows: cheap).
pub fn show(w: &SettingsWindow, history: &History, i18n: &I18n, now: DateTime<Local>) {
    let today = now.date_naive();
    let first = today.checked_sub_days(Days::new(DAYS as u64 - 1)).unwrap_or(today);
    let counts = history.copy_stats(start_of(first)).unwrap_or_default();
    let days = by_day(&counts, today);
    let (total, since) = history.copy_total().unwrap_or((0, None));

    let sum = |n: usize| days[DAYS - n..].iter().map(Day::total).sum::<u32>() as u64;
    w.set_stats_total(i18n.integer(total).into());
    w.set_stats_total_label(i18n.t("stats.total", &[("count", &total.to_string())]).into());
    w.set_stats_since(match since.and_then(|ms| Local.timestamp_millis_opt(ms).single()) {
        Some(t) => i18n.t("stats.since", &[("date", &i18n.day_month(t.date_naive()))]).into(),
        None => SharedString::default(),
    });
    w.set_stats_today(i18n.integer(sum(1)).into());
    w.set_stats_week(i18n.integer(sum(WEEK)).into());
    w.set_stats_month(i18n.integer(sum(DAYS)).into());

    // the last 7 days, newest first (like Todoist): "Mon · 16"
    let week: Vec<StatsDay> = (0..WEEK)
        .map(|back| {
            let d = days[DAYS - 1 - back];
            let date = today.checked_sub_days(Days::new(back as u64)).unwrap_or(today);
            StatsDay {
                label: format!("{} · {}", i18n.weekday(date), i18n.integer(d.total() as u64)).into(),
                text: d.text as i32,
                link: d.link as i32,
                image: d.image as i32,
                total: d.total() as i32,
            }
        })
        .collect();
    w.set_stats_week_max(week.iter().map(|d| d.total).max().unwrap_or(0));
    w.set_stats_days(ModelRc::new(VecModel::from(week)));

    // the last 30 days, oldest first
    let month: Vec<i32> = days.iter().map(|d| d.total() as i32).collect();
    w.set_stats_month_max(month.iter().copied().max().unwrap_or(0));
    w.set_stats_month_days(ModelRc::new(VecModel::from(month)));
    w.set_stats_month_from(i18n.day_month(first).into());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_go_to_their_local_day() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 28).unwrap();
        let at = |month: u32, d: u32, h: u32, m: u32| {
            Local.from_local_datetime(&NaiveDate::from_ymd_opt(2026, month, d).unwrap().and_hms_opt(h, m, 0).unwrap()).earliest().unwrap().timestamp_millis()
        };
        let counts = [
            CopyCount { at_ms: at(9, 28, 0, 0), kind: CopyKind::Text, count: 2 },
            CopyCount { at_ms: at(9, 28, 23, 45), kind: CopyKind::Link, count: 1 },
            CopyCount { at_ms: at(9, 27, 23, 45), kind: CopyKind::Image, count: 3 },
            CopyCount { at_ms: at(8, 29, 12, 0), kind: CopyKind::Text, count: 9 }, // before the 30 days (from Aug 30)
        ];
        let days = by_day(&counts, today);
        assert_eq!(days[DAYS - 1], Day { text: 2, link: 1, image: 0 });
        assert_eq!(days[DAYS - 2], Day { text: 0, link: 0, image: 3 });
        assert_eq!(days.iter().map(Day::total).sum::<u32>(), 6);
    }
}
