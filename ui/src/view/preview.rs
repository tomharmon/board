use super::*;
use crate::state::{AttachmentPreview, Preview};
use rune::SurfaceFit;

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
            if let Some(frame) = clip.frames.get(clip.current) {
                let ctrl = |key: &str, name: &str, glyph: &'static Icon, dir: isize| {
                    tool(key, name, glyph).on_press_tx(move |s: &mut State, tx| {
                        if let Some(AttachmentPreview {
                            content: Preview::Clip(c),
                            ..
                        }) = &mut s.preview
                        {
                            if dir == 0 {
                                c.play();
                                tx.wake_after(std::time::Duration::from_millis(20));
                            } else {
                                c.step(dir);
                            }
                        }
                    })
                };
                content = content
                    .child(
                        el().key("clip-frame")
                            .role("img")
                            .label("Video preview frame")
                            .w_full()
                            .h(260.0)
                            .pixels_fit(frame, SurfaceFit::Fit),
                    )
                    .child(
                        row()
                            .w_full()
                            .center_y()
                            .gap(6.0)
                            .child(
                                mono(
                                    format!(
                                        "Silent sample   Frame {} of {}",
                                        clip.current + 1,
                                        clip.frames.len()
                                    ),
                                    MUTED,
                                )
                                .grow(1.0)
                                .min_w(0.0)
                                .wrap_anywhere(),
                            )
                            .child(
                                ctrl("clip-prev", "Previous frame", &glyphs::CHEVRON_UP, -1)
                                    .disabled(clip.current == 0),
                            )
                            .child(ctrl(
                                "clip-play",
                                if clip.playing {
                                    "Pause sample"
                                } else {
                                    "Play sample"
                                },
                                if clip.playing {
                                    &glyphs::DASH
                                } else {
                                    &glyphs::TRIANGLE_RIGHT
                                },
                                0,
                            ))
                            .child(
                                ctrl("clip-next", "Next frame", &glyphs::CHEVRON_DOWN, 1)
                                    .disabled(clip.current + 1 >= clip.frames.len()),
                            ),
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
                    .w_full()
                    .wrap_anywhere(),
                );
            }
        }
        Preview::Image {
            pixels,
            width,
            height,
        } => {
            content = content
                .child(
                    el().key("preview-image")
                        .role("img")
                        .label(&p.file.name)
                        .w_full()
                        .h(300.0)
                        .pixels_fit(pixels, SurfaceFit::Fit),
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
