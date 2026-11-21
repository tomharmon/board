use super::*;
use anyhow::ensure;
use board_local::LabelTerm;

const REPLIES: usize = 40;

#[derive(Default)]
pub struct ReadingEdge {
    latest: Option<i64>,
    seen: Option<i64>,
    scrolled_back: bool,
}

impl ReadingEdge {
    pub fn observe(&mut self, latest: Option<i64>) {
        if latest.is_none() || self.latest.is_none() {
            self.scrolled_back = false;
        }
        self.latest = latest;
        if !self.scrolled_back {
            self.seen = latest;
        }
    }
    pub fn show_jump(&self) -> bool {
        self.scrolled_back && self.latest > self.seen
    }
    pub fn visible(&mut self, visible: bool) {
        self.scrolled_back = !visible;
        if visible {
            self.seen = self.latest;
        }
    }
}

#[derive(Default, Clone, Copy, Eq, PartialEq)]
pub enum Position {
    #[default]
    Latest,
    Before(i64),
    After(i64),
}

pub struct Browse {
    pub search_open: bool,
    pub adding_channel: bool,
    pub private_channel: bool,
    pub channel_name: TextEdit,
    pub filter: Filter,
    pub filter_open: bool,
    pub author: TextEdit,
    pub from: TextEdit,
    pub words: TextEdit,
    pub mention: TextEdit,
    pub labels: TextEdit,
    pub jump_open: bool,
    pub jump: TextEdit,
    pub position: Position,
    pub focused: Option<i64>,
    pub reveal: Option<i64>,
    pub older: bool,
    pub newer: bool,
    pub targets: Vec<board_local::sources::SyncTarget>,
    pub feed_edge: ReadingEdge,
    pub thread_edge: ReadingEdge,
}
impl Default for Browse {
    fn default() -> Self {
        Self {
            search_open: false,
            adding_channel: false,
            private_channel: false,
            channel_name: TextEdit::new(""),
            filter: Filter::default(),
            filter_open: false,
            author: TextEdit::new(""),
            from: TextEdit::new(""),
            words: TextEdit::new(""),
            mention: TextEdit::new(""),
            labels: TextEdit::new(""),
            jump_open: false,
            jump: TextEdit::new(""),
            position: Position::Latest,
            focused: None,
            reveal: None,
            older: false,
            newer: false,
            targets: Vec::new(),
            feed_edge: ReadingEdge::default(),
            thread_edge: ReadingEdge::default(),
        }
    }
}
impl Browse {
    pub fn reset_thread(&mut self) {
        self.position = Position::Latest;
        self.focused = None;
        self.reveal = None;
        self.older = false;
        self.newer = false;
        self.targets.clear();
        self.thread_edge = ReadingEdge::default();
    }

    pub fn read_thread(&mut self, db: &Board, root: i64) -> Result<Vec<Message>> {
        let mut latest = db.conversation_window(
            root,
            &Filter::default(),
            Some(if self.position == Position::Latest {
                REPLIES + 1
            } else {
                1
            }),
        )?;
        let head = latest.replies.last().map_or(root, |m| m.id);
        let filter = Filter {
            re: Some(root),
            ..Filter::default()
        };
        let mut replies = match self.position {
            Position::Latest => std::mem::take(&mut latest.replies),
            Position::Before(before) => {
                db.messages_window(&filter, Some(before), REPLIES + 1)?.rows
            }
            Position::After(after) => {
                db.messages_window(
                    &Filter {
                        after: Some(after),
                        ..filter
                    },
                    None,
                    REPLIES + 1,
                )?
                .rows
            }
        };
        replies.retain(|m| m.id != root);
        self.older = match self.position {
            Position::After(_) => true,
            _ if replies.len() > REPLIES => {
                replies.remove(0);
                true
            }
            _ => false,
        };
        replies.truncate(REPLIES);
        self.newer = replies.last().is_some_and(|m| m.id < head);
        Ok(std::iter::once(latest.message).chain(replies).collect())
    }
    pub fn has_filters(&self) -> bool {
        self.filter.by.is_some()
            || self.filter.from.is_some()
            || self.filter.mentions.is_some()
            || !self.filter.label_terms.is_empty()
    }
}
impl State {
    pub fn copy_destination(&mut self, root: i64, url: &str, tx: &mut Tx) {
        let result = self.db().and_then(|db| {
            ensure!(
                self.page.thread_id() == Some(root) && !db.get(root)?.deleted,
                "Destination is no longer visible"
            );
            ensure!(
                db.sync_targets(root)?
                    .targets
                    .iter()
                    .any(|t| t.url.as_deref() == Some(url)),
                "Destination is no longer visible"
            );
            Ok(())
        });
        match result {
            Ok(()) => tx.copy(url.to_owned()),
            Err(e) => {
                self.error = Some(e.to_string());
                self.refresh();
            }
        }
    }
    pub fn reset_feed(&mut self) {
        self.starts.clear();
        self.feed_size = 50;
        self.browse.feed_edge = ReadingEdge::default();
    }
    pub fn add_channel(&mut self) {
        let name = self
            .browse
            .channel_name
            .text
            .trim()
            .trim_start_matches('#')
            .to_owned();
        let result = self.db().and_then(|db| {
            if self.browse.private_channel {
                db.create_private_channel(&name, &self.actor).map(|_| ())
            } else {
                db.create_channel_as(&name, &self.actor).map(|_| ())
            }
        });
        match result {
            Ok(_) => {
                self.browse.adding_channel = false;
                self.browse.private_channel = false;
                self.select(Some(name));
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }
    pub fn show_filters(&mut self, tx: &mut Tx) {
        self.browse.filter_open = !self.browse.filter_open;
        if self.browse.filter_open {
            let b = &mut self.browse;
            b.words = TextEdit::new(&self.query.text);
            b.author = TextEdit::new(b.filter.by.as_deref().unwrap_or(""));
            b.from = TextEdit::new(b.filter.from.as_deref().unwrap_or(""));
            b.mention = TextEdit::new(b.filter.mentions.as_deref().unwrap_or(""));
            b.labels = TextEdit::new(
                b.filter
                    .label_terms
                    .iter()
                    .map(|l| {
                        l.values
                            .as_ref()
                            .map_or_else(|| l.key.clone(), |v| format!("{}={}", l.key, v.join("|")))
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            tx.focus_when_shown("filter-author", true);
        }
    }

    pub fn apply_filters(&mut self) {
        let b = &mut self.browse;
        let terms = b
            .labels
            .text
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        match LabelTerm::parse(&terms) {
            Ok(labels) => {
                let opt = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_owned());
                b.filter.by = opt(&b.author.text);
                b.filter.from = opt(&b.from.text);
                b.filter.mentions = opt(&b.mention.text);
                b.filter.label_terms = labels;
                self.query = TextEdit::new(b.words.text.trim());
                b.filter_open = false;
                self.reset_feed();
                self.desk.first_page();
                self.error = None;
                self.refresh();
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    pub fn clear_filters(&mut self) {
        self.browse.filter = Filter::default();
        self.browse.filter_open = false;
        self.query.clear();
        self.tasks_only = false;
        self.reset_feed();
        self.desk.first_page();
        self.error = None;
        self.refresh();
    }

    pub fn jump_field(&mut self, tx: &mut Tx) {
        self.browse.jump_open = true;
        self.browse.jump.clear();
        tx.focus_when_shown("jump-id", true);
    }

    pub fn jump_to_id(&mut self) {
        let result = (|| {
            let id = self
                .browse
                .jump
                .text
                .trim()
                .trim_start_matches('#')
                .parse::<i64>()
                .context("Enter a positive message ID")?;
            ensure!(id > 0, "Enter a positive message ID");
            Ok(id)
        })();
        match result {
            Ok(id) => self.open_message_thread(id),
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    pub fn page_replies(&mut self, position: Position, tx: &mut Tx) {
        self.browse.position = position;
        self.browse.focused = None;
        self.refresh();
        if position == Position::Latest {
            tx.scroll_to_bottom("conversation");
        } else if let Some(m) = self.thread.get(1) {
            tx.scroll_into_view(format!("message-{}", m.id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use board_local::{access::Rules, sources::Provider};
    use rune::{Env, Runtime, clipboard};

    #[test]
    fn copying_a_recorded_destination_requires_current_read_access_even_before_poll() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proof.db");
        let owner = Board::open(&path).unwrap();
        let root = owner
            .post("work", "owner", "Report", &Default::default())
            .unwrap();
        let url = "https://example.invalid/archive/42";
        owner
            .create_source("owner", "archive", Provider::Local, "audit-room", "work")
            .unwrap();
        owner
            .attach_source("owner", "archive", "t", "note", root.id, Some(url))
            .unwrap();
        let grant = owner
            .set_grant("owner", None, "reader", &Rules::parse("work=read").unwrap())
            .unwrap();
        let token = owner
            .issue_credential("owner", "reader", None, None)
            .unwrap();
        let mut db = Board::open(&path).unwrap();
        db.bind_token(&token.token).unwrap();
        let mut state = State::opened(Ok((db, Some("reader".to_owned()))));
        state.go(Page::Thread(root.id));
        let mut rt = Runtime::new(state, crate::view::view, (1200., 800.), Env::default());
        let clip = clipboard::memory();
        rt.use_clipboard(&clip);
        rt.frame(0.);
        let (x, y) = rt.find("copy-destination-0").unwrap();
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.04);
        assert_eq!(clip.text().unwrap().as_str(), url);
        clip.set_text("leave alone");
        owner
            .set_grant(
                "owner",
                Some(grant.id),
                "reader",
                &Rules::parse("other=read").unwrap(),
            )
            .unwrap();
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.08);
        assert_eq!(clip.text().unwrap().as_str(), "leave alone");
        assert!(rt.find("copy-destination-0").is_none());
    }
}
