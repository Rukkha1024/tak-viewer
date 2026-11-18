use super::*;
use std::collections::BTreeSet;
use tak_core::view_data::ViewData;

#[test]
fn marker_menu_selection_matrix_uses_membership_and_count() {
    let mut take = crate::views::sample_take("matrix.tak", &["A", "Unlabeled", "Looks named"]);
    let trajectories = &mut take.trajectories;
    trajectories[0].kind = MarkerKind::Template;
    trajectories[0].parent_uid = Some([9; 16]);
    trajectories[0].slot = Some(1);
    trajectories[1].kind = MarkerKind::Unlabeled;
    trajectories[1].parent_uid = None;
    trajectories[1].slot = None;
    trajectories[2].kind = MarkerKind::Unlabeled;
    trajectories[2].parent_uid = None;
    trajectories[2].slot = None;
    let data = ViewData::build(take);
    let named = data.markers.iter().position(|m| m.name == "A").unwrap();
    let unlabeled = data
        .markers
        .iter()
        .position(|m| m.name == "Unlabeled")
        .unwrap();
    let another = data
        .markers
        .iter()
        .position(|m| m.name == "Looks named")
        .unwrap();
    for (indices, label, unlabel) in [
        (vec![], false, false),
        (vec![unlabeled], true, false),
        (vec![named], true, true),
        (vec![named, unlabeled], false, true),
        (vec![unlabeled, another], false, false),
    ] {
        let selected = BTreeSet::from_iter(indices);
        let s = Selection::read(&data, &selected);
        assert_eq!(s.count, selected.len());
        assert_eq!(s.source.is_some(), label, "{selected:?}");
        assert_eq!(s.named, unlabel, "{selected:?}");
    }
}

#[test]
fn marker_menu_long_catalog_paints_only_visible_rows() {
    fn drawn_rows(count: i32) -> (usize, std::time::Duration) {
        let rows: Vec<_> = (0..count)
            .map(|slot| {
                CatalogRow::Label(
                    format!("Target {slot}"),
                    LabelKey {
                        parent: [9; 16],
                        slot,
                    },
                )
            })
            .collect();
        let ctx = egui::Context::default();
        let draw = || {
            ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| show_labels(ui, &rows));
            })
        };
        draw();
        let start = std::time::Instant::now();
        let output = draw();
        let duration = start.elapsed();
        let count = output.shapes.iter().filter(|shape| {
            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text().starts_with("Target "))
        }).count();
        (count, duration)
    }

    let short = drawn_rows(200);
    let long = drawn_rows(20_000);
    eprintln!("Label submenu, 200 vs 20,000 labels: {short:?} / {long:?}");
    assert_eq!(short.0, long.0);
    assert!((1..=30).contains(&long.0));
}
