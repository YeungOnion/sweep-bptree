use std::{borrow::Cow, cmp::Ordering};

use crate::Key;

use super::{Argument, SearchArgument};

/// Argument to count the number of groups in a set of keys
/// Note, the group must be ordered
/// This Argument basicly provides two capabilities:
/// 1. Get the group count
/// 2. Query inside group by offset
#[derive(Clone, Copy, Debug)]
pub enum GroupCount<G> {
    Zero,
    One(G, usize),
    Multiple {
        min_group: (G, usize),
        max_group: (G, usize),
        /// how many groups for child
        group_count: usize,
    },
}

impl<G: Clone + Ord + std::fmt::Debug> Default for GroupCount<G> {
    fn default() -> Self {
        Self::Zero
    }
}

impl<G: Clone + Ord + std::fmt::Debug> GroupCount<G> {
    pub fn is_zero(&self) -> bool {
        matches!(self, GroupCount::Zero)
    }

    pub fn group_count(&self) -> usize {
        match self {
            GroupCount::Zero => 0,
            GroupCount::One(_, _) => 1,
            GroupCount::Multiple { group_count, .. } => *group_count,
        }
    }

    /// merge self with right
    fn merge_with(&mut self, right: &Self) {
        let new_value = match (std::mem::take(self), right) {
            (GroupCount::Zero, r) => r.clone(),
            (l, GroupCount::Zero) => l,

            (GroupCount::One(g1, c1), GroupCount::One(g2, c2)) => {
                if g1.cmp(g2) == Ordering::Equal {
                    GroupCount::One(g1.clone(), c1 + *c2)
                } else {
                    GroupCount::Multiple {
                        min_group: (g1.clone(), c1),
                        max_group: (g2.clone(), *c2),
                        group_count: 2,
                    }
                }
            }
            (
                GroupCount::One(g1, c1),
                GroupCount::Multiple {
                    min_group,
                    max_group,
                    group_count,
                },
            ) => {
                if g1.cmp(&min_group.0) == Ordering::Equal {
                    GroupCount::Multiple {
                        min_group: (g1, c1 + min_group.1),
                        max_group: max_group.clone(),
                        group_count: *group_count,
                    }
                } else {
                    GroupCount::Multiple {
                        min_group: (g1, c1),
                        max_group: max_group.clone(),
                        group_count: *group_count + 1,
                    }
                }
            }
            (
                GroupCount::Multiple {
                    min_group,
                    mut max_group,
                    mut group_count,
                },
                GroupCount::One(ref g2, c2),
            ) => {
                if g2.cmp(&max_group.0) == Ordering::Equal {
                    max_group.1 = max_group.1 + c2;
                } else {
                    max_group = (g2.clone(), *c2);
                    group_count += 1;
                }

                GroupCount::Multiple {
                    min_group,
                    max_group,
                    group_count,
                }
            }
            (
                GroupCount::Multiple {
                    min_group: left_min_group,
                    max_group: left_max_group,
                    group_count: left_group_count,
                },
                GroupCount::Multiple {
                    min_group: right_min_group,
                    max_group: rigth_max_group,
                    group_count: right_group_count,
                },
            ) => {
                let mut new_group_count = left_group_count + right_group_count;

                if left_max_group.0.cmp(&right_min_group.0) == Ordering::Equal {
                    new_group_count -= 1;
                }

                GroupCount::Multiple {
                    min_group: left_min_group,
                    max_group: rigth_max_group.clone(),
                    group_count: new_group_count,
                }
            }
        };

        *self = new_value;
    }
}

pub trait FromRef<T> {
    fn from_ref(input: &T) -> Self;
}

impl<K, G> Argument<K> for GroupCount<G>
where
    K: Key,
    G: FromRef<K> + Clone + Ord + std::fmt::Debug,
{
    fn from_leaf(keys: &[K]) -> Self {
        let mut keys_iter = keys.iter();

        let first_group = G::from_ref(match keys_iter.next() {
            Some(k) => k,
            None => return Self::Zero,
        });

        let mut group_count = 1;
        let mut min_group_count = 1;
        let mut last_group_count = 1;

        let mut last_group = Cow::Borrowed(&first_group);
        let mut last_group_item_count = &mut min_group_count;

        for key in keys_iter {
            let group = G::from_ref(key);
            if last_group.as_ref().ne(&group) {
                group_count += 1;
                last_group_item_count = &mut last_group_count;
                last_group = Cow::Owned(group);
            } else {
                *last_group_item_count += 1;
            }
        }

        match group_count {
            1 => GroupCount::One(first_group, min_group_count),
            _ => {
                let last_group = last_group.into_owned();
                GroupCount::Multiple {
                    min_group: (first_group, min_group_count),
                    max_group: (last_group, last_group_count),
                    group_count,
                }
            }
        }
    }

    fn from_inner(_keys: &[K], arguments: &[Self]) -> Self {
        if arguments.is_empty() {
            return Self::Zero;
        }

        let mut accumulated = Self::Zero;
        arguments.iter().for_each(|a| accumulated.merge_with(a));
        accumulated
    }
}

impl<K, G> SearchArgument<K> for GroupCount<G>
where
    K: Key,
    G: FromRef<K> + Clone + Ord + std::fmt::Debug,
{
    /// It can be searched by Group and offset
    type Query = (G, usize);

    fn locate_in_leaf((group, offset): (G, usize), keys: &[K]) -> Option<usize> {
        let mut in_group_offset = 0;
        for (idx, k) in keys.iter().enumerate() {
            let group_for_key = G::from_ref(k);

            match group_for_key.cmp(&group) {
                Ordering::Less => continue,
                Ordering::Equal => {
                    if in_group_offset == offset {
                        return Some(idx);
                    } else {
                        in_group_offset += 1;
                    }
                }
                Ordering::Greater => return None,
            }
        }

        None
    }

    fn locate_in_inner(
        (group, mut offset): Self::Query,
        _keys: &[K],
        arguments: &[Self],
    ) -> Option<(usize, Self::Query)> {
        for (idx, a) in arguments.iter().enumerate() {
            match a {
                GroupCount::Zero => {}
                GroupCount::One(g, c) => match g.cmp(&group) {
                    Ordering::Less => continue,
                    Ordering::Equal => {
                        if *c > offset {
                            return Some((idx, (group, offset)));
                        } else {
                            offset -= c;
                            continue;
                        }
                    }
                    Ordering::Greater => return None,
                },
                GroupCount::Multiple { max_group, .. } => match max_group.0.cmp(&group) {
                    Ordering::Less => continue,
                    Ordering::Equal => {
                        if max_group.1 > offset {
                            return Some((idx, (group, offset)));
                        } else {
                            offset -= max_group.1;
                            continue;
                        }
                    }
                    Ordering::Greater => {
                        return Some((idx, (group, offset)));
                    }
                },
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use crate::{BPlusTree, NodeStoreVec};

    use super::*;

    #[test]
    fn test_group_count() {
        #[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Copy)]
        struct Half(u8);

        impl FromRef<u8> for Half {
            fn from_ref(input: &u8) -> Self {
                Self(*input / 2 as u8)
            }
        }

        let argument = GroupCount::<Half>::from_leaf(&[1, 2, 3]);
        assert_eq!(argument.group_count(), 2);

        let argument = <GroupCount<Half>>::from_inner(
            &[1, 2, 3],
            &[
                GroupCount::Multiple {
                    min_group: (Half(0), 2),
                    max_group: (Half(1), 1),
                    group_count: 2,
                },
                GroupCount::Multiple {
                    min_group: (Half(1), 2),
                    max_group: (Half(4), 3),
                    group_count: 2,
                },
                GroupCount::Multiple {
                    min_group: (Half(4), 4),
                    max_group: (Half(10), 5),
                    group_count: 3,
                },
            ],
        );

        // 1 and 4 are dup groups in child, so we need to fix the double counting
        assert_eq!(argument.group_count(), 2 + 1 + 2);

        let argument = GroupCount::<Half>::from_inner(
            &[1, 2, 3],
            &[
                GroupCount::One(Half(0), 3),
                GroupCount::One(Half(0), 4),
                GroupCount::One(Half(0), 5),
            ],
        );
        assert_eq!(argument.group_count(), 1);
    }

    #[test]
    fn test_group_count_in_tree() {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
        struct First(u64);

        impl FromRef<(u64, u64)> for First {
            fn from_ref(input: &(u64, u64)) -> Self {
                First(input.0)
            }
        }

        let node_store = NodeStoreVec::<(u64, u64), i64, GroupCount<First>>::new();
        let mut tree = BPlusTree::new(node_store);

        tree.insert((1, 1), 100);
        assert_eq!(tree.root_argument().group_count(), 1);
        tree.remove(&(1, 1));
        assert!(tree.root_argument().is_zero());

        tree.insert((1, 1), 100);
        tree.insert((1, 2), 101);
        assert_eq!(tree.root_argument().group_count(), 1);

        tree.insert((1, 3), 100);
        tree.insert((2, 4), 100);
        assert_eq!(tree.root_argument().group_count(), 2);
        tree.insert((3, 5), 100);
        tree.insert((4, 6), 100);
        assert_eq!(tree.root_argument().group_count(), 4);
        tree.remove(&(4, 6));
        assert_eq!(tree.root_argument().group_count(), 3);

        // find in group First(1)
        // offset 0
        assert_eq!(tree.get_by_argument((First(1), 0)).unwrap().1, &100);
        // offset 1
        assert_eq!(tree.get_by_argument((First(1), 1)).unwrap().1, &101);
        // offset 3 (2 is also exists)
        assert!(dbg!(tree.get_by_argument((First(1), 3))).is_none());
    }
}
