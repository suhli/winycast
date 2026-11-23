use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};

use pinyin::ToPinyin;

use crate::{Row, Target};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub target: Target,
    keys: Vec<String>,
}

impl Entry {
    pub fn display(id: String, title: String, detail: String, target: Target) -> Self {
        Self {
            id,
            title,
            detail,
            target,
            keys: Vec::new(),
        }
    }

    pub fn new(
        id: String,
        title: impl Into<String>,
        detail: impl Into<String>,
        aliases: &str,
        target: Target,
    ) -> Self {
        let title = title.into();
        let mut keys = vec![title.to_lowercase()];
        for value in [&title, aliases] {
            let mut full = String::new();
            let mut initials = String::new();
            let mut translated = false;
            for ch in value.chars() {
                if let Some(syllable) = ch.to_pinyin() {
                    let text = syllable.plain();
                    full.push_str(text);
                    if let Some(initial) = text.chars().next() {
                        initials.push(initial);
                    }
                    translated = true;
                } else if ch.is_alphanumeric() {
                    full.extend(ch.to_lowercase());
                    initials.extend(ch.to_lowercase());
                }
            }
            if translated {
                keys.extend([full, initials]);
            }
        }
        keys.extend(aliases.split_whitespace().map(str::to_lowercase));
        if !aliases.is_empty() {
            keys.push(aliases.to_lowercase());
        }
        Self {
            id,
            title,
            detail: detail.into(),
            target,
            keys,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Usage {
    pub count: u32,
    pub pinned: bool,
    pub last_used: i64,
}

impl Usage {
    pub fn weight(self, now: i64) -> i32 {
        let age_days = now.saturating_sub(self.last_used).max(0) / 86400;
        let recent = if self.last_used > 0 && self.count > 0 {
            [25, 15, 10, 5]
                .get((age_days / 7) as usize)
                .copied()
                .unwrap_or(0)
        } else {
            0
        };
        (self.count.min(1000) as f64).ln_1p().mul_add(10., 0.) as i32 + recent
    }
}

pub fn unix_time() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

fn match_score(key: &str, token: &str) -> Option<i32> {
    if key == token {
        return Some(800);
    }
    if key.starts_with(token) {
        return Some(660);
    }
    if let Some(start) = key.find(token) {
        return Some(510 - start.min(100) as i32);
    }
    let mut chars = token.chars();
    let mut wanted = chars.next()?;
    let mut score = 200;
    let mut previous = None;
    for (i, actual) in key.chars().enumerate() {
        if wanted == actual {
            score += if previous == i.checked_sub(1) && previous.is_some() {
                18
            } else {
                3
            };
            score -= previous.map_or(i.min(180), |prior| (i - prior - 1).min(180)) as i32;
            previous = Some(i);
            if let Some(next) = chars.next() {
                wanted = next;
            } else {
                return Some(score.max(1));
            }
        }
    }
    None
}

pub fn rank<'a>(
    entries: impl IntoIterator<Item = &'a Entry>,
    query: &str,
    usage: &HashMap<String, Usage>,
    cancelled: impl Fn() -> bool,
) -> Vec<Row> {
    let query = query.to_lowercase();
    let tokens: Vec<_> = query.split_whitespace().collect();
    let now = unix_time();
    let mut ranked = Vec::new();
    for entry in entries {
        if cancelled() {
            return Vec::new();
        }
        let relevance = tokens.iter().try_fold(0, |sum, token| {
            entry
                .keys
                .iter()
                .enumerate()
                .filter_map(|(index, key)| {
                    match_score(key, token).map(|score| score - i32::from(index != 0) * 22)
                })
                .max()
                .map(|score| sum + score)
        });
        let Some(relevance) = relevance else {
            continue;
        };
        let use_data = usage.get(&entry.id).copied().unwrap_or_default();
        let bonus = i32::from(use_data.pinned) * 130 + use_data.weight(now);
        let default_bias =
            i32::from(tokens.is_empty() && matches!(entry.target, Target::Command(_))) * 100;
        ranked.push((relevance + bonus + default_bias, use_data.pinned, entry));
    }
    ranked.sort_unstable_by(|(score_a, _, a), (score_b, _, b)| {
        score_b
            .cmp(score_a)
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.id.cmp(&b.id))
    });
    ranked
        .into_iter()
        .take(500)
        .map(|(_, pinned, entry)| Row {
            entry: entry.clone(),
            pinned,
            suggested: false,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, title: &str) -> Entry {
        Entry::new(id.into(), title, "", "", Target::App(id.into()))
    }

    #[test]
    fn matches_pinyin_initials_and_fuzzy_letters() {
        let apps = [app("calc", "计算器"), app("note", "Notepad")];
        for (query, id) in [
            ("jisuanqi", "calc"),
            ("ji suan", "calc"),
            ("jsq", "calc"),
            ("计算", "calc"),
            ("ntpd", "note"),
        ] {
            let result = rank(&apps, query, &HashMap::new(), || false);
            assert_eq!(result[0].entry.id, id);
        }
    }

    #[test]
    fn ranking_uses_stars_and_frequency_and_checks_cancellation() {
        let apps = [app("a", "Tool"), app("b", "Tool"), app("c", "Tool")];
        let usage = HashMap::from([
            (
                "b".into(),
                Usage {
                    count: 10,
                    pinned: false,
                    ..Usage::default()
                },
            ),
            (
                "c".into(),
                Usage {
                    count: 0,
                    pinned: true,
                    ..Usage::default()
                },
            ),
        ]);
        let result = rank(&apps, "tool", &usage, || false);
        assert_eq!(
            result
                .iter()
                .map(|row| row.entry.id.as_str())
                .collect::<Vec<_>>(),
            ["c", "b", "a"]
        );
        assert!(rank(&apps, "", &usage, || true).is_empty());
    }
}
