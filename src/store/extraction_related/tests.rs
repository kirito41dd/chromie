use super::*;
use crate::knowledge::{Link, MemoryType, Relation};

fn note(id: u128, content: &str) -> MemorySnapshot {
    MemorySnapshot {
        id: Uuid::from_u128(id),
        revision: 1,
        content: content.into(),
        memory_type: MemoryType::Note,
        project_key: None,
        links: vec![],
    }
}

#[test]
fn an_oversize_note_with_links_does_not_hide_later_fitting_notes() {
    let mut first = note(1, "虚构的大条目");
    let remaining = serde_json::to_vec(&first).unwrap().len();
    first.links.push(Link {
        memory_id: Uuid::from_u128(3),
        relation: Relation::DerivedFrom,
        revision: Some(2),
    });
    let second = note(2, "小");
    let expected = second.id;
    let (selected, partial) =
        select_within_budget(vec![first, second], 240 * 1024 - remaining, false).unwrap();
    assert!(partial);
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].id, expected);
}

#[test]
fn item_and_byte_limits_accept_the_exact_boundary() {
    let item = note(1, "虚构");
    let size = serde_json::to_vec(&item).unwrap().len();
    let (exact, partial) = select_within_budget(vec![item], 240 * 1024 - size, false).unwrap();
    assert_eq!(exact.len(), 1);
    assert!(!partial);
    for count in [15, 16] {
        let items = (0..count).map(|id| note(id, "虚构")).collect();
        let (selected, partial) = select_within_budget(items, 0, false).unwrap();
        assert_eq!(partial, count > 15);
        assert_eq!(selected.len(), 15);
        assert_eq!(selected.last().unwrap().id, Uuid::from_u128(14));
    }
}
