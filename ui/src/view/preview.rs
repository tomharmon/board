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
