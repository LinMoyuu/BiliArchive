//! Expand the abbreviated users in a like notification through the detail API.
use super::*;

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub struct LikeUser {
    pub key: String,
    pub uid: String,
    pub nickname: String,
    pub avatar: String,
    pub like_time: u64,
    pub raw: Value,
}
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LikeStatus {
    #[default]
    Pending,
    Partial,
    Finished,
}
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub struct LikeDetail {
    pub status: LikeStatus,
    pub error: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct LikeCursor {
    pn: u64,
    last_mid: String,
    done: bool,
    // Per-task progress, independent of users retained from earlier collections.
    seen: BTreeSet<String>,
}
impl LikeCursor {
    pub(super) fn request_cursor(&self) -> Value {
        json!([self.pn, self.last_mid])
    }
    pub(super) fn is_done(&self) -> bool {
        self.done
    }
}
pub fn status(item: &Item) -> String {
    let state = match item.like_detail.status {
        LikeStatus::Pending => "待补采",
        LikeStatus::Partial => "未完成",
        LikeStatus::Finished => "接口已遍历",
    };
    let difference = if item.like_detail.status == LikeStatus::Finished
        && item.like_users.len() as u64 != item.like_count
    {
        "；明细数与通知计数不一致，平台可能未提供全部用户"
    } else {
        ""
    };
    format!(
        "通知报告 {} 个赞 / 已归档 {} 位用户 · {}{}{}",
        item.like_count,
        item.like_users.len(),
        state,
        difference,
        if item.like_detail.error.is_empty() {
            String::new()
        } else {
            format!(" · {}", item.like_detail.error)
        }
    )
}
pub fn users<'a>(item: &'a Item, q: &str, ascending: bool) -> Vec<&'a LikeUser> {
    let q = q.to_lowercase();
    let mut result: Vec<_> = item
        .like_users
        .iter()
        .filter(|u| {
            q.is_empty()
                || u.uid.to_lowercase().contains(&q)
                || u.nickname.to_lowercase().contains(&q)
        })
        .collect();
    result.sort_by(|a, b| match (a.like_time, b.like_time) {
        (0, 0) => std::cmp::Ordering::Equal,
        (0, _) => std::cmp::Ordering::Greater,
        (_, 0) => std::cmp::Ordering::Less,
        _ => {
            if ascending {
                a.like_time.cmp(&b.like_time)
            } else {
                b.like_time.cmp(&a.like_time)
            }
        }
    });
    result
}
pub(super) fn merge_users(target: &mut Vec<LikeUser>, incoming: Vec<LikeUser>) {
    let mut positions: BTreeMap<_, _> = target
        .iter()
        .enumerate()
        .map(|(n, u)| (u.key.clone(), n))
        .collect();
    for u in incoming {
        if let Some(&n) = positions.get(&u.key) {
            if u.like_time >= target[n].like_time {
                target[n] = u
            }
        } else {
            positions.insert(u.key.clone(), target.len());
            target.push(u)
        }
    }
}
fn user(row: &Value) -> Result<LikeUser> {
    if !row["user"].is_object() {
        bail!("点赞明细缺少用户对象")
    }
    let uid = id(&row["user"]["mid"]);
    let time = num(&row["like_time"]);
    let mut raw = row.clone();
    lossless(&mut raw);
    let key = if valid_id(&uid) && uid != "0" {
        format!("uid-{}", uid)
    } else {
        format!("anon-{}", digest(&serde_json::to_string(&raw)?))
    };
    Ok(LikeUser {
        key,
        uid,
        nickname: first(&[&row["user"]["nickname"], &row["user"]["uname"]]),
        avatar: text(&row["user"]["avatar"]),
        like_time: time,
        raw,
    })
}
pub(super) fn collect(
    store: &Arc<Mutex<Store>>,
    options: &Options,
    task: &mut Task,
    cancel: &AtomicBool,
    request: &mut impl FnMut(&str) -> Result<Value>,
    ctx: &mut CrawlContext<'_>,
) -> Result<Vec<String>> {
    let cards: Vec<String> = store
        .lock()
        .unwrap()
        .doc
        .items
        .iter()
        .filter(|i| {
            i.ty == "likes" && (options.contains(i.timestamp) || task.likes.contains_key(&i.id))
        })
        .map(|i| i.id.clone())
        .collect();
    if task.likes.keys().any(|key| !cards.contains(key)) {
        bail!("点赞断点对应的本地通知缺失，请结束旧任务后重新采集")
    }
    let mut errors = vec![];
    let total = cards.len();
    for (index, card_id) in cards.into_iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        if !ctx.paging_allows(&paging::Target::Like(card_id.clone())) {
            continue;
        }
        let mut cursor = task.likes.get(&card_id).cloned().unwrap_or_default();
        let saved_status = {
            let state = store.lock().unwrap();
            state
                .doc
                .items
                .iter()
                .find(|i| i.ty == "likes" && i.id == card_id)
                .unwrap()
                .like_detail
                .status
        };
        if cursor.done && saved_status == LikeStatus::Finished {
            continue;
        }
        // A changed summary count invalidates old paging offsets, while retaining archived users.
        if cursor.done || saved_status == LikeStatus::Pending {
            cursor = LikeCursor::default();
            task.likes.insert(card_id.clone(), cursor.clone());
        }
        let result = (|| -> Result<()> {
            while !cursor.done && !cancel.load(Ordering::SeqCst) {
                if !ctx.paging_begin(
                    paging::Position::Like(card_id.clone(), cursor.clone()),
                    options,
                )? {
                    break;
                }
                {
                    let mut state = store.lock().unwrap();
                    let item = state
                        .doc
                        .items
                        .iter_mut()
                        .find(|i| i.ty == "likes" && i.id == card_id)
                        .unwrap();
                    item.like_detail = LikeDetail {
                        status: LikeStatus::Partial,
                        error: String::new(),
                    };
                }
                let pn = cursor
                    .pn
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("点赞页码超出范围"))?;
                let (saved, reported) = {
                    let state = store.lock().unwrap();
                    let item = state
                        .doc
                        .items
                        .iter()
                        .find(|i| i.ty == "likes" && i.id == card_id)
                        .unwrap();
                    (item.like_users.len(), item.like_count)
                };
                ctx.progress(
                    format!(
                        "点赞明细 {}/{} · 通知 {} · 第 {} 页 · 已归档 {}/{} 位",
                        index + 1,
                        total,
                        card_id,
                        pn,
                        saved,
                        reported
                    ),
                    &card_id,
                );
                let response=ctx.request(request,&format!("https://api.bilibili.com/x/msgfeed/like_detail?platform=web&build=0&mobi_app=web&card_id={}&pn={}&last_mid={}",urlencoding::encode(&card_id),pn,urlencoding::encode(&cursor.last_mid)))?;
                let d = data(&response)?;
                let rows = array(d, "items")?;
                let page: Vec<_> = rows.iter().map(user).collect::<Result<_>>()?;
                let new_keys: BTreeSet<_> = page
                    .iter()
                    .map(|u| u.key.clone())
                    .filter(|key| !cursor.seen.contains(key))
                    .collect();
                // Retain accessible records even when pagination metadata stops progressing.
                {
                    let mut state = store.lock().unwrap();
                    let item = state
                        .doc
                        .items
                        .iter_mut()
                        .find(|i| i.ty == "likes" && i.id == card_id)
                        .unwrap();
                    merge_users(&mut item.like_users, page);
                }
                let end_value = &d["page"]["is_end"];
                if !end_value.is_boolean() && !end_value.is_u64() {
                    checkpoint_store(store, task, "likes")?;
                    let reason = ctx.paging_failed(
                        "点赞明细缺少有效 page.is_end".into(),
                        json!({"reason":"invalid_paging"}),
                    )?;
                    store
                        .lock()
                        .unwrap()
                        .doc
                        .items
                        .iter_mut()
                        .find(|i| i.ty == "likes" && i.id == card_id)
                        .unwrap()
                        .like_detail
                        .error = reason;
                    checkpoint_store(store, task, "likes")?;
                    break;
                }
                let end = yes(end_value);
                let next_mid = rows
                    .last()
                    .map(|r| id(&r["user"]["mid"]))
                    .unwrap_or_default();
                if !end && (rows.is_empty() || next_mid.is_empty() || new_keys.is_empty()) {
                    let actual_saved = {
                        let state = store.lock().unwrap();
                        state
                            .doc
                            .items
                            .iter()
                            .find(|i| i.ty == "likes" && i.id == card_id)
                            .unwrap()
                            .like_users
                            .len()
                    };
                    let (code, reason) = if rows.is_empty() {
                        ("empty_page", "接口返回空用户列表")
                    } else if next_mid.is_empty() {
                        ("missing_cursor", "缺少下一页用户标识")
                    } else {
                        ("no_new_users", "本页未返回新用户")
                    };
                    let diagnostic = json!({"page":pn,"returned_users":rows.len(),"new_users":actual_saved.saturating_sub(saved),"unseen_page_users":new_keys.len(),"archived_users":actual_saved,"reported_likes":reported,"last_mid":next_mid,"is_end":end,"reason":code});
                    ctx.progress(
                        format!(
                            "点赞明细 {}/{} · 通知 {} · 第 {} 页 · 已归档 {}/{} 位",
                            index + 1,
                            total,
                            card_id,
                            pn,
                            actual_saved,
                            reported
                        ),
                        &card_id,
                    );
                    let detail = format!(
                        "{}（返回 {} 位，新增 {} 位），已保留当前页与用户",
                        reason,
                        rows.len(),
                        actual_saved.saturating_sub(saved)
                    );
                    let deferred = format!("分页异常，已延后 · 点赞通知 {} · {}", card_id, detail);
                    {
                        let mut state = store.lock().unwrap();
                        let item = state
                            .doc
                            .items
                            .iter_mut()
                            .find(|i| i.ty == "likes" && i.id == card_id)
                            .unwrap();
                        item.like_detail = LikeDetail {
                            status: LikeStatus::Partial,
                            error: deferred,
                        };
                    }
                    checkpoint_store(store, task, "likes")?;
                    let reason = ctx.paging_failed(detail, diagnostic)?;
                    store
                        .lock()
                        .unwrap()
                        .doc
                        .items
                        .iter_mut()
                        .find(|i| i.ty == "likes" && i.id == card_id)
                        .unwrap()
                        .like_detail
                        .error = reason;
                    checkpoint_store(store, task, "likes")?;
                    break;
                }
                cursor.pn = pn;
                cursor.last_mid = next_mid;
                cursor.done = end;
                cursor.seen.extend(new_keys);
                {
                    let mut state = store.lock().unwrap();
                    let item = state
                        .doc
                        .items
                        .iter_mut()
                        .find(|i| i.ty == "likes" && i.id == card_id)
                        .unwrap();
                    item.like_detail = LikeDetail {
                        status: if end {
                            LikeStatus::Finished
                        } else {
                            LikeStatus::Partial
                        },
                        error: String::new(),
                    };
                    state.doc.exported_at = crate::model::now_text();
                }
                task.likes.insert(card_id.clone(), cursor.clone());
                checkpoint_store(store, task, "likes")?;
                ctx.recovered()?;
                ctx.paging_accepted()?;
            }
            Ok(())
        })();
        if let Err(e) = result {
            if e.is::<PersistFailure>() {
                return Err(e);
            }
            let message = if cancel.load(Ordering::SeqCst) {
                "采集已中断，进度已保存".to_string()
            } else {
                format!("{:#}", e)
            };
            {
                let mut state = store.lock().unwrap();
                let item = state
                    .doc
                    .items
                    .iter_mut()
                    .find(|i| i.ty == "likes" && i.id == card_id)
                    .unwrap();
                item.like_detail = LikeDetail {
                    status: LikeStatus::Partial,
                    error: message.clone(),
                };
            }
            checkpoint_store(store, task, "likes")?;
            if e.to_string().contains("-101") {
                return Err(e);
            }
            if !cancel.load(Ordering::SeqCst) {
                if ctx.live {
                    return Err(e);
                }
                ctx.issue(format!("点赞通知 {}：{}", card_id, message))?;
                errors.push(format!("点赞通知 {}：{}", card_id, message))
            }
        }
    }
    Ok(errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state(root: &Path) -> Arc<Mutex<Store>> {
        Arc::new(Mutex::new(Store {
            root: base(root, 12),
            doc: Doc::new(12),
            ..Default::default()
        }))
    }
    fn feed() -> Value {
        json!({"code":0,"data":{"total":{"items":[{"id":7,"counts":4,"like_time":100,"users":[{"mid":2,"nickname":"preview"}],"item":{"title":"liked"}}],"cursor":{"is_end":true}}}})
    }
    fn first() -> Value {
        json!({"code":0,"data":{"page":{"is_end":false},"items":[{"user":{"mid":18446744073709551614u64,"nickname":"full user"},"like_time":90},{"user":{"mid":0,"nickname":"anonymous A"},"like_time":80}]}})
    }
    fn last() -> Value {
        json!({"code":0,"data":{"page":{"is_end":true},"items":[{"user":{"mid":3,"nickname":"other"},"like_time":70},{"user":{"mid":0,"nickname":"anonymous B"},"like_time":60},{"user":{"mid":18446744073709551614u64,"nickname":"older duplicate"},"like_time":50}]}})
    }
    #[test]
    fn expands_and_resumes_at_the_next_detail_page() {
        let temp = tempfile::tempdir().unwrap();
        let store = state(temp.path());
        let path = temp.path().join("checkpoint.json");
        let cancel = AtomicBool::new(false);
        let options = Options::parse(vec!["likes".into()], "", "").unwrap();
        let errors = crawl_at(
            &store,
            12,
            options.clone(),
            &cancel,
            path.clone(),
            |url| {
                if url.contains("like_detail") {
                    assert!(url.contains("pn=1"));
                    cancel.store(true, Ordering::SeqCst);
                    Ok(first())
                } else {
                    Ok(feed())
                }
            },
            |_, _| Ok(()),
            |_| {},
        )
        .unwrap();
        assert!(errors.is_empty());
        assert!(path.exists());
        assert_eq!(store.lock().unwrap().doc.items[0].like_users.len(), 2);
        cancel.store(false, Ordering::SeqCst);
        let errors = crawl_at(
            &store,
            12,
            options.clone(),
            &cancel,
            path.clone(),
            |url| {
                assert!(
                    url.contains("like_detail")
                        && url.contains("pn=2")
                        && url.contains("last_mid=0")
                );
                Ok(last())
            },
            |_, _| Ok(()),
            |_| {},
        )
        .unwrap();
        assert!(errors.is_empty());
        assert!(!path.exists());
        {
            let state = store.lock().unwrap();
            let item = &state.doc.items[0];
            assert_eq!(item.like_users.len(), 4);
            assert_eq!(item.like_detail.status, LikeStatus::Finished);
            assert_eq!(item.like_users[0].uid, "18446744073709551614");
            assert_eq!(state.doc.query("likes", "", "full user", false).len(), 1);
            assert_eq!(users(item, "18446744073709551614", false).len(), 1);
        }
        // A fresh task walks again, even if all first-page users were archived before.
        let errors = crawl_at(
            &store,
            12,
            options,
            &cancel,
            path,
            |url| {
                if !url.contains("like_detail") {
                    Ok(feed())
                } else if url.contains("pn=1") {
                    Ok(first())
                } else {
                    Ok(last())
                }
            },
            |_, _| Ok(()),
            |_| {},
        )
        .unwrap();
        assert!(errors.is_empty());
        assert_eq!(store.lock().unwrap().doc.items[0].like_users.len(), 4);
    }
    #[test]
    fn repeated_page_remains_partial_and_retry_keeps_existing_users() {
        let temp = tempfile::tempdir().unwrap();
        let store = state(temp.path());
        let path = temp.path().join("checkpoint.json");
        let cancel = AtomicBool::new(false);
        let options = Options::parse(vec!["likes".into()], "", "").unwrap();
        let errors = crawl_at(
            &store,
            12,
            options.clone(),
            &cancel,
            path.clone(),
            |url| {
                if url.contains("like_detail") {
                    Ok(first())
                } else {
                    Ok(feed())
                }
            },
            |_, _| Ok(()),
            |_| {},
        )
        .unwrap();
        assert_eq!(errors.len(), 1);
        assert!(path.exists());
        assert_eq!(
            store.lock().unwrap().doc.items[0].like_detail.status,
            LikeStatus::Partial
        );
        let mut emit = |_| {};
        let mut sleep = |_| {};
        let mut ctx = CrawlContext::new(&cancel, &mut emit, &mut sleep);
        ctx.manual_paging = true;
        let errors = crawl_run(
            &store,
            12,
            options,
            &cancel,
            path.clone(),
            |url| {
                assert!(url.contains("pn=2"));
                Ok(json!({"code":0,"data":{"page":{"is_end":true},"items":[]}}))
            },
            |_, _| Ok(()),
            &mut ctx,
        )
        .unwrap();
        assert!(errors.is_empty());
        assert!(!path.exists());
        let state = store.lock().unwrap();
        assert_eq!(state.doc.items[0].like_users.len(), 2);
        assert!(status(&state.doc.items[0]).contains("不一致"));
    }
    #[test]
    fn summary_refresh_and_old_archives_do_not_erase_details() {
        let mut doc = Doc::new(12);
        let mut item = feed_item(&feed()["data"]["total"]["items"][0], "likes").unwrap();
        item.like_users = vec![user(&first()["data"]["items"][0]).unwrap()];
        item.like_detail.status = LikeStatus::Finished;
        doc.items.push(item);
        let mut incoming = Doc::new(12);
        incoming
            .items
            .push(feed_item(&feed()["data"]["total"]["items"][0], "likes").unwrap());
        doc.merge(incoming).unwrap();
        assert_eq!(doc.items[0].like_users.len(), 1);
        assert_eq!(doc.items[0].like_detail.status, LikeStatus::Finished);
        let mut value = serde_json::to_value(&doc).unwrap();
        for field in ["like_users", "like_detail", "like_count"] {
            value["items"][0].as_object_mut().unwrap().remove(field);
        }
        let old: Doc = serde_json::from_value(value).unwrap();
        let mut restored = Doc::new(12);
        restored.merge(old).unwrap();
        assert_eq!(restored.items[0].like_count, 4);
        assert_eq!(restored.items[0].like_detail.status, LikeStatus::Pending);
    }
}

// List responses stay small even when a notification has thousands of archived users.
pub fn item_summary(i: &Item) -> Value {
    json!({"id":i.id,"type":i.ty,"category":item_type(i),"session":i.session,"sender_uid":i.sender_uid,"sender_name":i.sender_name,"text":i.text,"timestamp":i.timestamp,"url":i.url,"status":i.status,"attachments":i.attachments,"raw":i.raw,"like_count":i.like_count,"like_users_count":i.like_users.len(),"like_detail":i.like_detail})
}
