use super::*;
use board_local::sources::Provider;

fn provider(p: &Provider) -> &'static str {
    match p {
        Provider::Local => "Local",
        Provider::Github => "GitHub",
        Provider::Slack => "Slack",
        Provider::Linear => "Linear",
    }
}

pub(super) fn view(s: &State) -> El {
    let source = &s.desk.source;
    if s.page == Page::Sources {
        let mut rows = col().w_full().gap(10.0).child(heading("Sources", None));
        if source.list.is_empty() {
            rows = rows.child(empty("No visible sources"));
        }
        for item in &source.list {
            let key = item.name.clone();
            rows = rows.child(listrow(
                &format!("source-{}", item.name),
                &item.name,
                &format!(
                    "{}    #{}    {}",
                    provider(&item.provider),
                    item.channel,
                    item.owner
                ),
                move |s| {
                    s.desk.source.selected = key.clone();
                    s.desk.source.outgoing = false;
                    s.go(Page::Inbox);
                },
            ));
        }
        return rows;
    }
    let Some(config) = source.list.first() else {
        return empty("Source unavailable");
    };
    let mut panel = col()
        .w_full()
        .gap(12.0)
        .child(heading(&config.name, None))
        .child(
            mono(
                format!(
                    "{}    {}    #{}",
                    provider(&config.provider),
                    config.remote,
                    config.channel
                ),
                MUTED,
            )
            .w_full()
            .wrap_anywhere(),
        )
        .child(mono(
            if config.outbound {
                "Outbound enabled"
            } else {
                "Outbound disabled"
            },
            MUTED,
        ))
        .child(seg(
            &["Inbox", "Outbox"],
            usize::from(source.outgoing),
            "Delivery queue",
            "source-queue",
            |s, i| {
                s.desk.source.outgoing = i == 1;
                s.desk.source.expanded = None;
                s.desk.first_page();
                s.refresh();
            },
        ));
    if source.rows.is_empty() {
        panel = panel.child(empty("No deliveries on this page"));
    }
    for job in &source.rows {
        let id = job.id;
        panel = panel.child(listrow(
            &format!("job-{id}"),
            &format!("#{id}    {}", job.operation),
            &format!("{}    Attempts: {}", job.state, job.attempts),
            move |s| {
                s.desk.source.expanded = (s.desk.source.expanded != Some(id)).then_some(id);
                s.refresh();
            },
        ));
        if Some(id) == source.expanded {
            if job.state == "uncertain" {
                panel = panel.child(label("External outcome unknown", 14.0, 600.0, ACCENT));
            } else if job.error_recorded {
                panel = panel.child(label("Delivery error recorded", 14.0, 600.0, ACCENT));
            }
            if let Some(local) = job.local {
                panel = panel.child(
                    row()
                        .w_full()
                        .gap(8.0)
                        .center_y()
                        .child(
                            mono(format!("Linked copies: {}", source.copies), MUTED)
                                .grow(1.0)
                                .min_w(0.0)
                                .truncate(),
                        )
                        .child(
                            action(
                                &format!("job-message-{id}"),
                                &format!("Open #{local}"),
                                false,
                            )
                            .on_press(move |s: &mut State| s.open_message_thread(local)),
                        ),
                );
            }
        }
    }
    panel
}
