//! Keeping the result list model in step with new results without rebuilding rows that stay.

use std::collections::HashSet;

use slint::{Model, VecModel};

use super::Item;

/// Where the previously selected result (by key) is in the new list; the top if it is gone.
pub(super) fn followed_index(selected: Option<String>, keys: &[String]) -> usize {
    selected.and_then(|s| keys.iter().position(|k| *k == s)).unwrap_or(0)
}

/// Bring `model` (showing `old`) to `rows` (keys `new`) with as few changes as possible:
/// rows that stay are updated in place, vanished ones removed, unknown ones inserted. Inserted
/// rows whose key was not in `old` at all are marked `fresh` when `mark_fresh` is set.
pub(super) fn sync_model(
    model: &VecModel<Item>,
    old: &mut Vec<String>,
    new: &[String],
    rows: Vec<Item>,
    mark_fresh: bool,
) {
    let before: HashSet<String> = old.iter().cloned().collect();
    for (i, (key, mut row)) in new.iter().zip(rows).enumerate() {
        if old.get(i) != Some(key) {
            if let Some(j) = old.iter().skip(i + 1).position(|k| k == key).map(|p| p + i + 1) {
                // Rows between were dropped from the list.
                for _ in i..j {
                    model.remove(i);
                    old.remove(i);
                }
            } else {
                row.fresh = mark_fresh && !before.contains(key);
                model.insert(i, row);
                old.insert(i, key.clone());
                continue;
            }
        }
        row.fresh = false;
        if model.row_data(i).as_ref() != Some(&row) {
            model.set_row_data(i, row);
        }
    }
    while old.len() > new.len() {
        model.remove(new.len());
        old.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::{followed_index, sync_model};
    use crate::ui::Item;
    use slint::{Model, VecModel};

    fn row(k: &str) -> Item {
        Item { title: k.into(), ..Default::default() }
    }

    /// Apply `new` to a model showing `old`; returns the titles and which rows are fresh.
    fn sync(old: &[&str], new: &[&str]) -> (Vec<String>, Vec<bool>) {
        let model = VecModel::from(old.iter().map(|k| row(k)).collect::<Vec<_>>());
        let mut keys: Vec<String> = old.iter().map(std::string::ToString::to_string).collect();
        let new_keys: Vec<String> = new.iter().map(std::string::ToString::to_string).collect();
        sync_model(&model, &mut keys, &new_keys, new.iter().map(|k| row(k)).collect(), true);
        assert_eq!(keys, new_keys, "mirror of the model keys");
        let rows: Vec<Item> = model.iter().collect();
        (rows.iter().map(|r| r.title.to_string()).collect(), rows.iter().map(|r| r.fresh).collect())
    }

    #[test]
    fn model_is_updated_in_place_and_only_new_rows_are_fresh() {
        assert_eq!(
            sync(&["a", "b"], &["a", "b", "c"]),
            (vec!["a".into(), "b".into(), "c".into()], vec![false, false, true])
        );
        // A late result ranks first: it is inserted and fades in, the others just shift.
        assert_eq!(
            sync(&["a", "b"], &["x", "a", "b"]),
            (vec!["x".into(), "a".into(), "b".into()], vec![true, false, false])
        );
        // Rows removed from the top or the middle.
        assert_eq!(sync(&["a", "b", "c"], &["c"]).0, ["c"]);
        assert_eq!(sync(&["a", "b", "c"], &["a", "c"]).0, ["a", "c"]);
        assert_eq!(sync(&["a", "b"], &[]).0, Vec::<String>::new());
        // A reorder of known rows never fades.
        let (titles, fresh) = sync(&["a", "b", "c"], &["c", "a", "b"]);
        assert_eq!(titles, ["c", "a", "b"]);
        assert!(fresh.iter().all(|f| !f));
    }

    #[test]
    fn selection_follows_its_result_when_the_list_reorders() {
        let keys: Vec<String> = ["a:1", "b:2", "c:3"].map(String::from).to_vec();
        assert_eq!(followed_index(Some("a:1".into()), &keys), 0);
        assert_eq!(followed_index(Some("c:3".into()), &keys), 2);
        assert_eq!(followed_index(Some("gone:9".into()), &keys), 0);
        assert_eq!(followed_index(None, &keys), 0);
    }
}
