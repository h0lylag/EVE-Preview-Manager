use crate::common::color::HexColor;

/// Edit an RGB/ARGB string without replacing unfinished input until a color is picked.
pub(super) fn ui(ui: &mut egui::Ui, value: &mut String) -> bool {
    let mut changed = ui
        .add(egui::TextEdit::singleline(value).desired_width(100.0))
        .on_hover_text("#RRGGBB or #AARRGGBB (alpha first)")
        .changed();

    let id = ui.next_auto_id().with("color_state");
    // Keep hue for gray colors between frames; resync when the backing text changes.
    let mut hsva = ui
        .data(|data| data.get_temp::<(String, egui::ecolor::Hsva)>(id))
        .filter(|(previous, _)| previous == value)
        .map(|(_, hsva)| hsva)
        .unwrap_or_else(|| {
            // The fallback belongs only to the picker; malformed text stays editable.
            let [alpha, red, green, blue] = HexColor::parse(value)
                .map(|color| color.argb32().to_be_bytes())
                .unwrap_or([255; 4]);
            // Convert RGB separately: premultiplied adapters discard it at zero alpha.
            let mut hsva = egui::ecolor::Hsva::from_srgb([red, green, blue]);
            hsva.a = f32::from(alpha) / 255.0;
            hsva
        });
    // egui 0.36.1 still loses hidden RGB when its U8 alpha field is set to zero;
    // the alpha slider and this control's exact ARGB field preserve it.
    if egui::color_picker::color_edit_button_hsva(
        ui,
        &mut hsva,
        egui::color_picker::Alpha::OnlyBlend,
    )
    .changed()
    {
        let [red, green, blue, alpha] = hsva.to_srgba_unmultiplied();
        *value = if alpha == 255 {
            format!("#{red:02X}{green:02X}{blue:02X}")
        } else {
            format!("#{alpha:02X}{red:02X}{green:02X}{blue:02X}")
        };
        changed = true;
    }
    ui.data_mut(|data| data.insert_temp(id, (value.clone(), hsva)));
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        ctx: &egui::Context,
        value: &mut String,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, egui::Rect, bool) {
        let mut rect = egui::Rect::NOTHING;
        let mut changed = false;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                rect = ui
                    .horizontal(|ui| changed |= super::ui(ui, value))
                    .response
                    .rect;
            },
        );
        output.textures_delta.clear();
        (output, rect, changed)
    }

    fn click(ctx: &egui::Context, value: &mut String, pos: egui::Pos2) -> bool {
        let mut changed = false;
        for pressed in [true, false] {
            changed |= frame(
                ctx,
                value,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            )
            .2;
        }
        changed
    }

    #[test]
    fn color_picker_opens_without_rewriting_input_and_recovers_invalid_text() {
        for input in [
            "",
            "#",
            "#FF",
            "#GG0000",
            "#€ABC",
            "#00FF0000",
            "#0000FF00",
            "#000000FF",
            "#801234AB",
            "#ff0000",
            "#FFFFFF",
        ] {
            let ctx = egui::Context::default();
            let mut value = input.to_owned();
            for _ in 0..2 {
                assert!(!frame(&ctx, &mut value, vec![]).2);
                assert_eq!(value, input);
            }
            let (_, rect, _) = frame(&ctx, &mut value, vec![]);
            assert!(!click(
                &ctx,
                &mut value,
                rect.right_center() - egui::vec2(10.0, 0.0)
            ));
            assert!(egui::Popup::is_any_open(&ctx), "{input:?}");
            assert_eq!(value, input, "opening the picker must preserve exact text");

            if input == "#FFFFFF" {
                // Changing hue while white must survive until saturation changes.
                let (output, _, _) = frame(&ctx, &mut value, vec![]);
                let hue = output
                    .shapes
                    .iter()
                    .filter_map(|shape| match &shape.shape {
                        egui::epaint::Shape::Mesh(mesh) => {
                            let rect = mesh.calc_bounds();
                            (rect.width() > 200.0 && rect.height() < 40.0).then_some(rect)
                        }
                        _ => None,
                    })
                    .min_by(|a, b| a.top().total_cmp(&b.top()))
                    .unwrap();
                assert!(click(
                    &ctx,
                    &mut value,
                    hue.left_center() + egui::vec2(hue.width() * 2.0 / 3.0, 0.0)
                ));
                assert_eq!(value, "#FFFFFF");
            }
            if HexColor::parse(input).is_none() || input == "#FFFFFF" {
                // Locate egui's saturation/value square from its painted mesh.
                let (output, _, _) = frame(&ctx, &mut value, vec![]);
                let square = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::epaint::Shape::Mesh(mesh) => {
                            let rect = mesh.calc_bounds();
                            (rect.width() > 200.0 && rect.width() == rect.height()).then_some(rect)
                        }
                        _ => None,
                    })
                    .expect("open picker must paint its color square");
                assert!(click(
                    &ctx,
                    &mut value,
                    square.right_top() + egui::vec2(-0.001, 0.001)
                ));
                let [a, r, g, b] = HexColor::parse(&value).unwrap().argb32().to_be_bytes();
                assert_eq!(a, 255);
                if input == "#FFFFFF" {
                    assert_eq!(b, 255);
                    assert!(r <= 1 && g <= 1, "{value}");
                } else {
                    assert_eq!(r, 255);
                    assert!(g <= 1 && b <= 1, "{value}");
                }
            }

            // Alpha edits must preserve straight RGB, including at zero alpha.
            let original = HexColor::parse(&value).unwrap().argb32().to_be_bytes();
            let (output, _, _) = frame(&ctx, &mut value, vec![]);
            let alpha = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Mesh(mesh) => {
                        let rect = mesh.calc_bounds();
                        (rect.width() > 200.0 && rect.height() < 40.0).then_some(rect)
                    }
                    _ => None,
                })
                .max_by(|a, b| a.bottom().total_cmp(&b.bottom()))
                .unwrap();
            assert!(click(
                &ctx,
                &mut value,
                alpha.left_center() + egui::vec2(alpha.width() / 4.0, 0.0)
            ));
            let [a, r, g, b] = HexColor::parse(&value).unwrap().argb32().to_be_bytes();
            assert!((63..=64).contains(&a), "{value}");
            for (actual, expected) in [r, g, b].into_iter().zip(original[1..].iter()) {
                assert!(actual.abs_diff(*expected) <= 1, "{input}: {value}");
            }

            egui::Popup::close_all(&ctx);
            frame(&ctx, &mut value, vec![]);
            click(&ctx, &mut value, rect.left_center() + egui::vec2(20.0, 0.0));
            assert!(
                frame(
                    &ctx,
                    &mut value,
                    vec![
                        egui::Event::Key {
                            key: egui::Key::A,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::COMMAND,
                        },
                        egui::Event::Paste("#001234AB".into()),
                    ]
                )
                .2
            );
            assert_eq!(value, "#001234AB");
            assert!(!frame(&ctx, &mut value, vec![]).2);
            assert_eq!(value, "#001234AB");

            // Reopening after a text edit must use the new color, not cached HSV.
            assert!(!click(
                &ctx,
                &mut value,
                rect.right_center() - egui::vec2(10.0, 0.0)
            ));
            frame(&ctx, &mut value, vec![]);
            assert!(click(
                &ctx,
                &mut value,
                alpha.left_center() + egui::vec2(alpha.width() / 4.0, 0.0)
            ));
            assert_eq!(value, "#401234AB");

            // A profile reload can replace the same field while its popup is open.
            value = "#0000FF00".into();
            assert!(!frame(&ctx, &mut value, vec![]).2);
            assert_eq!(value, "#0000FF00");
            assert!(click(
                &ctx,
                &mut value,
                alpha.left_center() + egui::vec2(alpha.width() / 4.0, 0.0)
            ));
            assert_eq!(value, "#4000FF00");
        }
    }
}
