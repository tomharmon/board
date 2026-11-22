use super::*;
use workspace::field;

pub(super) fn button() -> El {
    tool("filter-messages", "Filter messages", &icons::LIST_FILTER).on_press_tx(State::show_filters)
}

pub(super) fn summary(s: &State) -> El {
    let f = &s.browse.filter;
    let mut terms = Vec::new();
    if s.tasks_only {
        terms.push("tasks only".into());
    }
    if let Some(origin) = &f.from {
        terms.push(format!("From {origin}"));
    }
    if let Some(by) = &f.by {
        terms.push(format!("By {by}"));
    }
    if let Some(who) = &f.mentions {
        terms.push(format!("Mentions {who}"));
    }
    if s.page == Page::Activity && !s.query.text.is_empty() {
        terms.push(format!("Text: {}", s.query.text));
    }
    mono(terms.join("   "), MUTED)
        .w_full()
        .wrap_anywhere()
        .max_lines(2)
}

pub(super) fn view(s: &State, inset: f32) -> El {
    let b = &s.browse;
    let mut content = col()
        .w_full()
        .px(inset)
        .py(18.0)
        .gap(14.0)
        .child(
            row()
                .w_full()
                .center_y()
                .gap(6.0)
                .child(label("Message filters", 14.0, 600.0, INK).grow(1.0))
                .child(
                    tool("close-filters", "Close filters", &glyphs::CLOSE)
                        .on_press(|s: &mut State| s.browse.filter_open = false),
                ),
        )
        .child(
            row()
                .w_full()
                .gap(5.0)
                .center_y()
                .child(action("tasks-only", "tasks only", s.tasks_only).on_press(
                    |s: &mut State| {
                        s.tasks_only = !s.tasks_only;
                        s.browse.filter_open = false;
                        s.reset_feed();
                        s.refresh();
                    },
                ))
                .child(
                    tool(
                        "search-toggle",
                        if s.is_timeline() {
                            "Search messages"
                        } else {
                            "Search threads"
                        },
                        &glyphs::SEARCH,
                    )
                    .on_press_tx(|s: &mut State, tx| {
                        s.browse.filter_open = false;
                        s.open_search(tx);
                    }),
                )
                .child(
                    tool("jump-message", "Go to message ID", &glyphs::CURSOR_TEXT).on_press_tx(
                        |s: &mut State, tx| {
                            s.browse.filter_open = false;
                            s.jump_field(tx);
                        },
                    ),
                )
                .child(
                    action("activity", "activity history", false)
                        .on_press(|s: &mut State| s.go(Page::Activity)),
                ),
        )
        .child(field(
            "filter-author",
            "Author",
            &b.author,
            |s| &mut s.browse.author,
            false,
        ))
        .child(field(
            "filter-from",
            "Origin (board or source name)",
            &b.from,
            |s| &mut s.browse.from,
            false,
        ))
        .child(field(
            "filter-mention",
            "Mentions",
            &b.mention,
            |s| &mut s.browse.mention,
            false,
        ))
        .child(field(
            "filter-text",
            "Text contains",
            &b.words,
            |s| &mut s.browse.words,
            false,
        ));
    content = content.child(
        row()
            .w_full()
            .center_y()
            .gap(8.0)
            .child(action("clear-filters", "Clear filters", false).on_press(State::clear_filters))
            .child(el().grow(1.0))
            .child(action("apply-filters", "Apply", true).on_press(State::apply_filters)),
    );
    scroll("filter-scroll", content)
}
