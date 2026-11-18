use super::*;

#[test]
fn playback_working_range_rejects_invalid_number_entries() {
    let mut pb = Playback::new(100, 999, 100.0);
    pb.set_working_range(125, 750, 0.0);
    for text in ["", "a", "125.5", "1000000000000000000000", "99", "751"] {
        assert!(commit(Endpoint::Start, text, &mut pb, 0.0).is_some());
        assert_eq!(pb.working_range(), (125, 750));
    }
    for text in ["124", "1000"] {
        assert!(commit(Endpoint::End, text, &mut pb, 0.0).is_some());
        assert_eq!(pb.working_range(), (125, 750));
    }
    assert_eq!(commit(Endpoint::Start, " 750 ", &mut pb, 0.0), None);
    assert_eq!(pb.working_range(), (750, 750));
    assert_eq!(commit(Endpoint::End, "999", &mut pb, 0.0), None);
    assert_eq!(pb.working_range(), (750, 999));
}
