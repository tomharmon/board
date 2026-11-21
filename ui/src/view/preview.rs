use super::*;
use crate::state::{AttachmentPreview, Preview};
use rune::SurfaceFit;

fn transport(c: &crate::state::Clip) -> El {
    let action = |key: &str, name: &str, glyph: &'static Icon, dir: isize| {
        tool(key, name, glyph).on_press_tx(move |s: &mut State, tx| {
            s.video_control(tx, |c| if dir == 0 { c.play() } else { c.step(dir) })
        })
    };
    let progress = c.current as f32 / c.total.saturating_sub(1).max(1) as f32;
    let bar = row()
        .key("clip-seek")
        .role("slider")
        .label("Video playhead")
        .help("Seek video; Left/Right 5 seconds, Home/End")
        .focusable(true)
        .w_full()
        .h(22.)
        .center_y()
        .child(el().grow(progress).h(3.).fill(super::c(INK)))
        .child(el().size(5., 15.).fill(super::c(INK)))
        .child(el().grow(1. - progress).h(3.).fill(super::c(EDGE)))
        .on_drag(|s: &mut State, d, tx| {
            if d.phase == rune::Phase::Up && !d.cancelled() && d.w > 0. {
                let fraction = (d.x / d.w).clamp(0., 1.);
                s.video_control(tx, |c| {
                    c.seek((c.total.saturating_sub(1) as f32 * fraction).round() as usize)
                });
            }
        })
        .on_key(|s: &mut State, k, tx| {
            if !matches!(k.key, Key::Left | Key::Right | Key::Home | Key::End) {
                return false;
            }
            s.video_control(tx, |c| match k.key {
                Key::Left => c.step(-crate::state::Clip::FIVE_SECONDS),
                Key::Right => c.step(crate::state::Clip::FIVE_SECONDS),
                Key::Home => c.seek(0),
                Key::End => c.seek(c.total.saturating_sub(1)),
                _ => (),
            });
            true
        });
    let mut out = col().w_full().gap(4.).child(bar).child(
        row()
            .w_full()
            .center_y()
            .gap(5.)
            .child(
                mono(c.time(), MUTED)
                    .key("clip-position")
                    .grow(1.)
                    .min_w(0.)
                    .wrap_anywhere(),
            )
            .child(
                tool(
                    "clip-sound",
                    if c.sound {
                        "Mute video"
                    } else {
                        "Enable video sound"
                    },
                    if c.sound {
                        &icons::SOUND
                    } else {
                        &icons::MUTED_SOUND
                    },
                )
                .on_press_tx(|s: &mut State, tx| s.video_control(tx, |c| c.toggle_sound())),
            )
            .child(
                action("clip-prev", "Previous frame", &icons::PREVIOUS_FRAME, -1)
                    .disabled(c.current == 0),
            )
            .child(action(
                "clip-play",
                if c.playing {
                    "Pause video"
                } else {
                    "Play video"
                },
                if c.playing {
                    &icons::PAUSE
                } else {
                    &glyphs::TRIANGLE_RIGHT
                },
                0,
            ))
            .child(
                action("clip-next", "Next frame", &icons::NEXT_FRAME, 1)
                    .disabled(c.current + 1 >= c.total),
            ),
    );
    if let Some(e) = &c.sound_error {
        out = out.child(
            mono(e, MUTED)
                .key("clip-audio-error")
                .w_full()
                .wrap_anywhere(),
        );
    }
    if c.audio_active() {
        out = out.child(
            row()
                .key("clip-audio-active")
                .label(format!("PCM level {} percent", c.peak))
                .center_y()
                .gap(6.)
                .child(mono("Local audio", MUTED))
                .child(
                    el().w(75.)
                        .h(3.)
                        .fill(super::c(EDGE))
                        .child(el().w(0.75 * c.peak as f32).h(3.).fill(super::c(INK))),
                ),
        );
    }
    out
}

pub(super) fn view(p: &AttachmentPreview, inset: f32) -> El {
    let id = p.file.id;
    let name = p.file.name.clone();
    let header = row()
        .w_full()
        .center_y()
        .gap(10.0)
        .child(
            tool("close-preview", "Back to message", &glyphs::CLOSE)
                .on_press_tx(State::close_preview),
        )
        .child(
            label(&name, 20.0, 600.0, INK)
                .grow(1.0)
                .min_w(0.0)
                .wrap_anywhere(),
        )
        .child(
            tool("export-preview", "Save attachment", &glyphs::FOLDER_OPEN)
                .on_press_tx(move |s: &mut State, tx| s.save_attachment(id, &name, tx)),
        );
    let mut content = col()
        .key("attachment-preview")
        .w_full()
        .px(inset)
        .py(18.0)
        .gap(14.0)
        .child(header)
        .child(
            mono(
                format!(
                    "{}   {} bytes   Message #{}",
                    p.file.content_type, p.file.size, p.message
                ),
                MUTED,
            )
            .w_full()
            .wrap_anywhere(),
        )
        .child(rule());
    match &p.content {
        Preview::Clip(clip) => {
            if let Some(frame) = clip.frame() {
                content = content.child(
                    el().key("clip-frame")
                        .role("img")
                        .label("Video preview frame")
                        .w_full()
                        .h(260.0)
                        .pixels_fit(frame, SurfaceFit::Fit),
                );
            } else {
                content = content.child(
                    label(
                        clip.error.as_deref().unwrap_or("Loading video preview"),
                        14.0,
                        500.0,
                        MUTED,
                    )
                    .key("clip-status")
                    .min_h(260.)
                    .w_full()
                    .wrap_anywhere(),
                );
            }
            if clip.total > 0 || clip.error.is_none() {
                content = content.child(transport(clip));
            }
        }
        Preview::Image {
            pixels,
            width,
            height,
            motion,
        } => {
            content = content
                .child(
                    el().key("preview-image")
                        .role("img")
                        .label(&p.file.name)
                        .w_full()
                        .h(300.0)
                        .pixels_fit(
                            motion.as_ref().map_or(pixels, |m| m.pixels()),
                            SurfaceFit::Fit,
                        ),
                )
                .child(mono(format!("{width} x {height} px"), MUTED));
        }
        Preview::Text { body, shortened } => {
            content = content.child(
                mono(body, INK)
                    .key("preview-text")
                    .w_full()
                    .wrap_anywhere()
                    .selectable(true),
            );
            if *shortened {
                content = content.child(mono("Preview truncated", MUTED));
            }
        }
        Preview::Other(reason) => {
            content = content.child(
                label(reason, 15.0, 500.0, MUTED)
                    .key("preview-unavailable")
                    .w_full()
                    .wrap_anywhere(),
            );
        }
    }
    scroll("preview-scroll", content)
}
