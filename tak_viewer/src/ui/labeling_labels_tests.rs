    use super::*;
    use crate::ui::panels::{LabelsState, ListSelectionState};
    use std::collections::BTreeSet;
    use tak_core::tak::MarkerKind;
    use tak_core::view_data::{GroupKind, ViewData};
    use tak_core::view_data::{ColorSource, Group};


    #[derive(Debug, PartialEq, Eq)]
    enum Row { Header(usize), Markers(usize, Option<usize>) }

    fn pane_state(data: &ViewData, st: &LabelsState, selected: &BTreeSet<usize>, primary: Option<usize>, frame: i64) -> LabelingState {
        LabelingState {
            filter: st.filter.clone(),
            groups: data.groups.iter().enumerate().map(|(gi, group)| LabelGroup {
                id: gi as u64, asset_uid: None, title: group.title.clone(),
                collapsed: st.collapsed.contains(&gi),
                labels: group.members.iter().map(|&i| {
                    let m = &data.markers[i];
                    LabelRow { key: None, track_uid: Some(m.uid), has_samples: m.n_samples > 0, name: m.name.clone(), color: theme::color(m.color), fill_pct: m.fill_pct, present: m.present(frame), selected: selected.contains(&i), primary: primary == Some(i) }
                }).collect(),
            }).collect(),
            scroll_to_track: st.scroll_to.map(|i| data.markers[i].uid),
            ..Default::default()
        }
    }

    fn rows(data: &ViewData, st: &LabelsState, count: usize) -> Vec<Row> {
        let state = pane_state(data, st, &BTreeSet::new(), None, 0);
        list_rows(&state, count).into_iter().map(|row| match row {
            ListRow::Group(gi) => Row::Header(gi),
            ListRow::Labels(gi, left, right) => Row::Markers(data.groups[gi].members[left], right.map(|i| data.groups[gi].members[i])),
        }).collect()
    }
    fn data(groups: &[&[&str]]) -> ViewData {
        let names: Vec<&str> = groups.iter().flat_map(|g| g.iter().copied()).collect();
        let mut take = crate::views::sample_take("labels.tak", &names);
        for t in &mut take.trajectories {
            t.kind = MarkerKind::Template;
        }
        let mut d = ViewData::build(take);
        let mut first = 0;
        d.groups = groups.iter().enumerate().map(|(gi, names)| {
            let members: Vec<usize> = (first..first + names.len()).collect();
            first += names.len();
            for &mi in &members {
                d.markers[mi].group = gi;
            }
            Group { title: format!("Group{gi}"), kind: GroupKind::AssetTemplate, color: ColorSource::Asset(gi), members, asset: None }
        }).collect();
        d
    }

    #[test]
    fn layout_reads_down_first_per_group_in_existing_order_with_odd_single_and_empty_groups() {
        let mut d = data(&[&["M0", "M1", "M2", "M3", "M4"], &["M5"], &["M6", "M7", "M8", "M9"], &[]]);
        // Existing group order, rather than numeric marker order, is the reading order.
        d.groups[0].members = vec![2, 0, 4, 1, 3];
        let st = LabelsState::default();
        assert_eq!(rows(&d, &st, 2), vec![
            Row::Header(0), Row::Markers(2, Some(1)), Row::Markers(0, Some(3)), Row::Markers(4, None),
            Row::Header(1), Row::Markers(5, None),
            Row::Header(2), Row::Markers(6, Some(8)), Row::Markers(7, Some(9)), Row::Header(3),
        ]);
        let single = rows(&d, &st, 1);
        let markers: Vec<usize> = single.iter().filter_map(|r| match *r {
            Row::Markers(left, None) => Some(left),
            Row::Markers(_, Some(_)) => panic!("one column has no right marker"),
            Row::Header(_) => None,
        }).collect();
        assert_eq!(markers, [2, 0, 4, 1, 3, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn collapsed_groups_and_search_keep_the_existing_rules_and_split_only_matches() {
        let mut d = data(&[&["keep A", "skip", "Keep B", "keep C"], &["other"], &["keep D"]]);
        d.groups[2].kind = GroupKind::Unlabeled;
        let mut st = LabelsState::default();
        st.reset(&d, 7);
        assert!(st.collapsed.contains(&2), "Unlabeled still starts collapsed");
        st.collapsed.insert(0);
        assert_eq!(rows(&d, &st, 2), [Row::Header(0), Row::Header(1), Row::Markers(4, None), Row::Header(2)]);
        st.filter = "  KEEP  ".into();
        assert_eq!(rows(&d, &st, 2), [
            Row::Header(0), Row::Markers(0, Some(3)), Row::Markers(2, None),
            Row::Header(2), Row::Markers(5, None),
        ], "search opens matching collapsed groups and hides groups without matches");
        st.filter = "no match".into();
        assert!(rows(&d, &st, 2).is_empty());
    }

    #[test]
    fn column_threshold_and_scroll_offsets_use_both_cells_and_the_real_pitch() {
        assert_eq!(label_columns(259.9), 1);
        assert_eq!(label_columns(260.0), 2);
        assert_eq!(label_columns(380.0), 2);
        let data = data(&[&["M0", "M1", "M2", "M3", "M4", "M5", "M6"]]);
        let mut state = pane_state(&data, &LabelsState::default(), &BTreeSet::new(), None, 0);
        let list = list_rows(&state, 2);
        for (target, expected) in [(4, Some(0.0)), (6, Some(0.0)), (3, Some(29.0))] {
            state.scroll_to_track = Some(data.markers[target].uid);
            assert_eq!(label_scroll_offset(&list, &state, 29.0), expected);
        }
        state.scroll_to_track = Some([99; 16]);
        assert_eq!(label_scroll_offset(&list, &state, 29.0), None);
        assert_eq!(label_scroll_offset(&[ListRow::Group(0)], &state, 29.0), None);
    }

    struct PaneTest {
        ctx: egui::Context,
        data: ViewData,
        state: LabelsState,
        selected: BTreeSet<usize>,
        primary: Option<usize>,
        frame: i64,
        width: f32,
        height: f32,
        spacing: f32,
    }

    impl PaneTest {
        fn new(groups: &[&[&str]]) -> Self {
            let data = data(groups);
            let mut state = LabelsState::default();
            state.reset(&data, 7);
            Self { ctx: egui::Context::default(), data, state, selected: BTreeSet::new(), primary: None, frame: 0, width: 380.0, height: 220.0, spacing: 3.0 }
        }

        fn draw(&mut self, modifiers: egui::Modifiers, events: Vec<egui::Event>) -> (egui::FullOutput, Option<LabelingAction>) {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(self.width + 16.0, (self.height + 400.0).min(700.0)))),
                focused: true, modifiers, events, ..Default::default()
            };
            let mut action = None;
            let ctx = self.ctx.clone();
            let out = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.set_width(self.width);
                    ui.spacing_mut().item_spacing.y = self.spacing;
                    let state = pane_state(&self.data, &self.state, &self.selected, self.primary, self.frame);
                    let result = show(ui, &state);
                    self.state.scroll_to = None;
                    action = match result {
                        Some(LabelingAction::ToggleGroup(id)) => { if !self.state.collapsed.remove(&(id as usize)) { self.state.collapsed.insert(id as usize); } None },
                        other => other,
                    };
                });
            });
            (out, action)
        }

        fn idle(&mut self) -> egui::FullOutput {
            self.draw(egui::Modifiers::NONE, vec![]).0
        }

        fn click(&mut self, at: egui::Pos2, modifiers: egui::Modifiers) -> Option<LabelingAction> {
            let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers };
            self.draw(modifiers, vec![egui::Event::PointerMoved(at)]);
            self.draw(modifiers, vec![button(true)]);
            self.draw(modifiers, vec![button(false)]).1
        }
    }

    struct Painted {
        text: String,
        glyphs: String,
        rect: Rect,
        clip: Rect,
        color: Color32,
        elided: bool,
    }

    fn texts(out: &egui::FullOutput) -> Vec<Painted> {
        fn walk(shape: &egui::Shape, clip: Rect, found: &mut Vec<Painted>) {
            match shape {
                egui::Shape::Text(t) => found.push(Painted {
                    text: t.galley.text().to_owned(),
                    glyphs: t.galley.rows.iter().flat_map(|r| r.glyphs.iter().map(|g| g.chr)).collect(),
                    rect: Rect::from_min_size(t.pos, t.galley.size()), clip,
                    color: t.galley.job.sections.first().map_or(t.fallback_color, |s| s.format.color),
                    elided: t.galley.elided,
                }),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, clip, found)),
                _ => {}
            }
        }
        let mut found = vec![];
        for s in &out.shapes {
            walk(&s.shape, s.clip_rect, &mut found);
        }
        found
    }

    fn at<'a>(texts: &'a [Painted], name: &str) -> &'a Painted {
        texts.iter().find(|t| t.text == name).unwrap_or_else(|| panic!("{name} was not painted"))
    }

    #[test]
    fn at_380pt_each_row_has_the_column_major_names_and_right_aligned_coverage() {
        let mut p = PaneTest::new(&[&["M0", "M1", "M2", "M3", "M4"]]);
        for (i, m) in p.data.markers.iter_mut().enumerate() {
            m.fill_pct = if i == 2 { 100.0 } else { 90.0 - i as f32 * 10.0 };
            m.gaps = vec![(2, 3), (5, 6)];
            m.max_gap = 123;
        }
        let painted = texts(&p.idle());
        assert_eq!(painted.iter().filter(|t| t.text == "Labels").count(), 2);
        let mut headers: Vec<_> = painted.iter().filter(|t| t.text == "%" && (t.rect.center().y - at(&painted, "Labels").rect.center().y).abs() < 0.5).collect();
        headers.sort_by(|a, b| a.rect.left().total_cmp(&b.rect.left()));
        assert_eq!(headers.len(), 2);
        for (left, right) in [("M0", "M3"), ("M1", "M4")] {
            assert!((at(&painted, left).rect.center().y - at(&painted, right).rect.center().y).abs() < 0.1);
            assert!((at(&painted, right).rect.left() - at(&painted, left).rect.left() - 190.0).abs() < 0.5);
        }
        assert!(at(&painted, "M0").rect.top() < at(&painted, "M1").rect.top());
        assert!(at(&painted, "M1").rect.top() < at(&painted, "M2").rect.top());
        for (value, col) in [("90%", 0), ("80%", 0), ("100%", 0), ("60%", 1), ("50%", 1)] {
            assert!((at(&painted, value).rect.right() - headers[col].rect.right()).abs() < 0.5, "{value} right-aligned to its header");
        }
        assert!(!painted.iter().any(|t| ["빈구간", "최대", "123", "2", "0%"].contains(&t.text.as_str())), "old header/cell numbers are gone");
        assert!(painted.iter().any(|t| t.text.contains("Gap count and longest gap in Selected Marker.")), "the legend tells where the other numbers are");
    }

    #[test]
    fn right_column_click_ctrl_command_and_shift_keep_full_column_order_and_endpoint_target() {
        let mut p = PaneTest::new(&[&["M0", "M1", "M2", "M3", "M4"]]);
        let painted = texts(&p.idle());
        let mut v = crate::views::View::default();
        let ctrl = egui::Modifiers { ctrl: true, ..Default::default() };
        let command = egui::Modifiers { command: true, ..Default::default() };
        let shift = egui::Modifiers { shift: true, ..Default::default() };
        for (name, modifiers, want, selection) in [
            ("M3", egui::Modifiers::NONE, 3, vec![3]),
            ("M0", ctrl, 0, vec![0, 3]),
            ("M3", ctrl, 3, vec![0]),
            ("M4", command, 4, vec![0, 4]),
            ("M1", shift, 1, vec![1, 2, 3, 4]),
        ] {
            let action = p.click(at(&painted, name).rect.center(), modifiers).expect("row selection");
            let state = pane_state(&p.data, &p.state, &p.selected, p.primary, p.frame);
            let order = state.track_order(MarkerList::Named);
            let current: Vec<_> = v.ordered_selection().iter().map(|&i| p.data.markers[i].uid).collect();
            let gesture = match action {
                LabelingAction::SelectLabel { track_uid: Some(uid), additive: false, .. } => {
                    assert_eq!(uid, p.data.markers[want].uid);
                    SelectionGesture::Plain
                }
                LabelingAction::SelectTracks { list: MarkerList::Named, uid: Some(uid), gesture } => {
                    assert_eq!(uid, p.data.markers[want].uid);
                    gesture
                }
                other => panic!("unexpected {other:?}"),
            };
            let selected = p.state.list_selection[0].apply(&order, &current, Some(p.data.markers[want].uid), gesture).unwrap();
            v.selection_order = selected.iter().map(|uid| p.data.markers.iter().position(|m| m.uid == *uid).unwrap()).collect();
            v.selected = v.selection_order.iter().copied().collect();
            v.primary = v.selection_order.last().copied();
            assert_eq!(v.selected.iter().copied().collect::<Vec<_>>(), selection);
            if modifiers.shift { assert_eq!(v.primary, Some(want), "range endpoint is Pattern target"); }
            p.selected = v.selected.clone();
            p.primary = v.primary;
        }
    }

    #[test]
    fn odd_empty_cell_is_not_a_click_target_and_group_header_keeps_full_width() {
        let mut p = PaneTest::new(&[&["M0", "M1", "M2", "M3", "M4"]]);
        let painted = texts(&p.idle());
        let empty = at(&painted, "M2").rect.center() + vec2(190.0, 0.0);
        assert!(p.click(empty, egui::Modifiers::NONE).is_none());
        let header_right = pos2(at(&painted, "M3").rect.center().x, at(&painted, "Group0").rect.center().y);
        assert!(p.click(header_right, egui::Modifiers::NONE).is_none());
        assert!(p.state.collapsed.contains(&0));
        let after = texts(&p.idle());
        assert!(!after.iter().any(|t| t.text == "M0" || t.text == "M3"));
        p.click(header_right, egui::Modifiers::NONE);
        assert!(!p.state.collapsed.contains(&0));
    }

    #[test]
    fn long_names_in_both_cells_are_font_elided_and_the_coverage_stays_visible() {
        let left = "A_very_long_marker_name_abcdefghijklmnopqrstuvwxyz";
        let right = "B_very_long_marker_name_abcdefghijklmnopqrstuvwxyz";
        let mut p = PaneTest::new(&[&[left, right]]);
        p.data.markers[0].fill_pct = 83.0;
        p.data.markers[1].fill_pct = 57.0;
        let painted = texts(&p.idle());
        for (name, value) in [(left, "83%"), (right, "57%")] {
            let (name, value) = (at(&painted, name), at(&painted, value));
            assert!(name.elided);
            assert!(name.glyphs.ends_with('…'), "inspect painted glyphs, not the original LayoutJob text");
            assert!(name.rect.right() <= name.clip.right() + 0.5);
            assert!(name.rect.right() < value.rect.left());
            assert!(!value.elided);
            assert_eq!(value.glyphs, value.text);
            assert!(value.clip.contains_rect(value.rect));
        }
    }

    #[test]
    fn narrow_pane_paints_one_column_and_resizing_to_260pt_restores_two() {
        let mut p = PaneTest::new(&[&["M0", "M1", "M2", "M3", "M4"]]);
        p.width = 259.0;
        let painted = texts(&p.idle());
        assert_eq!(painted.iter().filter(|t| t.text == "%").count(), 2);
        assert_eq!(painted.iter().filter(|t| t.text == "Labels").count(), 1);
        for i in 1..5 {
            let (a, b) = (at(&painted, &format!("M{}", i - 1)), at(&painted, &format!("M{i}")));
            assert!((a.rect.left() - b.rect.left()).abs() < 0.1);
            assert!(a.rect.top() < b.rect.top());
        }
        p.width = 260.0;
        let painted = texts(&p.idle());
        assert_eq!(painted.iter().filter(|t| t.text == "%").count(), 3);
        assert!((at(&painted, "M0").rect.top() - at(&painted, "M3").rect.top()).abs() < 0.1);
    }

    #[test]
    fn right_column_scroll_target_is_visible_with_nondefault_spacing_and_only_nearby_rows_are_painted() {
        let names: Vec<String> = (0..200).map(|i| format!("M{i:03}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut p = PaneTest::new(&[&refs]);
        p.height = 130.0;
        p.spacing = 9.0;
        let before = texts(&p.idle());
        assert!(!before.iter().any(|t| t.text == "M180"));
        p.state.scroll_to = Some(180);
        let out = p.idle();
        let painted = texts(&out);
        let target = at(&painted, "M180");
        assert!(target.clip.contains_rect(target.rect));
        assert!((target.rect.top() - at(&painted, "M080").rect.top()).abs() < 0.1);
        // The name has its own cell clip. Use the row background's clip for the viewport.
        let (row_top, viewport_top) = out.shapes.iter().find_map(|s| match &s.shape {
            egui::Shape::Rect(r) if r.rect.contains(target.rect.center()) && (r.rect.height() - ROW_H).abs() < 0.1 => Some((r.rect.top(), s.clip_rect.top())),
            _ => None,
        }).expect("the target's background is painted");
        assert!((row_top - viewport_top - 3.0 * (ROW_H + p.spacing)).abs() < 6.0, "three actual row pitches below the viewport top");
        assert_eq!(p.state.scroll_to, None);
        assert!(!painted.iter().any(|t| t.text == "M000" || t.text == "M199"));
        assert!(painted.iter().filter(|t| t.text.starts_with('M')).count() < 32, "virtualized instead of painting all 200 markers");
        assert!(texts(&p.idle()).iter().any(|t| t.text == "M180"));
    }

    #[test]
    fn legend_keeps_space_below_a_list_that_fills_the_remaining_pane() {
        let names: Vec<String> = (0..200).map(|i| format!("M{i:03}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut p = PaneTest::new(&[&refs]);
        p.height = f32::INFINITY;
        let painted = texts(&p.idle());
        let legend = painted.iter().find(|t| t.text.contains("Gap count and longest gap")).expect("the legend is painted");
        assert!(legend.clip.contains_rect(legend.rect), "the docked pane must not clip its legend below the scroll area");
        assert!(legend.rect.bottom() <= 692.5, "inside the 700 pt screen's panel margin");
    }

    #[test]
    fn marker_colors_absence_and_primary_secondary_highlights_stay_per_cell() {
        let mut p = PaneTest::new(&[&["M0", "M1", "M2", "M3"]]);
        p.selected = [0, 2].into_iter().collect();
        p.primary = Some(2);
        let out = p.idle();
        let painted = texts(&out);
        assert_eq!(at(&painted, "M0").color, theme::LABEL_TRACKED);
        assert_eq!(at(&painted, "M2").color, theme::LABEL_TRACKED);
        for (name, fill) in [("M0", theme::SELECT_SECONDARY.gamma_multiply(0.22)), ("M2", theme::SELECT_PRIMARY.gamma_multiply(0.28))] {
            assert!(out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.fill == fill && r.rect.contains(at(&painted, name).rect.center()) && (r.rect.width() - 190.0).abs() < 0.5)));
        }
        for m in &p.data.markers {
            assert!(out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Circle(c) if c.fill == theme::color(m.color) && (c.radius - 4.0).abs() < 0.1)));
        }
        p.frame = 10;
        let absent = texts(&p.idle());
        assert_eq!(at(&absent, "M0").color, theme::LABEL_ABSENT);
        assert_eq!(at(&absent, "M2").color, theme::LABEL_ABSENT);
    }

    fn ids(values: &[u8]) -> Vec<tak_core::tak::Uid> { values.iter().map(|&v| [v; 16]).collect() }

    #[test]
    fn full_selection_order_includes_search_asset_and_fold_hidden_rows_but_excludes_empty_tracks() {
        let d = data(&[&["M0", "M1", "M2"], &["M3", "M4"]]);
        let mut st = LabelsState::default();
        st.collapsed.insert(1);
        let mut pane = pane_state(&d, &st, &BTreeSet::new(), None, 0);
        pane.filter = "M0".into();
        pane.groups[0].asset_uid = Some([7; 16]);
        pane.groups[1].asset_uid = Some([8; 16]);
        pane.asset_filter = Some([7; 16]);
        pane.groups[0].labels[1].has_samples = false;
        pane.groups[0].labels[2].present = false; // Absent now, sampled elsewhere: eligible.
        pane.groups[1].labels[1].track_uid = None;
        let order = pane.track_order(MarkerList::Named);
        assert_eq!(order, [d.markers[0].uid, d.markers[2].uid, d.markers[3].uid]);
        let mut selection = ListSelectionState::default();
        let picked = selection.apply(&order, &[], Some(order[0]), SelectionGesture::Plain).unwrap();
        assert_eq!(selection.apply(&order, &picked, Some(order[2]), SelectionGesture::Range { additive: false }), Some(order.clone()));
        let before = selection.clone();
        assert!(selection.apply(&order, &picked, Some(d.markers[1].uid), SelectionGesture::Toggle).is_none());
        assert_eq!(selection, before, "empty tracks cannot change anchors or selection");
    }

    #[test]
    fn range_shrinks_reverses_and_ctrl_shift_preserves_only_the_original_union() {
        let order = ids(&[0, 1, 2, 3, 4, 5]);
        let mut selection = ListSelectionState::default();
        let picked = selection.apply(&order, &ids(&[9]), Some(order[2]), SelectionGesture::Toggle).unwrap();
        let wide = selection.apply(&order, &picked, Some(order[5]), SelectionGesture::Range { additive: true }).unwrap();
        assert_eq!(wide, ids(&[9, 2, 3, 4, 5]));
        let small = selection.apply(&order, &wide, Some(order[3]), SelectionGesture::Range { additive: true }).unwrap();
        assert_eq!(small, ids(&[9, 2, 3]), "old temporary rows disappear");
        let reverse = selection.apply(&order, &small, Some(order[0]), SelectionGesture::Range { additive: true }).unwrap();
        assert_eq!(reverse, ids(&[9, 2, 1, 0]));
        let plain_range = selection.apply(&order, &reverse, Some(order[4]), SelectionGesture::Range { additive: false }).unwrap();
        assert_eq!(plain_range, ids(&[2, 3, 4]));
        let shrunk = selection.apply(&order, &plain_range, Some(order[1]), SelectionGesture::Range { additive: false }).unwrap();
        assert_eq!(shrunk, ids(&[2, 1]));
    }

    #[test]
    fn separate_list_anchors_survive_other_list_gestures_and_external_selection_restarts_union() {
        let order = ids(&[0, 1, 2, 3]);
        let unlabeled = ids(&[10, 11, 12]);
        let mut anchors: [ListSelectionState; 2] = Default::default();
        let picked = anchors[0].apply(&order, &[], Some(order[1]), SelectionGesture::Plain).unwrap();
        let picked = anchors[1].apply(&unlabeled, &picked, Some(unlabeled[2]), SelectionGesture::Toggle).unwrap();
        let ranged = anchors[0].apply(&order, &picked, Some(order[3]), SelectionGesture::Range { additive: false }).unwrap();
        assert_eq!(ranged, ids(&[1, 2, 3]));
        assert_eq!(anchors[1].anchor, Some(unlabeled[2]));
        let ranged = anchors[1].apply(&unlabeled, &ranged, Some(unlabeled[0]), SelectionGesture::Range { additive: false }).unwrap();
        assert_eq!(ranged, ids(&[11, 12, 10]));
        let _ = anchors[0].apply(&order, &ranged, Some(order[3]), SelectionGesture::Range { additive: true });
        let externally_picked = ids(&[8]);
        let next = anchors[0].apply(&order, &externally_picked, Some(order[2]), SelectionGesture::Range { additive: true }).unwrap();
        assert_eq!(next, ids(&[8, 1, 2]));
    }

    #[test]
    fn select_all_keeps_included_primary_otherwise_uses_first_in_list_order() {
        let order = ids(&[4, 1, 3, 0]);
        let mut selection = ListSelectionState::default();
        let all = selection.apply(&order, &ids(&[7, 1]), None, SelectionGesture::All).unwrap();
        assert_eq!(all, ids(&[4, 3, 0, 1]));
        assert_eq!(selection.anchor, Some([1; 16]));
        let all = selection.apply(&order, &ids(&[1, 7]), None, SelectionGesture::All).unwrap();
        assert_eq!(all, ids(&[1, 3, 0, 4]));
        assert_eq!(selection.anchor, Some([4; 16]));
        assert_eq!(selection.apply(&[], &all, None, SelectionGesture::All), Some(vec![]));
        assert_eq!(selection.anchor, None);
    }

    #[test]
    fn pinned_selection_summary_counts_hidden_rows_and_names_the_hidden_target() {
        let d = data(&[&["M0", "M1", "M2"], &["M3", "M4", "M5"]]);
        let mut st = LabelsState::default();
        st.collapsed.insert(1);
        let selected = (0..6).collect();
        let pane = pane_state(&d, &st, &selected, Some(4), 0);
        assert_eq!(pane.selection_summary(), ("6 selected (3 hidden)".into(), "Pattern target: M4".into()));
        let ctx = egui::Context::default();
        let output = ctx.run(egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(380.0, 280.0))), ..Default::default() }, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| { show(ui, &pane); });
        });
        let painted = texts(&output);
        for name in ["6 selected (3 hidden)", "Pattern target: M4"] {
            let label = at(&painted, name);
            assert!(label.clip.contains_rect(label.rect));
            assert!(label.rect.top() < at(&painted, "QuickLabel Mode  D").rect.top());
        }
    }
