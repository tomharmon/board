use super::*;
use axum::response::{
    Sse,
    sse::{Event, KeepAlive},
};
use board_local::Filter;
use futures_util::stream::unfold;
use std::{collections::VecDeque, convert::Infallible, time::Duration};

pub(super) fn stream(
    app: App,
    filter: Filter,
    kind: Option<String>,
    all: bool,
    after: i64,
    history: VecDeque<board_local::Event>,
) -> Response {
    let identity = IDENTITY.with(Clone::clone);
    let stream = unfold(Some((after, history)), move |state| {
        let app = app.clone();
        let identity = identity.clone();
        let filter = filter.clone();
        let kind = kind.clone();
        async move {
            let (mut cursor, mut history) = state?;
            loop {
                let f = filter.clone();
                let k = kind.clone();
                let backlog = history.pop_front();
                // The response body is polled after the HTTP middleware returns. Rebind and recheck the credential.
                let page = IDENTITY
                    .scope(
                        identity.clone(),
                        app.call(move |b, _| {
                            if let Some(event) = backlog {
                                return Ok((cursor, b.recheck_event(event)?, true));
                            }
                            let (pos, rows) = b.event_feed(cursor, 1, &f, k.as_deref(), all)?;
                            Ok((pos, rows.into_iter().next(), false))
                        }),
                    )
                    .await;
                match page {
                    Err(e) => {
                        return Some((
                            Ok::<_, Infallible>(Event::default().event("error").data(e.code)),
                            None,
                        ));
                    }
                    Ok((pos, event, is_history)) => {
                        if !is_history {
                            cursor = pos;
                        }
                        if let Some(e) = event {
                            let msg = Event::default()
                                .id(e.cursor.to_string())
                                .event(&e.kind)
                                .json_data(e)
                                .expect("serializable event");
                            return Some((Ok(msg), Some((cursor, history))));
                        }
                        if is_history {
                            continue;
                        }
                        tokio::time::sleep(Duration::from_millis(120)).await;
                    }
                }
            }
        }
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(10)))
        .into_response()
}
