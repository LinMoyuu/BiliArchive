use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const PAGE_SIZE: u64 = 10;
pub const TYPES: [u32; 5] = [1, 11, 17, 12, 14];
pub type Ranges = BTreeMap<u32, PageRange>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageRange {
    pub enabled: bool,
    pub from: u64,
    pub to: u64, // zero means no upper limit
}
impl Default for PageRange {
    fn default() -> Self {
        Self {
            enabled: true,
            from: 1,
            to: 0,
        }
    }
}
pub fn defaults() -> Ranges {
    TYPES
        .into_iter()
        .map(|ty| (ty, PageRange::default()))
        .collect()
}

pub fn validate(ranges: &Ranges, require_enabled: bool) -> Result<()> {
    if ranges.len() != TYPES.len() || TYPES.iter().any(|ty| !ranges.contains_key(ty)) {
        bail!("采集范围必须包含五种内容类型");
    }
    for (&ty, r) in ranges {
        if r.from == 0 || (r.to > 0 && r.to < r.from) {
            bail!(
                "{}起始页必须至少为 1，结束页不能小于起始页",
                crate::checkpoint::label(ty)
            );
        }
    }
    if require_enabled && !ranges.values().any(|r| r.enabled) {
        bail!("至少启用一种采集类型");
    }
    Ok(())
}
pub fn parse(enabled: bool, from: &str, to: &str) -> Result<PageRange> {
    let from = from.trim();
    let to = to.trim();
    let from = if from.is_empty() {
        1
    } else {
        from.parse::<u64>()
            .map_err(|_| anyhow::anyhow!("起始页必须为正整数"))?
    };
    let to = if to.is_empty() {
        0
    } else {
        let n = to
            .parse::<u64>()
            .map_err(|_| anyhow::anyhow!("结束页必须为正整数或留空"))?;
        if n == 0 {
            bail!("结束页必须为正整数或留空");
        }
        n
    };
    if from == 0 || (to > 0 && to < from) {
        bail!("起始页必须至少为 1，结束页不能小于起始页");
    }
    Ok(PageRange { enabled, from, to })
}

/// Read the new configuration or retain only the old video bounds and enable flags.
pub fn from_config(v: &serde_json::Value) -> Result<Ranges> {
    if let Some(value) = v.get("collection_ranges") {
        let ranges: Ranges = serde_json::from_value(value.clone())?;
        validate(&ranges, false)?;
        return Ok(ranges);
    }
    let mut ranges = defaults();
    let mask = v["scope_mask"].as_u64().unwrap_or(15);
    for (ty, bit) in [(1, 1), (11, 2), (17, 2), (12, 4), (14, 8)] {
        ranges.get_mut(&ty).unwrap().enabled = mask & bit != 0;
    }
    let video = ranges.get_mut(&1).unwrap();
    video.from = v["page_from"].as_u64().unwrap_or(1).max(1);
    video.to = v["page_to"].as_u64().unwrap_or(0);
    if video.to > 0 && video.to < video.from {
        video.to = 0;
    }
    Ok(ranges)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_video_bounds_do_not_limit_other_types() {
        let r =
            from_config(&serde_json::json!({"scope_mask":3,"page_from":7,"page_to":9})).unwrap();
        assert_eq!((r[&1].from, r[&1].to), (7, 9));
        for ty in [11, 17, 12, 14] {
            assert_eq!((r[&ty].from, r[&ty].to), (1, 0));
        }
        assert!(r[&11].enabled && r[&17].enabled);
        assert!(!r[&12].enabled);
    }
    #[test]
    fn validates_ranges_and_roundtrips_independent_settings() {
        for (from, to) in [("0", ""), ("-1", ""), ("3", "2"), ("1", "0"), ("x", "")] {
            assert!(parse(true, from, to).is_err());
        }
        assert_eq!(parse(true, "", "").unwrap(), PageRange::default());
        let mut r = defaults();
        for (index, ty) in TYPES.into_iter().enumerate() {
            r.insert(
                ty,
                PageRange {
                    enabled: true,
                    from: index as u64 + 2,
                    to: index as u64 + 4,
                },
            );
        }
        assert_eq!(
            from_config(&serde_json::json!({"collection_ranges":r})).unwrap(),
            r
        );
        for value in r.values_mut() {
            value.enabled = false;
        }
        assert!(validate(&r, true).is_err());
        assert!(validate(&r, false).is_ok());
    }
}
