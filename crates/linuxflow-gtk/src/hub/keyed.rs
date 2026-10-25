//! Keyed list: updates a row container by difference (insertion, removal,
//! move, replacement of only the modified rows) instead of rebuilding everything. This way an
//! `Event::State` loses neither the focus nor the selection of unchanged rows.

use gtk::prelude::*;
use gtk4 as gtk;

/// Operation to apply to the widget container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op<W> {
    Remove(W),
    /// Inserts `widget` right after `after` (`None` = at the head).
    Insert {
        widget: W,
        after: Option<W>,
    },
    /// Moves `widget` right after `after` (`None` = at the head).
    Move {
        widget: W,
        after: Option<W>,
    },
}

struct Entry<W> {
    key: String,
    sig: String,
    widget: W,
}

/// Pure model of the list: stable key + content signature per row.
pub struct KeyedVec<W> {
    entries: Vec<Entry<W>>,
}

impl<W> Default for KeyedVec<W> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<W: Clone> KeyedVec<W> {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Keys in the current order.
    pub fn keys(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.key.clone()).collect()
    }

    /// Aligns the list with `wanted` (key, signature). `build(i)` builds the widget of item
    /// `i` of `wanted`; `apply` applies each operation to the real container.
    pub fn sync(
        &mut self,
        wanted: &[(String, String)],
        build: &mut dyn FnMut(usize) -> W,
        apply: &mut dyn FnMut(Op<W>),
    ) {
        // 1. Removes the rows whose key has disappeared.
        let mut kept: Vec<Entry<W>> = Vec::with_capacity(self.entries.len());
        for e in self.entries.drain(..) {
            if wanted.iter().any(|(k, _)| *k == e.key) {
                kept.push(e);
            } else {
                apply(Op::Remove(e.widget));
            }
        }
        self.entries = kept;

        // 2. Walks the wanted order: moves, replaces or inserts.
        for (i, (key, sig)) in wanted.iter().enumerate() {
            let after = if i == 0 {
                None
            } else {
                Some(self.entries[i - 1].widget.clone())
            };
            match self
                .entries
                .iter()
                .skip(i)
                .position(|e| e.key == *key)
                .map(|p| p + i)
            {
                Some(j) => {
                    if j != i {
                        let e = self.entries.remove(j);
                        apply(Op::Move {
                            widget: e.widget.clone(),
                            after,
                        });
                        self.entries.insert(i, e);
                    }
                    if self.entries[i].sig != *sig {
                        let old = self.entries[i].widget.clone();
                        let widget = build(i);
                        apply(Op::Remove(old));
                        let after = if i == 0 {
                            None
                        } else {
                            Some(self.entries[i - 1].widget.clone())
                        };
                        apply(Op::Insert {
                            widget: widget.clone(),
                            after,
                        });
                        self.entries[i] = Entry {
                            key: key.clone(),
                            sig: sig.clone(),
                            widget,
                        };
                    }
                }
                None => {
                    let widget = build(i);
                    apply(Op::Insert {
                        widget: widget.clone(),
                        after,
                    });
                    self.entries.insert(
                        i,
                        Entry {
                            key: key.clone(),
                            sig: sig.clone(),
                            widget,
                        },
                    );
                }
            }
        }
    }
}

/// GTK container (vertical box) managed by difference.
pub struct KeyedBox {
    container: gtk::Box,
    model: std::cell::RefCell<KeyedVec<gtk::Widget>>,
}

impl KeyedBox {
    pub fn new(spacing: i32) -> Self {
        let container = gtk::Box::new(gtk::Orientation::Vertical, spacing);
        Self {
            container,
            model: Default::default(),
        }
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.container
    }

    pub fn len(&self) -> usize {
        self.model.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.model.borrow().is_empty()
    }

    pub fn keys(&self) -> Vec<String> {
        self.model.borrow().keys()
    }

    pub fn sync(&self, wanted: &[(String, String)], build: &mut dyn FnMut(usize) -> gtk::Widget) {
        let container = self.container.clone();
        self.model
            .borrow_mut()
            .sync(wanted, build, &mut |op| match op {
                Op::Remove(w) => container.remove(&w),
                Op::Insert { widget, after } => {
                    container.insert_child_after(&widget, after.as_ref())
                }
                Op::Move { widget, after } => {
                    container.reorder_child_after(&widget, after.as_ref())
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulates the container: an ordered list of widgets (strings here).
    fn apply_all(container: &mut Vec<String>, ops: Vec<Op<String>>) {
        for op in ops {
            match op {
                Op::Remove(w) => container.retain(|x| *x != w),
                Op::Insert { widget, after } => {
                    let pos = after.map_or(0, |a| {
                        container.iter().position(|x| *x == a).map_or(0, |p| p + 1)
                    });
                    container.insert(pos, widget);
                }
                Op::Move { widget, after } => {
                    container.retain(|x| *x != widget);
                    let pos = after.map_or(0, |a| {
                        container.iter().position(|x| *x == a).map_or(0, |p| p + 1)
                    });
                    container.insert(pos, widget);
                }
            }
        }
    }

    fn run(
        model: &mut KeyedVec<String>,
        container: &mut Vec<String>,
        wanted: &[(&str, &str)],
        built: &mut Vec<String>,
    ) {
        let wanted: Vec<(String, String)> = wanted
            .iter()
            .map(|(k, s)| (k.to_string(), s.to_string()))
            .collect();
        let mut ops = Vec::new();
        let w2 = wanted.clone();
        let mut counter = built.len();
        let mut new_built = Vec::new();
        model.sync(
            &wanted,
            &mut |i| {
                counter += 1;
                let name = format!("{}#{}", w2[i].0, counter);
                new_built.push(name.clone());
                name
            },
            &mut |op| ops.push(op),
        );
        built.extend(new_built);
        apply_all(container, ops);
        assert_eq!(
            model.keys(),
            wanted.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>()
        );
        let names: Vec<String> = container
            .iter()
            .map(|w| w.split('#').next().unwrap_or("").to_string())
            .collect();
        assert_eq!(names, model.keys(), "container order != model order");
    }

    #[test]
    fn untouched_rows_keep_their_widget() {
        let (mut m, mut c, mut built) = (KeyedVec::default(), Vec::new(), Vec::new());
        run(
            &mut m,
            &mut c,
            &[("a", "1"), ("b", "1"), ("c", "1")],
            &mut built,
        );
        assert_eq!(built.len(), 3);
        let before = c.clone();
        run(
            &mut m,
            &mut c,
            &[("a", "1"), ("b", "1"), ("c", "1")],
            &mut built,
        );
        assert_eq!(built.len(), 3, "nothing must be rebuilt");
        assert_eq!(c, before);
    }

    #[test]
    fn insert_remove_replace_and_move() {
        let (mut m, mut c, mut built) = (KeyedVec::default(), Vec::new(), Vec::new());
        run(
            &mut m,
            &mut c,
            &[("a", "1"), ("b", "1"), ("c", "1")],
            &mut built,
        );
        let widget_b = c[1].clone();
        // New row at the head, "a" removed, "c" modified.
        run(
            &mut m,
            &mut c,
            &[("n", "1"), ("b", "1"), ("c", "2")],
            &mut built,
        );
        assert_eq!(built.len(), 5, "n et c reconstruits seulement");
        assert_eq!(c[1], widget_b, "b kept");
        // Reordering: b and c swapped, widgets kept.
        let n_built = built.len();
        run(
            &mut m,
            &mut c,
            &[("n", "1"), ("c", "2"), ("b", "1")],
            &mut built,
        );
        assert_eq!(built.len(), n_built);
        assert_eq!(c[2], widget_b);
        // Vidage.
        run(&mut m, &mut c, &[], &mut built);
        assert!(c.is_empty() && m.is_empty());
    }

    #[test]
    fn many_random_transitions_stay_consistent() {
        let (mut m, mut c, mut built) = (KeyedVec::default(), Vec::new(), Vec::new());
        let seqs: [&[(&str, &str)]; 5] = [
            &[("a", "1"), ("b", "1")],
            &[("c", "1"), ("b", "2"), ("a", "1")],
            &[("b", "2")],
            &[("d", "1"), ("e", "1"), ("b", "2"), ("a", "9")],
            &[("e", "1"), ("d", "1")],
        ];
        for s in seqs {
            run(&mut m, &mut c, s, &mut built);
        }
    }
}
