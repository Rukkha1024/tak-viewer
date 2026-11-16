
use crate::tak::MarkerStick;

pub const FULL_BODY_39: &str = "Conventional Full Body (39)";

const PURPLE: [u8; 3] = [140, 45, 225];
const SKY: [u8; 3] = [110, 210, 240];
const GREEN: [u8; 3] = [60, 210, 60];

/// (slot_a, slot_b, rgb) in file order.
pub const FULL_BODY_39_STICKS: [(u32, u32, [u8; 3]); 41] = [
    (12, 5, PURPLE), (13, 5, PURPLE), (12, 10, PURPLE), (13, 11, PURPLE), (12, 13, PURPLE),
    (5, 6, PURPLE), (6, 7, PURPLE), (8, 9, PURPLE),
    (14, 5, SKY), (14, 16, SKY), (14, 8, SKY), (16, 15, SKY), (15, 19, SKY), (19, 18, SKY),
    (18, 17, SKY), (18, 20, SKY),
    (21, 5, GREEN), (21, 23, GREEN), (21, 8, GREEN), (23, 22, GREEN), (22, 26, GREEN),
    (26, 25, GREEN), (25, 24, GREEN), (25, 27, GREEN),
    (3, 1, PURPLE), (3, 4, PURPLE), (4, 2, PURPLE),
    (28, 3, SKY), (28, 1, SKY), (28, 29, SKY), (29, 31, SKY), (31, 30, SKY), (30, 33, SKY),
    (33, 32, SKY),
    (34, 4, GREEN), (34, 2, GREEN), (34, 35, GREEN), (35, 37, GREEN), (37, 36, GREEN),
    (36, 39, GREEN), (39, 38, GREEN),
];

/// Standard sticks for a marker set name, or None when no standard table is known.
pub fn standard_sticks(markerset_name: Option<&str>) -> Option<Vec<MarkerStick>> {
    (markerset_name? == FULL_BODY_39).then(|| {
        FULL_BODY_39_STICKS
            .iter()
            .map(|&(a, b, c)| MarkerStick {
                slot_a: a,
                slot_b: b,
                rgb: [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0],
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_body_table_matches_file_values() {
        let s = standard_sticks(Some(FULL_BODY_39)).unwrap();
        assert_eq!(s.len(), 41);
        // Values as read from the files (float32).
        assert_eq!(s[0].rgb, [0.549_019_63_f32, 0.176_470_59, 0.882_352_96]);
        assert_eq!(s[8].rgb, [0.431_372_55_f32, 0.823_529_4, 0.941_176_5]);
        let slots: std::collections::BTreeSet<u32> = s.iter().flat_map(|k| [k.slot_a, k.slot_b]).collect();
        assert!(slots.iter().all(|&x| (1..=39).contains(&x)));
        assert!(standard_sticks(Some("Conventional Lower Body (16)")).is_none());
        assert!(standard_sticks(None).is_none());
    }
}
