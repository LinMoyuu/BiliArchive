//! Display identity and stable time ordering for mixed content in the desktop list.
use crate::model::{self, Video};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone};
fn content_type(v: &Video) -> u32 {
    if v.comment_type > 0 {
        return v.comment_type;
    }
    match v.r#type.as_str() {
        "video" => 1,
        "dynamic" => 11,
        "article" => 12,
        "audio" => 14,
        _ if v.bvid.starts_with("BV") => 1,
        _ => 0,
    }
}
pub fn identity(v: &Video) -> String {
    match content_type(v) {
        1 => {
            if v.bvid.is_empty() {
                format!("视频 av{}", v.aid)
            } else {
                format!("视频 {}", v.bvid)
            }
        }
        11 | 17 => {
            let label = if content_type(v) == 17 {
                "转发动态"
            } else {
                "图文动态"
            };
            if v.dynamic_id > 0 {
                format!("{label} {}", v.dynamic_id)
            } else {
                format!("{label} OID {}", v.aid)
            }
        }
        12 => format!("专栏 cv{}", v.aid),
        14 => format!("音频 au{}", v.aid),
        ty => format!("未知类型 {ty} · ID {}", v.aid),
    }
}
fn timestamp(n: i64) -> Option<i64> {
    let n = if n >= 100_000_000_000 { n / 1000 } else { n };
    (n > 0).then_some(n)
}
fn text_time(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(n) = s.parse::<i64>() {
        return timestamp(n);
    }
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return timestamp(d.timestamp());
    }
    if let Ok(d) = DateTime::parse_from_rfc2822(s) {
        return timestamp(d.timestamp());
    }
    for format in ["%Y-%m-%d %H:%M:%S", "%Y/%m/%d %H:%M:%S"] {
        if let Ok(d) = NaiveDateTime::parse_from_str(s, format) {
            return Local
                .from_local_datetime(&d)
                .earliest()
                .and_then(|d| timestamp(d.timestamp()));
        }
    }
    for format in ["%Y-%m-%d", "%Y/%m/%d"] {
        if let Ok(d) = NaiveDate::parse_from_str(s, format) {
            return Local
                .from_local_datetime(&d.and_hms_opt(0, 0, 0)?)
                .earliest()
                .and_then(|d| timestamp(d.timestamp()));
        }
    }
    None
}
fn published(v: &Video) -> Option<i64> {
    timestamp(v.publish_time).or_else(|| text_time(&v.publish_time_text))
}
pub fn sort_time(v: &Video) -> Option<i64> {
    published(v).or_else(|| {
        v.comments
            .iter()
            .filter_map(|c| timestamp(c.ctime).or_else(|| text_time(&c.ctime_text)))
            .min()
    })
}
pub fn time_label(v: &Video) -> String {
    if let Some(t) = published(v) {
        model::ts_text(t)
    } else if let Some(t) = sort_time(v) {
        format!("最早评论 {}", model::ts_text(t))
    } else {
        "未知时间".into()
    }
}
pub fn sorted_indices(rows: &[(Video, bool)]) -> Vec<usize> {
    // Compute comment-derived timestamps once, rather than scanning comments in each comparison.
    let mut keyed: Vec<_> = rows
        .iter()
        .enumerate()
        .map(|(index, (v, _))| (index, sort_time(v)))
        .collect();
    keyed.sort_by(|a, b| match (a.1, b.1) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    });
    keyed.into_iter().map(|(index, _)| index).collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mixed_ids_never_call_non_videos_av() {
        for (ty, prefix) in [
            (1, "视频 av"),
            (11, "图文动态 OID"),
            (17, "转发动态 OID"),
            (12, "专栏 cv"),
            (14, "音频 au"),
            (99, "未知类型"),
        ] {
            let v = Video {
                aid: 42,
                comment_type: ty,
                r#type: "video".into(),
                ..Default::default()
            };
            assert!(identity(&v).starts_with(prefix));
        }
        let v = Video {
            aid: 42,
            r#type: "dynamic".into(),
            dynamic_id: 123,
            ..Default::default()
        };
        assert_eq!(identity(&v), "图文动态 123");
    }
    #[test]
    fn stable_sort_keeps_original_selection_indices() {
        let rows = vec![
            (
                Video {
                    publish_time: 1_700_000_000,
                    ..Default::default()
                },
                false,
            ),
            (
                Video {
                    publish_time: 1_700_000_001_000,
                    ..Default::default()
                },
                true,
            ),
            (
                Video {
                    publish_time_text: "2023-11-14T22:13:21Z".into(),
                    ..Default::default()
                },
                false,
            ),
            (Video::default(), true),
            (
                Video {
                    comments: vec![model::Comment {
                        ctime: 1_600_000_000,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                false,
            ),
        ];
        assert_eq!(sorted_indices(&rows), vec![1, 2, 0, 4, 3]);
        assert!(rows[sorted_indices(&rows)[0]].1);
        assert!(text_time("2024/02/29").is_some());
        assert!(text_time("2026-02-29").is_none());
    }
}
