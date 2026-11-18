// Real pointer events against the GRAPH-only interaction path. The existing HoverGraph
// tests continue to exercise legacy device / 3D-overlay scrubbing via graph::show.
impl HoverGraph {
    fn frame_select(&mut self, events: Vec<eframe::egui::Event>, range: &mut Option<(i64, i64)>) -> (Vec<eframe::egui::epaint::ClippedShape>, Option<i64>) {
        self.frame_select_covered(events, range, None)
    }

    fn frame_select_covered(&mut self, events: Vec<eframe::egui::Event>, range: &mut Option<(i64, i64)>, cover: Option<eframe::egui::Rect>) -> (Vec<eframe::egui::epaint::ClippedShape>, Option<i64>) {
        use eframe::egui;
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0))), events, ..Default::default() };
        let mut goto = None;
        let out = self.ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                goto = super::show_select(ui, egui::Id::new("g"), &self.lanes, 0, 3999, 2000, &[], 100.0, &super::Look::pane(), Some(&mut self.drawn), &[], range, None);
            });
            if let Some(cover) = cover {
                egui::Area::new(egui::Id::new("graph_cover")).order(egui::Order::Foreground).fixed_pos(cover.min).show(ctx, |ui| {
                    ui.allocate_exact_size(cover.size(), egui::Sense::click_and_drag());
                });
            }
        });
        (out.shapes, goto)
    }
}

fn graph_selection_button(pos: eframe::egui::Pos2, pressed: bool) -> eframe::egui::Event {
    use eframe::egui::{Event, Modifiers, PointerButton};
    Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
}

#[test]
fn graph_selection_threshold_is_strictly_over_four_points_and_drag_stays_latched() {
    use eframe::egui::{vec2, Event};
    let mut g = HoverGraph::new(true, false);
    let mut range = Some((50, 75));
    g.frame_select(vec![], &mut range);
    let p = g.at(1, 2000.0, 50.0);
    g.frame_select(vec![Event::PointerMoved(p)], &mut range);
    assert_eq!(g.frame_select(vec![graph_selection_button(p, true)], &mut range).1, None);
    assert_eq!(range, Some((50, 75)), "press keeps the previous range");
    assert_eq!(g.frame_select(vec![Event::PointerMoved(p + vec2(4.0, 0.0))], &mut range).1, None);
    assert_eq!(range, Some((50, 75)), "exactly 4 pt is still a click");
    let to = p + vec2(4.01, 0.0);
    let (shapes, goto) = g.frame_select(vec![Event::PointerMoved(to)], &mut range);
    let end = g.drawn.drew.maps[1].unwrap().frame(to.x).round() as i64;
    assert_eq!((range, goto), (Some((2000, end)), None), "4.01 pt selects before egui's default drag threshold");
    assert!(shapes.iter().any(|s| matches!(&s.shape, eframe::egui::Shape::Rect(r) if r.fill == super::theme::RANGE.gamma_multiply(0.12))), "selection is painted during this input frame");
    assert_eq!((g.drawn.drew.hovered, g.drawn.drew.hover_text.as_ref()), (None, None));
    assert_eq!(g.frame_select(vec![Event::PointerMoved(p)], &mut range).1, None);
    assert_eq!(range, Some((2000, 2000)), "returning to the origin remains a one-frame selection");
    assert_eq!(g.frame_select(vec![graph_selection_button(p, false)], &mut range).1, None, "release never scrubs after a selection");
}

#[test]
fn graph_selection_subthreshold_click_moves_frame_and_keeps_existing_range() {
    use eframe::egui::{vec2, Event};
    for dx in [0.0, 3.9, 4.0] {
        let mut g = HoverGraph::new(true, false);
        let mut range = Some((50, 75));
        g.frame_select(vec![], &mut range);
        let p = g.at(1, 2000.0, 50.0);
        let to = p + vec2(dx, 0.0);
        g.frame_select(vec![Event::PointerMoved(p)], &mut range);
        assert_eq!(g.frame_select(vec![graph_selection_button(p, true)], &mut range).1, None);
        assert_eq!(g.frame_select(vec![Event::PointerMoved(to)], &mut range).1, None);
        let goto = g.frame_select(vec![graph_selection_button(to, false)], &mut range).1;
        assert_eq!(goto, Some(g.drawn.drew.maps[1].unwrap().frame(to.x).round() as i64), "click with {dx} pt movement");
        assert_eq!(range, Some((50, 75)));
    }
}

#[test]
fn graph_selection_orders_and_clamps_endpoints_and_uses_release_position() {
    use eframe::egui::{vec2, Event};
    let mut g = HoverGraph::new(true, false);
    let mut range = None;
    g.frame_select(vec![], &mut range);
    let p = g.at(1, 2100.0, 50.0);
    g.frame_select(vec![Event::PointerMoved(p)], &mut range);
    g.frame_select(vec![graph_selection_button(p, true)], &mut range);
    assert_eq!(g.frame_select(vec![Event::PointerMoved(p - vec2(10000.0, 0.0))], &mut range).1, None);
    assert_eq!(range, Some((0, 2100)), "backward selection reaches the first frame outside the plot");
    assert_eq!(g.frame_select(vec![graph_selection_button(p + vec2(10000.0, 0.0), false)], &mut range).1, None);
    assert_eq!(range, Some((2100, 3999)), "release position is included and clamped to the last frame");
}

#[test]
fn graph_selection_vertical_drag_selects_one_frame_and_works_without_marker_names() {
    use eframe::egui::{vec2, Event};
    let mut g = HoverGraph::new(false, false);
    let mut range = None;
    g.frame_select(vec![], &mut range);
    let p = g.at(1, 2000.0, 50.0);
    g.frame_select(vec![Event::PointerMoved(p)], &mut range);
    g.frame_select(vec![graph_selection_button(p, true)], &mut range);
    assert_eq!(g.frame_select(vec![Event::PointerMoved(p + vec2(0.0, 5.0))], &mut range).1, None);
    assert_eq!(range, Some((2000, 2000)));
    assert_eq!(g.frame_select(vec![graph_selection_button(p + vec2(0.0, 5.0), false)], &mut range).1, None);
}

#[test]
fn graph_selection_bottom_navigation_strip_keeps_scrubbing_even_when_drag_enters_plot() {
    use eframe::egui::{pos2, Event};
    let mut g = HoverGraph::new(true, false);
    let mut range = Some((50, 75));
    g.frame_select(vec![], &mut range);
    let m = g.drawn.drew.maps[2].unwrap();
    let from = pos2(m.x(1000.0), m.bottom + super::NAV_H / 2.0);
    let to = pos2(m.x(3000.0), m.bottom - 20.0);
    g.frame_select(vec![Event::PointerMoved(from)], &mut range);
    g.frame_select(vec![graph_selection_button(from, true)], &mut range);
    assert_eq!(g.frame_select(vec![Event::PointerMoved(to)], &mut range).1, Some(3000), "a press in the bottom strip retains legacy scrubbing");
    g.frame_select(vec![graph_selection_button(to, false)], &mut range);
    assert_eq!(range, Some((50, 75)), "crossing into the plot cannot turn a navigation drag into a range selection");
}

#[test]
fn graph_selection_keeps_wheel_middle_pan_double_click_and_hover() {
    use eframe::egui::{vec2, Event, Id, Modifiers, MouseWheelUnit, PointerButton};
    let mut g = HoverGraph::new(true, false);
    let mut range = Some((50, 75));
    g.frame_select(vec![], &mut range);
    let p = g.at(1, 2000.0, 50.0);
    g.frame_select(vec![Event::PointerMoved(p)], &mut range);
    let full = g.ctx.data(|d| d.get_temp::<ViewRange>(Id::new("g"))).unwrap();
    assert_eq!(g.frame_select(vec![Event::MouseWheel { unit: MouseWheelUnit::Point, delta: vec2(0.0, 150.0), modifiers: Modifiers::NONE }], &mut range).1, None);
    let zoom = g.ctx.data(|d| d.get_temp::<ViewRange>(Id::new("g"))).unwrap();
    assert!(zoom.hi - zoom.lo < full.hi - full.lo, "wheel still zooms");
    let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Middle, pressed, modifiers: Modifiers::NONE };
    g.frame_select(vec![button(p, true)], &mut range);
    assert_eq!(g.frame_select(vec![Event::PointerMoved(p + vec2(30.0, 0.0))], &mut range).1, None);
    let pan = g.ctx.data(|d| d.get_temp::<ViewRange>(Id::new("g"))).unwrap();
    assert!(pan.lo < zoom.lo, "middle drag still pans");
    g.frame_select(vec![button(p + vec2(30.0, 0.0), false)], &mut range);
    for _ in 0..2 {
        g.frame_select(vec![graph_selection_button(p, true)], &mut range);
        g.frame_select(vec![graph_selection_button(p, false)], &mut range);
    }
    let reset = g.ctx.data(|d| d.get_temp::<ViewRange>(Id::new("g"))).unwrap();
    assert_eq!((reset.lo, reset.hi), (full.lo, full.hi), "double click restores the full range");
    assert_eq!(range, Some((50, 75)), "navigation leaves the edit range unchanged");
    let p = g.at(1, 2000.0, 50.0);
    for _ in 0..2 { g.frame_select(vec![Event::PointerMoved(p)], &mut range); }
    assert_eq!(g.drawn.drew.hovered.map(|c| c.marker), Some(1), "multi-marker hover still identifies the curve");
}

#[test]
fn graph_selection_uses_actual_press_when_press_and_movement_share_an_input_frame() {
    use eframe::egui::{vec2, Event};
    let mut g = HoverGraph::new(true, false);
    let mut range = None;
    g.frame_select(vec![], &mut range);
    let (from, to) = (g.at(1, 1000.0, 50.0), g.at(1, 3000.0, 50.0));
    g.frame_select(vec![Event::PointerMoved(from)], &mut range);
    assert_eq!(g.frame_select(vec![graph_selection_button(from, true), Event::PointerMoved(to)], &mut range).1, None);
    assert_eq!(range, Some((1000, 3000)), "the press coordinate is the anchor, not the final pointer in the input batch");
    assert_eq!(g.frame_select(vec![graph_selection_button(to, false)], &mut range).1, None);

    // A movement out and back in one batch still crosses the threshold and suppresses a
    // click on release, even when egui's larger default threshold has not been crossed.
    let mut g = HoverGraph::new(true, false);
    let mut range = None;
    g.frame_select(vec![], &mut range);
    let from = g.at(1, 1000.0, 50.0);
    g.frame_select(vec![Event::PointerMoved(from)], &mut range);
    g.frame_select(vec![graph_selection_button(from, true)], &mut range);
    assert_eq!(g.frame_select(vec![Event::PointerMoved(from + vec2(5.0, 0.0)), Event::PointerMoved(from)], &mut range).1, None);
    assert_eq!(range, Some((1000, 1000)));
    assert_eq!(g.frame_select(vec![graph_selection_button(from, false)], &mut range).1, None);
}

#[test]
fn graph_selection_complete_drag_in_one_input_frame_is_ignored_when_egui_does_not_own_it() {
    use eframe::egui::Event;
    let mut g = HoverGraph::new(true, false);
    let mut range = None;
    g.frame_select(vec![], &mut range);
    let (from, to) = (g.at(1, 1000.0, 50.0), g.at(1, 3000.0, 50.0));
    g.frame_select(vec![Event::PointerMoved(from)], &mut range);
    assert_eq!(g.frame_select(vec![graph_selection_button(from, true), Event::PointerMoved(to), graph_selection_button(to, false)], &mut range).1, None);
    assert_eq!(range, None, "egui 0.29 does not recognize a complete same-frame drag; do not bypass its input ownership");
}

#[test]
fn graph_selection_ignores_prepress_motion_and_stops_at_release_before_later_motion() {
    use eframe::egui::{vec2, Event};
    let mut g = HoverGraph::new(true, false);
    let mut range = Some((50, 75));
    g.frame_select(vec![], &mut range);
    let p = g.at(1, 2000.0, 50.0);
    g.frame_select(vec![Event::PointerMoved(p)], &mut range);
    g.frame_select(vec![Event::PointerMoved(p - vec2(100.0, 0.0)), Event::PointerMoved(p), graph_selection_button(p, true)], &mut range);
    assert_eq!(range, Some((50, 75)), "movement before the press is not part of this gesture");
    assert_eq!(g.frame_select(vec![graph_selection_button(p, false)], &mut range).1, Some(2000), "the ordinary click remains a click");

    let mut g = HoverGraph::new(true, false);
    g.frame_select(vec![], &mut range);
    let (from, during, release, after) = (g.at(1, 1000.0, 50.0), g.at(1, 2500.0, 50.0), g.at(1, 2300.0, 50.0), g.at(1, 3000.0, 50.0));
    g.frame_select(vec![Event::PointerMoved(from)], &mut range);
    g.frame_select(vec![graph_selection_button(from, true)], &mut range);
    g.frame_select(vec![Event::PointerMoved(during)], &mut range);
    assert_eq!(g.frame_select(vec![graph_selection_button(release, false), Event::PointerMoved(after)], &mut range).1, None);
    assert_eq!(range, Some((1000, 2300)), "release position is final even when the pointer moves again in that input frame");
}

#[test]
fn graph_selection_does_not_receive_pointer_input_through_a_foreground_widget() {
    use eframe::egui::{vec2, Event, Rect};
    let mut g = HoverGraph::new(true, false);
    let mut range = Some((50, 75));
    g.frame_select(vec![], &mut range);
    let from = g.at(1, 2000.0, 50.0);
    let to = from + vec2(20.0, 0.0);
    let cover = Some(Rect::from_min_size(from - vec2(40.0, 25.0), vec2(120.0, 50.0)));
    for _ in 0..2 { g.frame_select_covered(vec![Event::PointerMoved(from)], &mut range, cover); }
    assert_eq!(g.frame_select_covered(vec![graph_selection_button(from, true)], &mut range, cover).1, None);
    assert_eq!(g.frame_select_covered(vec![Event::PointerMoved(to)], &mut range, cover).1, None);
    assert_eq!(g.frame_select_covered(vec![graph_selection_button(to, false)], &mut range, cover).1, None);
    assert_eq!(range, Some((50, 75)), "the foreground widget owns the drag");
}

#[test]
fn graph_selection_discards_a_drag_released_while_the_panel_is_hidden() {
    use eframe::egui::{vec2, Event, Pos2, RawInput, Rect};
    let mut g = HoverGraph::new(true, false);
    let mut range = Some((50, 75));
    g.frame_select(vec![], &mut range);
    let from = g.at(1, 2000.0, 50.0);
    g.frame_select(vec![Event::PointerMoved(from)], &mut range);
    g.frame_select(vec![graph_selection_button(from, true)], &mut range);
    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 400.0))),
        events: vec![graph_selection_button(from, false)],
        ..Default::default()
    };
    let _ = g.ctx.run(input, |_| {}); // The panel is hidden while the pointer is released.
    g.frame_select(vec![Event::PointerMoved(from + vec2(25.0, 0.0))], &mut range);
    assert_eq!(range, Some((50, 75)), "reopening cannot resume a released drag");
}
