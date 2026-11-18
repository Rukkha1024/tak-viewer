fn os_drop_input(ctx: &egui::Context, app: &mut TakViewerApp, position: Option<egui::Pos2>, paths: &[PathBuf], drop: bool) -> egui::FullOutput {
    let mut raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 960.0))),
        focused: true,
        ..Default::default()
    };
    if let Some(p) = position { raw.events.push(egui::Event::PointerMoved(p)); }
    if drop { raw.dropped_files = paths.iter().map(|p| egui::DroppedFile { path: Some(p.clone()), ..Default::default() }).collect(); }
    else { raw.hovered_files = paths.iter().map(|p| egui::HoveredFile { path: Some(p.clone()), ..Default::default() }).collect(); }
    eframe::App::raw_input_hook(app, ctx, &mut raw);
    ctx.run(raw, |ctx| app.frame(ctx))
}

#[test]
fn desktop_raw_input_edge_batch_is_equal_split_and_one_undo_action() {
    use dock::DropZone::*;
    for zone in [Left, Right, Above, Below] {
        for n in [1, 2, 3] {
            let ctx = egui::Context::default();
            let mut app = one_view(&ctx);
            app.hold_loads = true;
            frame_with(&ctx, &mut app, vec![]);
            let r = place_of(&app, Pane::View(0));
            let p = match zone {
                Left => egui::pos2(r.left()+3.0,r.center().y), Right => egui::pos2(r.right()-3.0,r.center().y),
                Above => egui::pos2(r.center().x,r.top()+3.0), Below => egui::pos2(r.center().x,r.bottom()-3.0), _ => unreachable!(),
            };
            let paths: Vec<_> = (0..n).map(|i| PathBuf::from(format!("no_such_dir_tak_viewer_test/drop_{i}.takm"))).collect();
            let pushed = app.history.pushed;
            os_drop_input(&ctx,&mut app,Some(p),&paths,false);
            assert_eq!(app.open_views().len(),1,"hover never opens a file");
            // No pointer movement on Drop (Windows OLE behavior); use tracked position.
            os_drop_input(&ctx,&mut app,None,&paths,true);
            assert_eq!(app.open_views().len(),1+n);
            assert_eq!(app.history.pushed,pushed+1,"one batch, one action");
            let rects = dock::view_rects(&app.dock,dock::NOMINAL_AREA);
            let rect = |slot| rects.iter().find(|(s,_)| *s == slot).unwrap().1;
            let areas: Vec<_> = (1..=n).map(|slot| rect(slot as u8).area()).collect();
            assert!(areas.iter().all(|a| (*a-areas[0]).abs()<1.0),"equal cells: {rects:?}");
            for (i,path) in paths.iter().enumerate() {
                assert_eq!(app.views[i+1].target(),Some(path.as_path()));
                let added = rect((i+1) as u8); let original = rect(0);
                assert!(match zone { Left=>added.right()<=original.left()+0.5,Right=>added.left()>=original.right()-0.5,Above=>added.bottom()<=original.top()+0.5,Below=>added.top()>=original.bottom()-0.5,_=>false });
            }
            app.undo_redo(&ctx,false);
            assert_eq!(app.open_views(),vec![0]);
            app.undo_redo(&ctx,true);
            assert_eq!(app.open_views().len(),1+n);
        }
    }
}

#[test]
fn desktop_strip_creates_one_tab_and_whole_batch_undo_redo() {
    let ctx = egui::Context::default(); let mut app = one_view(&ctx); app.hold_loads=true;
    frame_with(&ctx,&mut app,vec![]);
    let before=app.layout_tabs.active_id(); let tabs=app.layout_tabs.tabs().len(); let pushed=app.history.pushed;
    let p=dock::layout_strip_rect(&app.dock).unwrap().center();
    let paths=vec![PathBuf::from("drop_a.TAK"),PathBuf::from("drop_b.TaKm")];
    os_drop_input(&ctx,&mut app,Some(p),&paths,false);
    os_drop_input(&ctx,&mut app,None,&paths,true);
    assert_eq!(app.layout_tabs.tabs().len(),tabs+1); assert_eq!(app.open_views(),vec![0,1]);
    assert_eq!(app.history.pushed,pushed+1);
    app.undo_redo(&ctx,false); assert_eq!(app.layout_tabs.active_id(),before); assert_eq!(app.layout_tabs.tabs().len(),tabs);
    app.undo_redo(&ctx,true); assert_eq!(app.layout_tabs.tabs().len(),tabs+1); assert_eq!(app.open_views(),vec![0,1]);
    assert_eq!(app.views[1].target(),Some(paths[1].as_path()));
}

#[test]
fn desktop_center_refuses_batch_and_single_replacement_obeys_dirty_gate() {
    let ctx=egui::Context::default(); let mut app=one_view(&ctx); app.hold_loads=true;
    let _dir=comment_dir("desktop_center_undo");
    let fixtures=take_files(&_dir,&["before.tak","drop_a.tak","drop_b.takm"]);
    let (_,rx)=std::sync::mpsc::channel();
    app.views[0].show(views::sample_data(&fixtures[0].to_string_lossy(), &["M","N"]),rx,11,1.0,0.0);
    app.observe(false);
    frame_with(&ctx,&mut app,vec![]); let p=app.views[0].scene_rect.center(); let before=fixtures[0].clone();
    let paths=fixtures[1..].to_vec(); let pushed=app.history.pushed;
    os_drop_input(&ctx,&mut app,Some(p),&paths,true);
    assert_eq!(app.views[0].target(),Some(before.as_path())); assert_eq!(app.history.pushed,pushed); assert!(app.notice.as_ref().unwrap().contains("가장자리"));
    app.notice=None;
    os_drop_input(&ctx,&mut app,Some(p),&paths[..1],true);
    assert_eq!(app.open_views(),vec![0]); assert_eq!(app.views[0].target(),Some(paths[0].as_path())); assert_eq!(app.history.pushed,pushed+1);
    app.undo_redo(&ctx,false); assert_eq!(app.views[0].target(),Some(before.as_path()));
}

#[test]
fn desktop_full_layout_refuses_edge_atomically_but_center_still_works() {
    use crate::desktop_drop::Target;
    let ctx=egui::Context::default(); let mut app=one_view(&ctx); app.hold_loads=true;
    for i in 1..4 { app.open_side(&ctx,PathBuf::from(format!("full_{i}.tak"))); }
    frame_with(&ctx,&mut app,vec![]); let r=place_of(&app,Pane::View(0)); let p=egui::pos2(r.right()-2.0,r.center().y);
    let path=vec![PathBuf::from("fifth.takm")]; let pushed=app.history.pushed;
    assert_eq!(app.desktop_refusal(Target::View(0,dock::DropZone::Right),&path,None),Some(desktop_drop::Refusal::Views));
    os_drop_input(&ctx,&mut app,Some(p),&path,true);
    assert_eq!(app.open_views().len(),4); assert_eq!(app.history.pushed,pushed); assert!(app.notice.is_some());
    app.notice=None; let center=app.views[0].scene_rect.center();
    os_drop_input(&ctx,&mut app,Some(center),&path,true);
    assert_eq!(app.views[0].target(),Some(path[0].as_path())); assert_eq!(app.open_views().len(),4);
}

#[test]
fn desktop_file_cap_counts_hidden_tabs_and_pending_and_refuses_entire_batch() {
    use crate::desktop_drop::Target;
    let ctx=egui::Context::default(); let mut app=one_view(&ctx); app.hold_loads=true;
    for i in 1..MAX_OPEN_FILES - 1 {
        if i%MAX_VIEWS==0 { app.new_layout_tab(); }
        app.open_side(&ctx,PathBuf::from(format!("cap_{i}.tak")));
    }
    let last = vec![PathBuf::from("source_30.tak")];
    assert_eq!(app.desktop_refusal(Target::NewTab, &last, None), None);
    app.desktop_drop_files(&ctx, Target::NewTab, last);
    assert_eq!(app.open_file_identities(None).len(), MAX_OPEN_FILES);
    frame_with(&ctx,&mut app,vec![]); let count=app.layout_tabs.tabs().len(); let pushed=app.history.pushed;
    let paths=vec![PathBuf::from("source_31.tak"),PathBuf::from("source_32.takm")];
    assert_eq!(app.desktop_refusal(Target::NewTab,&paths[..1],None),Some(desktop_drop::Refusal::Files));
    assert_eq!(app.desktop_refusal(Target::NewTab,&paths,None),Some(desktop_drop::Refusal::Files));
    let file_id = app.next_file_id;
    app.desktop_drop_files(&ctx,Target::NewTab,paths);
    assert_eq!(app.next_file_id, file_id);
    assert_eq!(app.layout_tabs.tabs().len(),count); assert_eq!(app.history.pushed,pushed); assert!(app.notice.as_ref().unwrap().contains("30개까지"));
    let repeated=vec![app.views[app.active].target().unwrap().to_path_buf()];
    assert_eq!(app.desktop_refusal(Target::NewTab,&repeated,None),None);
    app.notice=None; app.desktop_drop_files(&ctx,Target::NewTab,repeated);
    assert_eq!(app.layout_tabs.tabs().len(),count+1,"same document in another tab is allowed");
}

#[test]
fn desktop_ignores_other_files_and_hover_cancel_clears_preview() {
    let ctx=egui::Context::default(); let mut app=one_view(&ctx); app.hold_loads=true;
    frame_with(&ctx,&mut app,vec![]); let r=place_of(&app,Pane::View(0)); let p=egui::pos2(r.right()-2.0,r.center().y);
    let path=vec![PathBuf::from("hover.tak")];
    os_drop_input(&ctx,&mut app,Some(p),&path,false); assert!(!app.desktop_hover.paths.is_empty());
    os_drop_input(&ctx,&mut app,None,&[],false); assert!(app.desktop_hover.paths.is_empty()); assert!(app.desktop_hover.position.is_none());
    let old=app.views[0].target().unwrap().to_path_buf(); let pushed=app.history.pushed;
    os_drop_input(&ctx,&mut app,Some(p),&[PathBuf::from("notes.txt")],true);
    assert_eq!(app.views[0].target(),Some(old.as_path())); assert_eq!(app.history.pushed,pushed);
}

#[test]
fn desktop_folder_drop_keeps_workspace_and_non_take_files_do_not_consume_a_view() {
    let ctx=egui::Context::default(); let mut app=one_view(&ctx); app.hold_loads=true;
    let _dir=comment_dir("desktop_drop_folder"); let dir=_dir.to_path_buf();
    frame_with(&ctx,&mut app,vec![]); let p=app.views[0].scene_rect.center();
    let incoming=dir.join("chosen.TAKM");
    os_drop_input(&ctx,&mut app,Some(p),&[dir.clone(),dir.join("ignored.txt"),incoming.clone()],true);
    assert!(app.workspace.roots().contains(&dir));
    assert_eq!(app.views[0].target(),Some(incoming.as_path())); assert_eq!(app.open_views(),vec![0]);
}

#[test]
fn desktop_center_dirty_last_reference_cancel_preserves_edits_and_file() {
    let ctx=egui::Context::default(); let _dir=comment_dir("desktop_drop_dirty");
    let source=_dir.join("source.takm"); make_takm(&source);
    let mut app=TakViewerApp::build(&ctx,None,vec![],None,None);
    app.open(&ctx,source.clone());
    close_wait_until(&ctx,&mut app,|a| a.views[0].data.is_some() && !a.views[0].busy());
    app.views[0].select(0,false); app.views[0].pb.frame=10; app.delete_selected(&ctx,0);
    close_wait_until(&ctx,&mut app,|a| !a.marker_job_running(0) && a.views[0].edit_refresh.is_none());
    assert!(app.views[0].marker_dirty());
    let p=app.views[0].scene_rect.center(); let destination=_dir.join("replacement.takm"); let pushed=app.history.pushed;
    os_drop_input(&ctx,&mut app,Some(p),&[destination],true);
    assert!(app.dirty_prompt.is_some()); assert_eq!(app.views[0].target(),Some(source.as_path()));
    let out=frame_with(&ctx,&mut app,vec![]); let cancel=text_rects(&out,"Cancel")[0].center();
    click_at(&ctx,&mut app,cancel);
    assert!(app.dirty_prompt.is_none()); assert!(app.views[0].marker_dirty()); assert_eq!(app.views[0].target(),Some(source.as_path())); assert_eq!(app.history.pushed,pushed);
}
