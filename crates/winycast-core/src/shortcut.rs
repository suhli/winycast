use crate::{Entry, Target};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i64)]
pub enum ShortcutKind {
    Snippet = 0,
    Quicklink = 1,
}

impl ShortcutKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Snippet => "常用文本",
            Self::Quicklink => "快捷链接",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShortcutDraft {
    pub kind: ShortcutKind,
    pub title: String,
    pub body: String,
}

impl ShortcutDraft {
    pub fn new(kind: ShortcutKind, body: String) -> Self {
        let title = if kind == ShortcutKind::Quicklink {
            web_address(&body)
                .and_then(|url| url.host_str().map(str::to_owned))
                .unwrap_or_default()
        } else {
            String::new()
        };
        Self { kind, title, body }
    }

    pub fn validated(mut self) -> Result<Self, &'static str> {
        self.title = self.title.trim().to_owned();
        if self.title.is_empty() || self.title.len() > 256 || self.title.contains(char::is_control)
        {
            return Err("请输入有效名称（最多 256 字节）");
        }
        if self.body.trim().is_empty() || self.body.len() > 8192 || self.body.contains('\0') {
            return Err("请输入有效内容（最多 8 KiB）");
        }
        if self.kind == ShortcutKind::Quicklink {
            let parsed = web_address(&self.body).ok_or("请输入有效的 HTTP 或 HTTPS 网址")?;
            self.body = parsed.into();
            if self.body.len() > 8192 {
                return Err("网址过长（转义后最多 8 KiB）");
            }
        }
        Ok(self)
    }
}

pub fn web_address(value: &str) -> Option<url::Url> {
    let value = value.trim();
    let scheme = value.split_once(':')?.0;
    if value.len() > 8192
        || !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        || value.contains(char::is_control)
    {
        return None;
    }
    url::Url::parse(value).ok().filter(url::Url::has_host)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Shortcut {
    pub id: i64,
    pub draft: ShortcutDraft,
}

impl Shortcut {
    pub fn into_entry(self) -> Entry {
        let aliases = match self.draft.kind {
            ShortcutKind::Snippet => "snippet 常用文本",
            ShortcutKind::Quicklink => "quicklink 快捷链接",
        };
        Entry::new(
            shortcut_entry_id(self.id),
            self.draft.title,
            self.draft.body,
            aliases,
            Target::Shortcut(self.id, self.draft.kind),
        )
    }
}

pub fn shortcut_entry_id(id: i64) -> String {
    format!("shortcut:{id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_web_links_and_preserves_multiline_snippets() {
        let draft = |kind, body: &str| ShortcutDraft {
            kind,
            title: " 速记 ".into(),
            body: body.into(),
        };
        let snippet = draft(ShortcutKind::Snippet, " 第一行\n\t第二行\n")
            .validated()
            .unwrap();
        assert_eq!(snippet.title, "速记");
        assert_eq!(snippet.body, " 第一行\n\t第二行\n");

        let link = draft(ShortcutKind::Quicklink, " HTTPS://Example.com/文档 ")
            .validated()
            .unwrap();
        assert_eq!(link.body, "https://example.com/%E6%96%87%E6%A1%A3");
        for body in [
            "file:///C:/test.exe",
            "ms-settings:",
            "example.com",
            "http://",
            "https://exa\nmple.com",
        ] {
            assert!(draft(ShortcutKind::Quicklink, body).validated().is_err());
        }
    }
}
