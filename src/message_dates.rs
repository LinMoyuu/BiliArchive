//! Calendar selection and date bounds shared by the message UI and collection options.
use crate::{AppWindow, MessageCalendarCell, MessageCalendarDate};
use anyhow::{anyhow, bail, Context, Result};
use chrono::{Datelike, Local, NaiveDate, TimeZone};

pub fn parse(value: &str) -> Result<Option<NaiveDate>> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(i, b)| i != 4 && i != 7 && !b.is_ascii_digit())
    {
        bail!("日期格式应为 yyyy/MM/dd")
    }
    Ok(Some(
        NaiveDate::parse_from_str(value, "%Y-%m-%d").context("日期无效，请检查年月日")?,
    ))
}
pub fn bounds(from: &str, to: &str) -> Result<(u64, u64)> {
    let start = parse(from).context("开始日期无效")?;
    let end = parse(to).context("结束日期无效")?;
    if matches!((start,end),(Some(a),Some(b)) if a>b) {
        bail!("开始日期不能晚于结束日期")
    }
    fn timestamp(date: Option<NaiveDate>, end: bool) -> Result<u64> {
        let Some(mut date) = date else { return Ok(0) };
        if date.year() < 1970 {
            bail!("日期不能早于 1970 年")
        }
        if end {
            date = date.succ_opt().ok_or_else(|| anyhow!("结束日期超出范围"))?;
        }
        let t = Local
            .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .ok_or_else(|| anyhow!("无效本地日期"))?
            .timestamp();
        if t < 0 {
            bail!("日期不能早于 1970 年")
        }
        Ok(if end {
            t.saturating_sub(1) as u64
        } else {
            t as u64
        })
    }
    Ok((timestamp(start, false)?, timestamp(end, true)?))
}
fn seed(value: &str, today: NaiveDate) -> NaiveDate {
    parse(value).ok().flatten().unwrap_or(today)
}
fn auto_value(current: &str, automatic: bool, today: NaiveDate) -> Option<String> {
    let today = today.format("%Y-%m-%d").to_string();
    (automatic && current != today).then_some(today)
}
fn calendar_date(date: NaiveDate) -> MessageCalendarDate {
    MessageCalendarDate {
        year: date.year(),
        month: date.month() as i32,
        day: date.day() as i32,
    }
}
pub fn reset(ui: &AppWindow) {
    ui.set_message_from("".into());
    ui.set_message_to(
        Local::now()
            .date_naive()
            .format("%Y-%m-%d")
            .to_string()
            .into(),
    );
    ui.set_message_date_auto_end(true);
    ui.set_message_date_error("".into());
    ui.set_message_calendar_generation(ui.get_message_calendar_generation().wrapping_add(1));
}
pub fn refresh_automatic_end(ui: &AppWindow) {
    if let Some(value) = auto_value(
        ui.get_message_to().as_str(),
        ui.get_message_date_auto_end(),
        Local::now().date_naive(),
    ) {
        ui.set_message_to(value.into());
    }
    clear_corrected_error(ui);
}
fn clear_corrected_error(ui: &AppWindow) {
    if !ui.get_message_date_error().is_empty()
        && bounds(ui.get_message_from().as_str(), ui.get_message_to().as_str()).is_ok()
    {
        ui.set_message_date_error("".into());
    }
}
pub fn validate(ui: &AppWindow) -> bool {
    match bounds(ui.get_message_from().as_str(), ui.get_message_to().as_str()) {
        Ok(_) => {
            ui.set_message_date_error("".into());
            true
        }
        Err(error) => {
            ui.set_message_date_error(format!("{:#}", error).into());
            false
        }
    }
}
fn check_calendar_candidate(from: &str, to: &str, field: i32, date: NaiveDate) -> Result<()> {
    let value = date.format("%Y-%m-%d").to_string();
    match field {
        0 => bounds(&value, to)?,
        1 => bounds(from, &value)?,
        _ => bail!("未知日期范围端点"),
    };
    Ok(())
}
fn accept(ui: &AppWindow, field: i32, date: MessageCalendarDate) -> Result<()> {
    let date = NaiveDate::from_ymd_opt(date.year, date.month as u32, date.day as u32)
        .ok_or_else(|| anyhow!("日历选择的日期无效"))?;
    check_calendar_candidate(
        ui.get_message_from().as_str(),
        ui.get_message_to().as_str(),
        field,
        date,
    )?;
    let value = date.format("%Y-%m-%d").to_string();
    if field == 0 {
        ui.set_message_from(value.into());
    } else {
        ui.set_message_to(value.into());
        ui.set_message_date_auto_end(false);
    }
    validate(ui);
    Ok(())
}
fn cells(date: NaiveDate, today: NaiveDate) -> Vec<MessageCalendarCell> {
    let first = NaiveDate::from_ymd_opt(date.year(), date.month(), 1).unwrap();
    let offset = first.weekday().num_days_from_monday() as i32;
    (0..42)
        .map(|i| {
            let day = i - offset + 1;
            let value = if day > 0 {
                NaiveDate::from_ymd_opt(date.year(), date.month(), day as u32)
            } else {
                None
            };
            MessageCalendarCell {
                day: if value.is_some() { day } else { 0 },
                selected: value == Some(date),
                today: value == Some(today),
            }
        })
        .collect()
}
fn prepare_calendar(ui: &AppWindow, date: NaiveDate) {
    ui.set_message_calendar_date(calendar_date(date));
    ui.set_message_calendar_year(date.year());
    ui.set_message_calendar_month(date.month() as i32);
    ui.set_message_calendar_days(slint::ModelRc::new(slint::VecModel::from(cells(
        date,
        Local::now().date_naive(),
    ))));
}
fn select_month(ui: &AppWindow, year: i32, month: i32) {
    if !(1970..=9999).contains(&year) || !(1..=12).contains(&month) {
        return;
    }
    let day = ui.get_message_calendar_date().day.clamp(1, 31);
    if let Some(date) = (1..=day)
        .rev()
        .find_map(|d| NaiveDate::from_ymd_opt(year, month as u32, d as u32))
    {
        prepare_calendar(ui, date);
    }
}
pub fn install(ui: &AppWindow, weak: &slint::Weak<AppWindow>) {
    ui.on_message_date_display(|value| {
        parse(value.as_str())
            .ok()
            .flatten()
            .map(|date| date.format("%Y/%m/%d").to_string().into())
            .unwrap_or_default()
    });
    reset(ui);
    {
        let weak = weak.clone();
        ui.on_message_date_clear(move |field| {
            if let Some(ui) = weak.upgrade() {
                match field {
                    0 => ui.set_message_from("".into()),
                    1 => {
                        ui.set_message_to("".into());
                        ui.set_message_date_auto_end(false);
                    }
                    _ => return,
                }
                validate(&ui);
            }
        });
    }
    {
        let weak = weak.clone();
        ui.on_message_calendar_open(move |field| {
            let today = Local::now().date_naive();
            let date = weak
                .upgrade()
                .map(|ui| {
                    let value = if field == 0 {
                        ui.get_message_from()
                    } else {
                        ui.get_message_to()
                    };
                    seed(value.as_str(), today)
                })
                .unwrap_or(today);
            if let Some(ui) = weak.upgrade() {
                prepare_calendar(&ui, date);
            }
            calendar_date(date)
        });
    }
    {
        let weak = weak.clone();
        ui.on_message_calendar_accept(move |field, date| {
            let Some(ui) = weak.upgrade() else {
                return false;
            };
            match accept(&ui, field, date) {
                Ok(()) => true,
                Err(error) => {
                    ui.set_message_date_error(format!("{:#}", error).into());
                    false
                }
            }
        });
    }
    {
        let weak = weak.clone();
        ui.on_message_calendar_month_changed(move |year, month| {
            if let Some(ui) = weak.upgrade() {
                select_month(&ui, year, month);
            }
        });
    }
    {
        let weak = weak.clone();
        ui.on_message_calendar_shift(move |delta| {
            if let Some(ui) = weak.upgrade() {
                let total = ui.get_message_calendar_year() * 12 + ui.get_message_calendar_month()
                    - 1
                    + delta;
                select_month(&ui, total.div_euclid(12), total.rem_euclid(12) + 1);
            }
        });
    }
    {
        let weak = weak.clone();
        ui.on_message_calendar_day_selected(move |day| {
            if let Some(ui) = weak.upgrade() {
                if let Some(date) = NaiveDate::from_ymd_opt(
                    ui.get_message_calendar_year(),
                    ui.get_message_calendar_month() as u32,
                    day as u32,
                ) {
                    prepare_calendar(&ui, date);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_format_leap_days_and_range_validation() {
        assert_eq!(
            parse("2024-02-29").unwrap(),
            NaiveDate::from_ymd_opt(2024, 2, 29)
        );
        for text in [
            "2026-02-29",
            "2026-13-01",
            "2026-02-30",
            "2026-2-03",
            "2026-02-",
            "2026/02/03",
            "abcd-ef-gh",
        ] {
            assert!(parse(text).is_err(), "{text}");
        }
        assert_eq!(bounds("", "").unwrap(), (0, 0));
        assert!(bounds("2026-02-04", "2026-02-03").is_err());
        let (from, to) = bounds("2026-02-03", "2026-02-03").unwrap();
        assert_eq!(to - from, 86399);
    }
    #[test]
    fn automatic_end_respects_explicit_selection_and_clear() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 22).unwrap();
        assert_eq!(
            auto_value("2026-11-21", true, today),
            Some("2026-11-22".into())
        );
        assert_eq!(auto_value("2026-11-22", true, today), None);
        for value in ["", "2020-01-01"] {
            assert_eq!(auto_value(value, false, today), None);
        }
        assert_eq!(seed("invalid", today), today);
        assert_eq!(
            seed("2024-02-29", today),
            NaiveDate::from_ymd_opt(2024, 2, 29).unwrap()
        );
    }
    #[test]
    fn calendar_candidates_validate_range_and_endpoint() {
        let date = NaiveDate::from_ymd_opt(2026, 2, 3).unwrap();
        assert!(check_calendar_candidate("", "2026-02-02", 0, date).is_err());
        assert!(check_calendar_candidate("2026-02-04", "", 1, date).is_err());
        assert!(check_calendar_candidate("2026-03-01", "2026-03-01", 0, date).is_ok());
        assert!(check_calendar_candidate("", "", 2, date).is_err());
        assert!(bounds("", "1969-12-31").is_err());
    }
    #[test]
    fn compact_calendar_grid_handles_leap_days_and_padding() {
        let leap = NaiveDate::from_ymd_opt(2024, 2, 29).unwrap();
        let grid = cells(leap, leap);
        assert_eq!(grid.len(), 42);
        assert_eq!(grid.iter().filter(|c| c.day > 0).count(), 29);
        assert_eq!(grid[3].day, 1);
        assert_eq!(grid[31].day, 29);
        assert!(grid[31].selected && grid[31].today);
        assert_eq!(grid.iter().filter(|c| c.selected).count(), 1);
        assert_eq!(grid[0].day, 0);
    }
}
