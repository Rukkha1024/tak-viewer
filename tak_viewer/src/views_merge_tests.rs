#[test]
fn merged_shared_comment_deletion_prunes_only_that_views_selection() {
    let mut owner = selection_view();
    for id in 1..=3 {
        let comment = comments::Comment { id, markers: vec![["A", "B", "C"][id as usize - 1].into()], start: 0, end: 1, comment_type: String::new(), text: format!("row {id}") };
        owner.comments.list.apply(&comments::Event::of(comments::Kind::Add, &comment, String::new(), String::new()));
    }
    let shared = owner.shared_file().unwrap();
    let mut other = View::default();
    other.show_shared_file(shared, 999, 1.0, 0.0);
    other.comment_selection.all(&[3, 2, 1]);
    let selection = other.comment_selection.clone();
    other.select_comment(3);
    other.comment_selection = selection;
    assert_eq!(other.primary, Some(2));
    let deleted = owner.comments.list.get(3).unwrap().clone();
    owner.comments.list.apply(&comments::Event::of(comments::Kind::Delete, &deleted, String::new(), String::new()));
    assert!(owner.publish_shared_file());
    assert!(other.sync_shared());
    assert_eq!(other.comment_selection.ids().collect::<Vec<_>>(), vec![1, 2]);
    assert_eq!(other.picked, Some(2), "equal-recency fallback keeps this view's sorted order");
    assert_eq!(other.primary, Some(1), "markers follow the surviving representative");
    assert!(owner.comment_selection.ids().next().is_none(), "view selections are independent");
}

#[test]
fn merged_shared_marker_refresh_keeps_both_uid_range_anchors() {
    let mut owner = selection_view();
    let shared = owner.shared_file().unwrap();
    let mut other = View::default();
    other.show_shared_file(shared, 999, 1.0, 0.0);
    other.labels.list_selection[0].anchor = Some([1; 16]);
    other.labels.list_selection[1].anchor = Some([3; 16]);
    let revision = owner.edit_document.as_ref().unwrap().lock().unwrap().revision();
    let mut take = sample_take("selection.tak", &["A", "B", "C", "D"]);
    take.trajectories.rotate_left(1);
    let mut updated = ViewData::build(take);
    updated.edit_revision = Some(revision);
    owner.edit_refresh = Some(revision);
    owner.publish_edited_view(updated).unwrap();
    assert!(other.sync_shared());
    assert_eq!(other.labels.list_selection[0].anchor, Some([1; 16]));
    assert_eq!(other.labels.list_selection[1].anchor, Some([3; 16]));
}
