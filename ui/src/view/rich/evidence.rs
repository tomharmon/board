use super::*;
use serde_json::Value;

pub(super) const CHUNK: usize = 40;
fn value<'a>(object: &'a Value, key: &str) -> &'a str {
    object.get(key).and_then(Value::as_str).unwrap_or("")
}
fn verdict(status: &str) -> El {
    let (title, bg, fg) = match status {
        "pass" => ("PASS", ADD_BG, ADD_INK),
        "fail" => ("FAIL", DEL_BG, DEL_INK),
        _ => ("NOT RUN", TINT, MUTED),
    };
    label(title, 12., 650., fg).px(6.).py(2.).fill(c(bg))
}
fn literal(v: &str) -> El {
    mono(v, INK).w_full().wrap_anywhere().selectable(true)
}
pub(super) fn card(prefix: &str, id: i64, idx: usize, d: &Display) -> Option<El> {
    let b = &d.block;
    let data = &b.data;
    let base = format!("{prefix}evidence-{id}-{idx}");
    let mut out = col().key(&base).w_full().px(9.).py(4.).gap(5.);
    match (b.schema.as_str(), b.version) {
        ("verdict", 1) => {
            let mut header = row()
                .w_full()
                .center_y()
                .gap(7.)
                .flex_wrap()
                .child(verdict(value(data, "status")));
            let artifact = value(data, "artifact");
            if !artifact.is_empty() {
                header = header.child(
                    mono(artifact, MUTED)
                        .min_w(0.)
                        .wrap_anywhere()
                        .selectable(true),
                );
            }
            out = out.child(header).child(literal(value(data, "summary")));
            for (n, check) in data["checks"].as_array().into_iter().flatten().enumerate() {
                let expanded = d.checks.contains(&n);
                let name = value(check, "name");
                out = out.child(
                    click(
                        row()
                            .key(format!("{base}-check-{n}"))
                            .w_full()
                            .h(28.)
                            .center_y()
                            .gap(6.),
                        name,
                    )
                    .expanded(expanded)
                    .child(
                        icon(
                            if expanded {
                                &glyphs::CHEVRON_DOWN
                            } else {
                                &icons::NEXT_FRAME
                            },
                            14.,
                        )
                        .color(c(MUTED)),
                    )
                    .child(label(name, 13., 500., DIM).grow(1.).min_w(0.).truncate())
                    .child(verdict(value(check, "status")))
                    .on_press(move |s: &mut State| {
                        if !s.expanded_checks.remove(&(id, idx, n)) {
                            s.expanded_checks.insert((id, idx, n));
                        }
                    }),
                );
                if expanded {
                    for (k, title) in [("command", "Command"), ("output", "Output")] {
                        let v = value(check, k);
                        if !v.is_empty() {
                            out = out.child(mono(title, MUTED)).child(literal(v));
                        }
                    }
                }
            }
        }
        ("diff", 1) => {
            let (base_rev, head) = (value(data, "base"), value(data, "head"));
            if !base_rev.is_empty() || !head.is_empty() {
                out = out.child(
                    label(format!("{base_rev}..{head}"), 13., 600., INK)
                        .w_full()
                        .wrap_anywhere()
                        .selectable(true),
                );
            }
            if let Some(files) = data["files"].as_array() {
                let names = files
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                if !names.is_empty() {
                    out = out.child(mono(names, MUTED).w_full().wrap_anywhere().selectable(true));
                }
            }
            let mut lines = value(data, "patch").lines().peekable();
            let mut patch = col().w_full();
            for (line, v) in lines.by_ref().take(d.lines).enumerate() {
                let (bg, ink) = if v.starts_with('+') {
                    (ADD_BG, ADD_INK)
                } else if v.starts_with('-') {
                    (DEL_BG, DEL_INK)
                } else {
                    (PANEL, DIM)
                };
                patch = patch.child(
                    mono(if v.is_empty() { " " } else { v }, ink)
                        .key(format!("{base}-line-{line}"))
                        .w_full()
                        .px(4.)
                        .fill(c(bg))
                        .wrap_anywhere()
                        .selectable(true),
                );
            }
            out = out.child(patch);
            if lines.peek().is_some() {
                let count = d.lines;
                out = out.child(
                    action(&format!("{base}-more"), "More diff lines", false).on_press(
                        move |s: &mut State| {
                            s.patch_lines.insert((id, idx), count + CHUNK);
                        },
                    ),
                );
            }
        }
        _ => return None,
    }
    Some(out)
}
