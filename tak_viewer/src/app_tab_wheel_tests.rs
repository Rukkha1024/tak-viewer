use super::*;
use egui::{Event, MouseWheelUnit, Pos2, Rect};

struct Fixture {
    ctx: egui::Context,
    app: TakViewerApp,
    frame_number: u32,
}

impl Fixture {
    fn new(count: usize) -> Self {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let first = app.layout_tabs.active_id().unwrap();
        for _ in 1..count {
            app.layout_tabs.add(None);
        }
        app.layout_tabs.activate(first);
        let (_tx, rx) = std::sync::mpsc::channel();
        app.views[0].show(
            views::sample_data("synthetic.tak", &["M"]),
            rx,
            10,
            1.0,
            0.0,
        );
        app.set_graph_float(false);
        let mut test = Self {
            ctx,
            app,
            frame_number: 0,
        };
        test.settle(5);
        test
    }

    fn frame(&mut self, events: Vec<Event>) -> egui::FullOutput {
        self.frame_number += 1;
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1050.0, 720.0))),
            time: Some(f64::from(self.frame_number) / 60.0),
            focused: true,
            events,
            ..Default::default()
        };
        eframe::App::raw_input_hook(&mut self.app, &self.ctx, &mut raw);
        self.ctx.run(raw, |ctx| self.app.frame(ctx))
    }

    fn settle(&mut self, frames: usize) {
        for _ in 0..frames {
            self.frame(vec![]);
        }
    }

    fn center(&self) -> Pos2 {
        self.app.layout_strip.wheel.center().unwrap()
    }

    fn offset(&self) -> f32 {
        self.app.layout_strip.wheel.offset()
    }

    fn wheel(&mut self, at: Pos2, x: f32, y: f32, unit: MouseWheelUnit, modifiers: Modifiers) {
        self.frame(vec![
            Event::PointerMoved(at),
            Event::MouseWheel {
                unit,
                delta: egui::vec2(x, y),
                modifiers,
            },
        ]);
        self.settle(24);
    }

    fn notch(&mut self, y: f32) {
        self.wheel(self.center(), 0.0, y, MouseWheelUnit::Line, Modifiers::NONE);
    }
}

#[test]
fn vertical_horizontal_and_shift_wheel_scroll_clamp_and_preserve_active_view() {
    let mut test = Fixture::new(30);
    let active = test.app.layout_tabs.active_id();
    let cam = test.app.views[0].cam;
    let frame = test.app.views[0].pb.frame;
    let history = test.app.history.kinds();
    test.notch(1.0);
    assert_eq!(test.offset(), 0.0);
    test.notch(-1.0);
    assert!(
        (test.offset() - 120.0).abs() < 0.01,
        "offset {}",
        test.offset()
    );
    test.wheel(
        test.center(),
        -26.0,
        -8.0,
        MouseWheelUnit::Point,
        Modifiers::NONE,
    );
    assert!((test.offset() - 146.0).abs() < 0.01);
    test.wheel(
        test.center(),
        0.0,
        -1.0,
        MouseWheelUnit::Line,
        Modifiers::SHIFT,
    );
    assert!((test.offset() - 266.0).abs() < 0.01);
    test.notch(-100.0);
    let end = test.offset();
    assert!(end > 2000.0);
    test.notch(-1.0);
    assert_eq!(test.offset(), end);
    test.notch(1.0);
    assert!((test.offset() - (end - 120.0)).abs() < 0.01);
    test.notch(100.0);
    assert_eq!(test.offset(), 0.0);
    assert_eq!(test.app.layout_tabs.active_id(), active);
    assert_eq!(test.app.layout_tabs.tabs().len(), 30);
    assert_eq!(test.app.views[0].cam, cam);
    assert_eq!(test.app.views[0].pb.frame, frame);
    assert_eq!(test.app.history.kinds(), history);
}

#[test]
fn wheel_stays_in_its_region_and_short_strip_still_consumes_it() {
    let mut test = Fixture::new(30);
    test.notch(-2.0);
    let offset = test.offset();
    let scene = test.app.views[0].scene_rect.center();
    let before = test.app.views[0].cam.distance;
    test.wheel(scene, 0.0, 50.0, MouseWheelUnit::Point, Modifiers::NONE);
    assert!(test.app.views[0].cam.distance < before);
    assert_eq!(test.offset(), offset);
    let (_, graph_node, _) = test.app.dock.find_tab(&Pane::Graph).unwrap();
    let graph = test.app.dock.main_surface()[graph_node]
        .rect()
        .unwrap()
        .center();
    test.wheel(graph, 0.0, 50.0, MouseWheelUnit::Point, Modifiers::NONE);
    assert_eq!(test.offset(), offset);

    let mut single = Fixture::new(1);
    let scene = single.app.views[0].scene_rect.center();
    let camera = single.app.views[0].cam;
    let mut raw = egui::RawInput {
        events: vec![
            Event::PointerMoved(single.center()),
            Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: egui::vec2(0.0, -3.0),
                modifiers: Modifiers::NONE,
            },
            Event::PointerMoved(scene),
        ],
        ..Default::default()
    };
    eframe::App::raw_input_hook(&mut single.app, &single.ctx, &mut raw);
    assert_eq!(
        raw.events.len(),
        2,
        "consume the wheel even if the pointer moves to 3D in the same input batch"
    );
    single.frame(raw.events);
    single.notch(-1.0);
    assert_eq!(single.offset(), 0.0);
    assert_eq!(single.app.views[0].cam, camera);
}

#[test]
fn wheel_keeps_rename_draft_and_switch_still_reveals_far_tab() {
    let mut test = Fixture::new(30);
    let first = test.app.layout_tabs.active_id().unwrap();
    assert!(test
        .app
        .layout_strip
        .begin_rename(&test.app.layout_tabs, first));
    test.frame(vec![]);
    test.frame(vec![Event::Text("draft name".into())]);
    test.notch(-15.0);
    assert!(test.offset() > 1000.0);
    assert!(test.app.layout_strip.is_renaming());
    assert_eq!(test.app.layout_tabs.active_id(), Some(first));
    assert_eq!(test.app.layout_tabs.tab(first).unwrap().name, "tab1");
    test.notch(100.0);
    test.frame(vec![Event::Key {
        key: Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }]);
    assert_eq!(test.app.layout_tabs.tab(first).unwrap().name, "draft name");
    assert_eq!(test.offset(), 0.0);

    let last = test.app.layout_tabs.tabs().last().unwrap().id;
    test.frame(vec![Event::MouseWheel {
        unit: MouseWheelUnit::Line,
        delta: egui::vec2(0.0, -1.0),
        modifiers: Modifiers::NONE,
    }]);
    assert!(test.app.switch_layout_tab_internal(last));
    test.settle(24);
    let picture = test.frame(vec![]);
    assert!(test.offset() > 2000.0);
    assert!(
        picture.shapes.iter().any(|shape| {
            let egui::Shape::Text(text) = &shape.shape else {
                return false;
            };
            text.galley.text() == "tab30"
                && shape.clip_rect.contains_rect(text.visual_bounding_rect())
        }),
        "the active tab name should be visible after switching"
    );
}
