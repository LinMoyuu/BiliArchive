use serde::{Deserialize, Serialize};

/// 一次导出所对应的完整文档：元信息 + 统计 + 视频列表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveDoc {
    pub meta: Meta,
    pub stats: Stats,
    pub videos: Vec<Video>,
}

impl Default for ArchiveDoc {
    fn default() -> Self {
        Self {
            meta: Meta::new(),
            stats: Stats::default(),
            videos: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub app: String,
    pub version: String,
    pub format_version: u32,
    pub source: String,
    pub exported_at: String,
}

impl Meta {
    pub fn new() -> Self {
        Self {
            app: "BiliCommentHub".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            format_version: 2,
            source: "https://member.bilibili.com/platform/inter-active/comment".into(),
            exported_at: crate::model::now_text(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Stats {
    pub videos: usize,
    pub comments: usize,
    pub replies: usize,
    pub likes: u64,
    /// 其中已删除 / 仅自己可见的稿件数
    #[serde(default)]
    pub deleted_videos: usize,
    /// 带 IP 属地的评论 / 回复条数
    #[serde(default)]
    pub with_location: usize,
    /// 动态条数（kind = dynamic 的内容）
    #[serde(default)]
    pub dynamics: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Video {
    pub bvid: String,
    pub aid: u64,
    pub title: String,
    pub owner_mid: u64,
    pub owner_name: String,
    pub url: String,
    pub cover: String,
    /// 内容类型：video 视频 / dynamic 动态 / article 专栏。
    /// 老数据没有这个字段，按视频处理
    #[serde(default)]
    pub kind: String,
    /// B 站评论接口里的 type：1 视频 / 11 图文动态 / 17 转发动态 / 12 专栏
    #[serde(default)]
    pub comment_type: u32,
    /// 动态的正文（纯文本，保留换行）
    #[serde(default)]
    pub text: String,
    /// 动态的配图（https 绝对地址）
    #[serde(default)]
    pub text_pics: Vec<String>,
    /// 内容本身发布时的 IP 属地（动态有，视频多半为空）
    #[serde(default)]
    pub ip_location: String,
    /// 内容本身的点赞 / 转发数（评论数在 comment_total 里）
    #[serde(default)]
    pub like_count: u64,
    #[serde(default)]
    pub forward_count: u64,
    /// 动态 id（opus 页地址里那一串），视频为 0
    #[serde(default)]
    pub dynamic_id: u64,
    /// 转发动态时，被转发那条的正文与作者
    #[serde(default)]
    pub orig_text: String,
    #[serde(default)]
    pub orig_author: String,
    #[serde(default)]
    pub publish_time: i64,
    #[serde(default)]
    pub publish_time_text: String,
    /// 稿件评论总数（官方计数）
    #[serde(default)]
    pub comment_total: u64,
    /// 稿件状态：public 公开 / private 仅自己可见 / rejected 未通过 / deleted 已删除
    #[serde(default)]
    pub status: String,
    /// 状态的中文说明
    #[serde(default)]
    pub status_text: String,
    /// 本次实际抓取到的评论
    #[serde(default)]
    pub comments: Vec<Comment>,
}

impl Video {
    pub fn reply_type(&self) -> u32 {
        if self.comment_type > 0 { return self.comment_type; }
        match self.kind.as_str() { "dynamic" => 11, "article" => 12, "audio" => 14, _ => 1 }
    }

    pub fn url_for(bvid: &str, aid: u64) -> String {
        if !bvid.is_empty() {
            format!("https://www.bilibili.com/video/{}", bvid)
        } else {
            format!("https://www.bilibili.com/video/av{}", aid)
        }
    }

    /// 稿件在公开侧是否还能访问
    pub fn is_hidden(&self) -> bool {
        matches!(self.status.as_str(), "deleted" | "private" | "rejected")
    }

    pub fn is_dynamic(&self) -> bool {
        self.kind == "dynamic"
    }

    pub fn is_article(&self) -> bool {
        self.kind == "article"
    }

    /// 动态没有标题，取正文首行当标题；视频照旧
    pub fn display_title(&self) -> String {
        if !self.title.is_empty() {
            return self.title.clone();
        }
        if self.is_dynamic() || self.is_article() || self.kind == "audio" {
            let line = self
                .text
                .lines()
                .map(|l| l.trim())
                .find(|l| !l.is_empty())
                .unwrap_or("");
            if !line.is_empty() {
                let chars: Vec<char> = line.chars().collect();
                return if chars.len() > 42 {
                    format!("{}…", chars[..42].iter().collect::<String>())
                } else {
                    line.to_string()
                };
            }
            let kind_name = if self.is_article() {
                "专栏"
            } else if self.kind == "audio" {
                "音频"
            } else {
                "动态"
            };
            return if self.dynamic_id > 0 {
                format!("{} {}", kind_name, self.dynamic_id)
            } else {
                format!("{} {}", kind_name, self.aid)
            };
        }
        if !self.bvid.is_empty() {
            self.bvid.clone()
        } else {
            format!("av{}", self.aid)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Comment {
    pub rpid: u64,
    pub mid: u64,
    pub uname: String,
    /// 评论者头像（https 绝对地址）
    #[serde(default)]
    pub avatar: String,
    #[serde(default)]
    pub level: u32,
    /// 是否大会员（昵称在网页上显示为粉色）
    #[serde(default)]
    pub is_vip: bool,
    /// IP 属地，例如「上海」「中国香港」；老评论可能没有
    #[serde(default)]
    pub location: String,
    /// 图片评论里的图片地址（https）
    #[serde(default)]
    pub pictures: Vec<String>,
    #[serde(default)]
    pub is_up: bool,
    #[serde(default)]
    pub is_pinned: bool,
    pub content: String,
    #[serde(default)]
    pub like: u64,
    #[serde(default)]
    pub ctime: i64,
    #[serde(default)]
    pub ctime_text: String,
    #[serde(default)]
    pub reply_count: u64,
    #[serde(default)]
    pub replies: Vec<Reply>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Reply {
    pub rpid: u64,
    pub mid: u64,
    pub uname: String,
    /// 回复者头像（https 绝对地址）
    #[serde(default)]
    pub avatar: String,
    #[serde(default)]
    pub level: u32,
    /// 是否大会员
    #[serde(default)]
    pub is_vip: bool,
    /// IP 属地
    #[serde(default)]
    pub location: String,
    /// 图片评论里的图片地址
    #[serde(default)]
    pub pictures: Vec<String>,
    #[serde(default)]
    pub is_up: bool,
    pub content: String,
    #[serde(default)]
    pub like: u64,
    #[serde(default)]
    pub ctime: i64,
    #[serde(default)]
    pub ctime_text: String,
    #[serde(default)]
    pub parent: u64,
}

impl Video {
    pub fn total_comments(&self) -> usize {
        self.comments.len()
    }
    pub fn total_replies(&self) -> usize {
        self.comments.iter().map(|c| c.replies.len()).sum()
    }
}

impl ArchiveDoc {
    /// 重算统计信息
    pub fn recount(&mut self) {
        let mut comments = 0usize;
        let mut replies = 0usize;
        let mut likes = 0u64;
        let mut with_location = 0usize;
        for v in &self.videos {
            comments += v.comments.len();
            likes += v.comments.iter().map(|c| c.like).sum::<u64>();
            with_location += v
                .comments
                .iter()
                .filter(|c| !c.location.is_empty())
                .count();
            for c in &v.comments {
                replies += c.replies.len();
                likes += c.replies.iter().map(|r| r.like).sum::<u64>();
                with_location += c
                    .replies
                    .iter()
                    .filter(|r| !r.location.is_empty())
                    .count();
            }
        }
        self.stats = Stats {
            videos: self.videos.len() - self.videos.iter().filter(|v| v.is_dynamic()).count(),
            dynamics: self.videos.iter().filter(|v| v.is_dynamic()).count(),
            comments,
            replies,
            likes,
            deleted_videos: self.videos.iter().filter(|v| v.is_hidden()).count(),
            with_location,
        };
    }

    /// 按 (类型, id) 合并：同一内容内按 rpid 去重，回复同理
    pub fn merge_video(&mut self, mut incoming: Video) {
        let norm = |v: &Video| {
            if v.kind.is_empty() {
                "video".to_string()
            } else {
                v.kind.clone()
            }
        };
        let incoming_kind = norm(&incoming);
        if let Some(existing) = self
            .videos
            .iter_mut()
            .find(|v| v.aid == incoming.aid && v.reply_type() == incoming.reply_type())
        {
            if existing.title.is_empty() {
                existing.title = incoming.title.clone();
            }
            if existing.kind.is_empty() {
                existing.kind = incoming_kind.clone();
            }
            if existing.text.is_empty() {
                existing.text = incoming.text.clone();
            }
            if existing.text_pics.is_empty() {
                existing.text_pics = incoming.text_pics.clone();
            }
            if existing.ip_location.is_empty() {
                existing.ip_location = incoming.ip_location.clone();
            }
            if incoming.like_count > existing.like_count {
                existing.like_count = incoming.like_count;
            }
            if incoming.forward_count > existing.forward_count {
                existing.forward_count = incoming.forward_count;
            }
            if existing.dynamic_id == 0 {
                existing.dynamic_id = incoming.dynamic_id;
            }
            if existing.orig_text.is_empty() {
                existing.orig_text = incoming.orig_text.clone();
            }
            if existing.orig_author.is_empty() {
                existing.orig_author = incoming.orig_author.clone();
            }
            if existing.bvid.is_empty() {
                existing.bvid = incoming.bvid.clone();
            }
            if existing.cover.is_empty() {
                existing.cover = incoming.cover.clone();
            }
            if existing.owner_mid == 0 { existing.owner_mid = incoming.owner_mid; }
            if existing.owner_name.is_empty() {
                existing.owner_name = incoming.owner_name.clone();
            }
            if !incoming.status.is_empty() && incoming.status != "unknown" {
                existing.status = incoming.status.clone();
                existing.status_text = incoming.status_text.clone();
            }
            if existing.publish_time == 0 && incoming.publish_time > 0 {
                existing.publish_time = incoming.publish_time;
                existing.publish_time_text = incoming.publish_time_text.clone();
            }
            if incoming.comment_total > existing.comment_total {
                existing.comment_total = incoming.comment_total;
            }
            for c in incoming.comments.drain(..) {
                match existing.comments.iter_mut().find(|e| e.rpid == c.rpid) {
                    Some(e) => {
                        if e.avatar.is_empty() {
                            e.avatar = c.avatar.clone();
                        }
                        if e.location.is_empty() {
                            e.location = c.location.clone();
                        }
                        if e.pictures.is_empty() {
                            e.pictures = c.pictures.clone();
                        }
                        if !e.is_vip {
                            e.is_vip = c.is_vip;
                        }
                        for r in c.replies {
                            match e.replies.iter_mut().find(|x| x.rpid == r.rpid) {
                                Some(x) => {
                                    if x.location.is_empty() {
                                        x.location = r.location.clone();
                                    }
                                    if x.pictures.is_empty() {
                                        x.pictures = r.pictures.clone();
                                    }
                                    if !x.is_vip {
                                        x.is_vip = r.is_vip;
                                    }
                                }
                                None => e.replies.push(r),
                            }
                        }
                        if c.is_pinned {
                            e.is_pinned = true;
                        }
                    }
                    None => existing.comments.push(c),
                }
            }
        } else {
            for c in incoming.comments.iter_mut() {
                let mut seen: Vec<u64> = Vec::new();
                c.replies.retain(|r| {
                    if seen.contains(&r.rpid) {
                        false
                    } else {
                        seen.push(r.rpid);
                        true
                    }
                });
                c.replies.sort_by_key(|r| r.ctime);
            }
            incoming.comments.sort_by(|a, b| b.ctime.cmp(&a.ctime));
            self.videos.push(incoming);
        }
        // A reply collected before its root may temporarily have been a top-level
        // orphan. Once a later page restores its root, retain it only under that root.
        for video in &mut self.videos {
            let replies: std::collections::HashSet<u64> = video.comments.iter()
                .flat_map(|c| c.replies.iter().map(|r| r.rpid)).collect();
            video.comments.retain(|c| !replies.contains(&c.rpid));
        }
        self.videos.sort_by(|a, b| a.aid.cmp(&b.aid));
        self.meta.exported_at = now_text();
        self.recount();
    }
}

pub fn now_text() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

pub fn ts_text(ts: i64) -> String {
    if ts <= 0 {
        return String::new();
    }
    match chrono::TimeZone::timestamp_opt(&chrono::Local, ts, 0) {
        chrono::LocalResult::Single(t) => t.format("%Y-%m-%d %H:%M:%S").to_string(),
        _ => String::new(),
    }
}

/// 人类可读的计数：12345 -> "1.2 万"
pub fn human_count(n: u64) -> String {
    if n >= 100_000_000 {
        format!("{:.1} 亿", n as f64 / 100_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1} 万", n as f64 / 10_000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod continuation_merge_tests {
    use super::*;
    #[test]
    fn later_root_replaces_earlier_orphan_without_duplicating_reply() {
        let mut doc=ArchiveDoc::default();
        doc.merge_video(Video{aid:42,owner_mid:123,comments:vec![Comment{rpid:2,content:"reply".into(),..Default::default()}],..Default::default()});
        doc.merge_video(Video{aid:42,owner_mid:123,comments:vec![Comment{rpid:1,replies:vec![Reply{rpid:2,content:"reply".into(),..Default::default()}],..Default::default()}],..Default::default()});
        assert_eq!(doc.videos[0].comments.len(),1);
        assert_eq!(doc.videos[0].comments[0].rpid,1);
        assert_eq!(doc.videos[0].comments[0].replies.len(),1);
        assert_eq!(doc.stats.comments,1);assert_eq!(doc.stats.replies,1);
    }
}
