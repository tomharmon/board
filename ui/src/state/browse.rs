use super::*;
use anyhow::ensure;
use board_local::{LabelTerm, automation::Schedule};

const REPLIES: usize = 40;

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
    pub schedules: Vec<Schedule>,
    pub cancel: Option<i64>,
}
impl Default for Browse {
    fn default() -> Self {
        Self {
            search_open: false,
            adding_channel: false,
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
            schedules: Vec::new(),
            cancel: None,
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
        self.schedules.clear();
        self.cancel = None;
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
        self.schedules = latest.schedules;
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
    pub fn reset_feed(&mut self) {
        self.starts.clear();
        self.feed_size = 50;
    }
    pub fn add_channel(&mut self) {
        let name = self
            .browse
            .channel_name
            .text
            .trim()
            .trim_start_matches('#')
            .to_owned();
        match self
            .db()
            .and_then(|db| db.create_channel_as(&name, &self.actor))
        {
            Ok(_) => {
                self.browse.adding_channel = false;
                self.select(Some(name));
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }
    pub fn cancel_pending(&mut self, id: i64) {
        if self.browse.cancel != Some(id) || !self.browse.schedules.iter().any(|s| s.id == id) {
            return;
        }
        let result = self
            .db()
            .and_then(|db| automation::cancel_schedule(db, id, &self.actor));
        match result {
            Ok(_) => {
                self.browse.cancel = None;
                self.error = None;
                self.refresh();
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
