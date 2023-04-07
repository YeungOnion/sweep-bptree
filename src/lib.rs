mod inner_node;
mod utils;
use std::{cell::Cell, mem::ManuallyDrop};

pub use inner_node::*;
mod leaf_node;
pub use leaf_node::*;
mod node_id;
pub use node_id::*;
mod cursor;
pub use cursor::*;
mod iterator;
pub use iterator::*;
mod node_stores;
pub use node_stores::*;
mod bulk_load;

/// B plus tree implementation, with following considerations:
///
/// 1. Performance critical, for sweep like algo, the sweepline's searching and updating is on hot path
/// 2. Cursor support, after one search, it should be fairly easy to move next or prev without another search
/// 3. Memory efficient, reduce mem alloc
///
/// # Example
/// ```rust
/// use sweep_bptree::{BPlusTree, NodeStoreVec};
///
/// // create a node_store with `u64` as key, `(f64, f64)` as value, inner node size 64, child size 65, leaf node size 64
/// let mut node_store = NodeStoreVec::<u64, (f64, f64), 64, 65, 64>::new();
/// let mut tree = BPlusTree::new(node_store);
///
/// // insert new value
/// assert!(tree.insert(3, (0., 0.)).is_none());
///
/// // update by insert again
/// assert_eq!(tree.insert(3, (1., 1.)).unwrap(), (0., 0.));
///
/// // remove the value
/// assert_eq!(tree.remove(&3).unwrap(), (1.0, 1.0));
///
/// assert!(tree.is_empty());
/// ```
///
/// # Example
/// Create multiple owned cursors
///
/// ``` rust
/// use sweep_bptree::{BPlusTree, NodeStoreVec};
/// let mut node_store = NodeStoreVec::<u64, (f64, f64), 64, 65, 64>::new();
/// let mut tree = BPlusTree::new(node_store);
///
/// for i in 0..100 {
///     tree.insert(i, (i as f64, i as f64));
/// }
///
/// let cursor_0 = tree.cursor_first().unwrap();
/// let cursor_1 = tree.cursor_first().unwrap();
///
/// // remove the key 0
/// tree.remove(&0);
///
/// // cursor's value should be empty now
/// assert!(cursor_0.value(&tree).is_none());
///
/// // move to next
/// let cursor_next = cursor_0.next(&tree).unwrap();
/// assert_eq!(*cursor_next.key(), 1);
/// assert_eq!(cursor_next.value(&tree).unwrap().0, 1.);
///
/// // insert a new value to key 0
/// tree.insert(0, (100., 100.));
/// // now cursor_1 should retrieve the new value
/// assert_eq!(cursor_1.value(&tree).unwrap().0, 100.);
/// ```
#[derive(Clone)]
pub struct BPlusTree<S: NodeStore> {
    root: NodeId,
    len: usize,
    node_store: ManuallyDrop<S>,
    /// store last accessed leaf, and it's key range
    leaf_cache: Cell<Option<CacheItem<S::K>>>,

    st: Statistic,
}

impl<S> BPlusTree<S>
where
    S: NodeStore,
{
    /// Create a new `BPlusTree` with the given `NodeStore`.
    pub fn new(mut node_store: S) -> Self {
        let (root_id, _) = node_store.new_empty_leaf();
        Self {
            root: NodeId::Leaf(root_id),
            node_store: ManuallyDrop::new(node_store),
            leaf_cache: Cell::new(None),
            len: 0,

            st: Statistic::default(),
        }
    }

    /// Create a new `BPlusTree` from existing parts
    fn new_from_parts(node_store: S, root: NodeId, len: usize) -> Self {
        let me = Self {
            root,
            node_store: ManuallyDrop::new(node_store),
            leaf_cache: Cell::new(None),
            len,

            st: Statistic::default(),
        };

        #[cfg(test)]
        me.validate();

        me
    }

    /// Gets a reference to the `NodeStore` that this `BPlusTree` was created with.
    pub fn node_store(&self) -> &S {
        &self.node_store
    }

    /// Returns the number of elements in the tree.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns true if the tree contains no elements.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn insert(&mut self, k: S::K, v: S::V) -> Option<S::V> {
        // quick check if the last accessed leaf is the one to insert
        if let Some(cache) = self.leaf_cache.get().as_ref() {
            if cache.in_range(&k) {
                let cache_leaf_id = cache.leaf_id;

                let leaf = self.node_store.get_mut_leaf(cache_leaf_id);
                if !leaf.is_full() {
                    let result = match leaf.try_upsert(k, v) {
                        LeafUpsertResult::Inserted => {
                            self.len += 1;
                            let cache_item = CacheItem::try_from(cache_leaf_id, leaf);
                            self.set_cache(cache_item);
                            None
                        }
                        LeafUpsertResult::Updated(v) => Some(v),
                        LeafUpsertResult::IsFull(_, _) => unreachable!(),
                    };

                    #[cfg(test)]
                    self.validate();

                    return result;
                }
            }
        }

        let node_id = self.root;

        let result = match self.descend_insert(node_id, k, v) {
            DescendInsertResult::Inserted => None,
            DescendInsertResult::Updated(prev_v) => Some(prev_v),
            DescendInsertResult::Split(k, new_child_id) => {
                let new_root = S::InnerNode::new([k], [node_id, new_child_id]);
                let new_root_id = self.node_store.add_inner(new_root);
                self.root = new_root_id.into();
                None
            }
        };

        if result.is_none() {
            self.len += 1;
        }

        #[cfg(test)]
        self.validate();

        result
    }

    fn into_parts(self) -> (S, NodeId, usize) {
        let mut me = ManuallyDrop::new(self);
        let _ = me.leaf_cache;
        (
            unsafe { ManuallyDrop::take(&mut me.node_store) },
            me.root,
            me.len,
        )
    }

    pub fn statistic(&self) -> &Statistic {
        &self.st
    }

    fn descend_insert_inner(
        &mut self,
        id: InnerNodeId,
        k: S::K,
        v: S::V,
    ) -> DescendInsertResult<S::K, S::V> {
        let node = self.node_store.get_inner(id);
        let (child_idx, child_id) = node.locate_child(&k);
        match self.descend_insert(child_id, k, v) {
            DescendInsertResult::Inserted => DescendInsertResult::Inserted,
            DescendInsertResult::Split(key, right_child) => {
                // child splited
                let inner_node = self.node_store.get_mut_inner(id);

                if !inner_node.is_full() {
                    let slot = child_idx;
                    inner_node.insert_at(slot, key, right_child);
                    DescendInsertResult::Inserted
                } else {
                    let (prompt_k, new_node) = inner_node.split(child_idx, key, right_child);

                    let new_node_id = self.node_store.add_inner(new_node);
                    DescendInsertResult::Split(prompt_k, NodeId::Inner(new_node_id))
                }
            }
            r => r,
        }
    }

    fn descend_insert(
        &mut self,
        node_id: NodeId,
        k: S::K,
        v: S::V,
    ) -> DescendInsertResult<S::K, S::V> {
        match node_id {
            NodeId::Inner(node_id) => self.descend_insert_inner(node_id, k, v),
            NodeId::Leaf(leaf_id) => self.insert_leaf(leaf_id, k, v),
        }
    }

    fn insert_leaf(&mut self, id: LeafNodeId, k: S::K, v: S::V) -> DescendInsertResult<S::K, S::V> {
        let leaf_node = self.node_store.get_mut_leaf(id);
        match leaf_node.try_upsert(k, v) {
            LeafUpsertResult::Inserted => {
                let cache_item = CacheItem::try_from(id, leaf_node);
                self.set_cache(cache_item);
                DescendInsertResult::Inserted
            }
            LeafUpsertResult::Updated(v) => DescendInsertResult::Updated(v),
            LeafUpsertResult::IsFull(idx, v) => {
                let new_id = self.node_store.reserve_leaf();

                let l_leaf = self.node_store.get_mut_leaf(id);
                let r_leaf = l_leaf.split_new_leaf(idx, (k, v), new_id, id);
                let slot_key: S::K = *r_leaf.data_at(0).0;

                if k >= slot_key {
                    self.set_cache(CacheItem::try_from(new_id, r_leaf.as_ref()));
                } else {
                    let cache_item = CacheItem::try_from(id, l_leaf);
                    self.set_cache(cache_item);
                }

                // fix r_leaf's next's prev
                if let Some(next) = r_leaf.next() {
                    self.node_store.get_mut_leaf(next).set_prev(Some(new_id));
                }
                self.node_store.assign_leaf(new_id, r_leaf);

                DescendInsertResult::Split(slot_key, NodeId::Leaf(new_id))
            }
        }
    }

    #[inline]
    fn set_cache(&self, cache_item: Option<CacheItem<S::K>>) {
        self.leaf_cache.set(cache_item);
    }

    /// Get reference to value identified by key.
    pub fn get(&self, k: &S::K) -> Option<&S::V> {
        if let Some(cache) = self.leaf_cache.get() {
            if cache.in_range(k) {
                // cache hit
                return self.find_in_leaf(cache.leaf_id, k);
            }
        }

        self.find_descend(self.root, k)
    }

    /// Get mutable reference to value identified by key.
    pub fn get_mut(&mut self, k: &S::K) -> Option<&mut S::V> {
        let mut cache_leaf_id: Option<LeafNodeId> = None;
        if let Some(cache) = self.leaf_cache.get() {
            if cache.in_range(k) {
                // cache hit
                cache_leaf_id = Some(cache.leaf_id);
            }
        }

        if let Some(leaf_id) = cache_leaf_id {
            return self.find_in_leaf_mut(leaf_id, k);
        }

        self.find_descend_mut(self.root, k)
    }

    fn find_descend(&self, node_id: NodeId, k: &S::K) -> Option<&S::V> {
        match node_id {
            NodeId::Inner(inner_id) => {
                let inner_node = self.node_store.get_inner(inner_id);
                let (_, child_id) = inner_node.locate_child(k);
                self.find_descend(child_id, k)
            }
            NodeId::Leaf(leaf_id) => self.find_in_leaf_and_cache_it(leaf_id, k),
        }
    }

    fn find_in_leaf(&self, leaf_id: LeafNodeId, k: &S::K) -> Option<&S::V> {
        let leaf_node = self.node_store.get_leaf(leaf_id);
        let (_, kv) = leaf_node.locate_slot_with_value(k);
        kv
    }

    fn find_descend_mut(&mut self, node_id: NodeId, k: &S::K) -> Option<&mut S::V> {
        match node_id {
            NodeId::Inner(inner_id) => {
                let inner_node = self.node_store.get_inner(inner_id);
                let (_, child_id) = inner_node.locate_child(k);
                self.find_descend_mut(child_id, k)
            }
            NodeId::Leaf(leaf_id) => self.find_in_leaf_mut_and_cache_it(leaf_id, k),
        }
    }

    fn find_in_leaf_mut(&mut self, leaf_id: LeafNodeId, k: &S::K) -> Option<&mut S::V> {
        let leaf_node = self.node_store.get_mut_leaf(leaf_id);
        let (_, v) = leaf_node.locate_slot_mut(k);
        v
    }

    fn find_in_leaf_mut_and_cache_it(
        &mut self,
        leaf_id: LeafNodeId,
        k: &S::K,
    ) -> Option<&mut S::V> {
        let leaf_node = self.node_store.get_mut_leaf(leaf_id);
        self.leaf_cache.set(CacheItem::try_from(leaf_id, leaf_node));
        let (_, kv) = leaf_node.locate_slot_mut(k);
        kv
    }

    fn find_in_leaf_and_cache_it(&self, leaf_id: LeafNodeId, k: &S::K) -> Option<&S::V> {
        let leaf = self.node_store.get_leaf(leaf_id);
        self.set_cache(CacheItem::try_from(leaf_id, leaf));
        let (_, v) = leaf.locate_slot_with_value(k);
        v
    }

    /// delete element identified by K
    pub fn remove(&mut self, k: &S::K) -> Option<S::V> {
        // quick check if the last accessed leaf is the one to remove
        if let Some(cache) = self.leaf_cache.get().as_ref() {
            if cache.in_range(&k) {
                let cache_leaf_id = cache.leaf_id;

                let leaf = self.node_store.get_mut_leaf(cache_leaf_id);
                if leaf.able_to_lend() {
                    let result = match leaf.try_delete(k) {
                        LeafDeleteResult::Done(v) => {
                            self.len -= 1;
                            let cache_item = CacheItem::try_from(cache_leaf_id, leaf);
                            self.set_cache(cache_item);
                            Some(v.1)
                        }
                        LeafDeleteResult::NotFound => None,
                        LeafDeleteResult::UnderSize(_) => unreachable!(),
                    };

                    #[cfg(test)]
                    self.validate();

                    return result;
                }
            }
        }

        let root_id = self.root;
        let r = match root_id {
            NodeId::Inner(inner_id) => match self.remove_inner(inner_id, k) {
                DeleteDescendResult::Done(kv) => Some(kv),
                DeleteDescendResult::None => None,
                DeleteDescendResult::InnerUnderSize(deleted_item) => {
                    let root = self.node_store.get_mut_inner(inner_id);

                    if root.is_empty() {
                        self.root = root.child_id(0);
                    }

                    Some(deleted_item)
                }
            },
            NodeId::Leaf(leaf_id) => {
                let leaf = self.node_store.get_mut_leaf(leaf_id);
                match leaf.try_delete(k) {
                    LeafDeleteResult::Done(kv) => Some(kv),
                    LeafDeleteResult::NotFound => None,
                    LeafDeleteResult::UnderSize(idx) => {
                        let item = leaf.delete_at(idx);
                        Some(item)
                    }
                }
            }
        };

        if r.is_some() {
            self.len -= 1;
        }

        r.map(|kv| kv.1)
    }

    fn remove_inner(&mut self, node_id: InnerNodeId, k: &S::K) -> DeleteDescendResult<S::K, S::V> {
        let mut inner_node = self.node_store.take_inner(node_id);

        let (child_idx, child_id) = inner_node.locate_child(k);
        let r = match child_id {
            NodeId::Inner(inner_id) => match self.remove_inner(inner_id, k) {
                DeleteDescendResult::Done(kv) => DeleteDescendResult::Done(kv),
                DeleteDescendResult::InnerUnderSize(deleted_item) => {
                    self.handle_inner_under_size(&mut inner_node, child_idx, deleted_item)
                }
                DeleteDescendResult::None => DeleteDescendResult::None,
            },
            NodeId::Leaf(leaf_id) => {
                let leaf = self.node_store.get_mut_leaf(leaf_id);
                match leaf.try_delete(k) {
                    LeafDeleteResult::Done(kv) => DeleteDescendResult::Done(kv),
                    LeafDeleteResult::NotFound => DeleteDescendResult::None,
                    LeafDeleteResult::UnderSize(idx) => {
                        self.handle_leaf_under_size(&mut inner_node, child_idx, idx)
                    }
                }
            }
        };

        self.node_store.put_back_inner(node_id, inner_node);
        r
    }

    fn handle_inner_under_size(
        &mut self,
        node: &mut S::InnerNode,
        child_idx: usize,
        deleted_item: (S::K, S::V),
    ) -> DeleteDescendResult<S::K, S::V> {
        if child_idx > 0 {
            if Self::try_rotate_right_for_inner_node(&mut self.node_store, node, child_idx - 1)
                .is_some()
            {
                self.st.rotate_right_inner += 1;
                return DeleteDescendResult::Done(deleted_item);
            }
        }
        if child_idx < node.size() {
            if Self::try_rotate_left_for_inner_node(&mut self.node_store, node, child_idx).is_some()
            {
                self.st.rotate_left_inner += 1;
                return DeleteDescendResult::Done(deleted_item);
            }
        }

        let merge_slot = if child_idx > 0 {
            self.st.merge_with_left_inner += 1;
            child_idx - 1
        } else {
            self.st.merge_with_right_inner += 1;
            child_idx
        };

        match Self::merge_inner_node(&mut self.node_store, node, merge_slot) {
            InnerMergeResult::Done => {
                return DeleteDescendResult::Done(deleted_item);
            }
            InnerMergeResult::UnderSize => {
                return DeleteDescendResult::InnerUnderSize(deleted_item);
            }
        }
    }

    fn handle_leaf_under_size(
        &mut self,
        node: &mut S::InnerNode,
        child_idx: usize,
        key_idx_in_child: usize,
    ) -> DeleteDescendResult<<S as NodeStore>::K, <S as NodeStore>::V> {
        let prev_sibling = if child_idx > 0 {
            Some(
                self.node_store
                    .get_leaf(unsafe { node.child_id(child_idx - 1).leaf_id_unchecked() }),
            )
        } else {
            None
        };
        let next_sibling = if child_idx < node.size() {
            Some(
                self.node_store
                    .get_leaf(unsafe { node.child_id(child_idx + 1).leaf_id_unchecked() }),
            )
        } else {
            None
        };

        let action: FixAction = match (prev_sibling, next_sibling) {
            (Some(p), Some(n)) => {
                if p.able_to_lend() {
                    if n.able_to_lend() {
                        if p.len() > n.len() {
                            FixAction::RotateRight
                        } else {
                            FixAction::RotateLeft
                        }
                    } else {
                        FixAction::RotateRight
                    }
                } else if n.able_to_lend() {
                    FixAction::RotateLeft
                } else {
                    FixAction::MergeLeft
                }
            }
            (Some(p), None) => {
                if p.able_to_lend() {
                    FixAction::RotateRight
                } else {
                    FixAction::MergeLeft
                }
            }
            (None, Some(n)) => {
                if n.able_to_lend() {
                    FixAction::RotateLeft
                } else {
                    FixAction::MergeRight
                }
            }
            _ => unreachable!(),
        };

        match action {
            FixAction::RotateRight => {
                let (deleted, cache_item) = Self::try_rotate_right_for_leaf_node(
                    &mut self.node_store,
                    node,
                    child_idx - 1,
                    key_idx_in_child,
                );
                self.st.rotate_right_leaf += 1;
                self.set_cache(cache_item);
                return DeleteDescendResult::Done(deleted);
            }
            FixAction::RotateLeft => {
                let (deleted, cache_item) = Self::rotate_left_for_leaf_node(
                    &mut self.node_store,
                    node,
                    child_idx,
                    key_idx_in_child,
                );
                self.st.rotate_left_leaf += 1;
                self.set_cache(cache_item);
                return DeleteDescendResult::Done(deleted);
            }
            FixAction::MergeLeft => {
                self.st.merge_with_left_leaf += 1;
                // merge with prev node
                let (result, cache_item) = Self::merge_leaf_node_left(
                    &mut self.node_store,
                    node,
                    child_idx - 1,
                    key_idx_in_child,
                );
                self.set_cache(cache_item);
                result
            }
            FixAction::MergeRight => {
                self.st.merge_with_right_leaf += 1;
                // merge with next node
                let (result, cache_item) = Self::merge_leaf_node_with_right(
                    &mut self.node_store,
                    node,
                    child_idx,
                    key_idx_in_child,
                );

                self.set_cache(cache_item);
                result
            }
        }
    }

    fn try_rotate_right_for_inner_node(
        node_store: &mut S,
        node: &mut S::InnerNode,
        slot: usize,
    ) -> Option<()> {
        //     1    3  5
        //      ..2  4
        // rotate right
        //     1 2     5
        //     ..  3,4
        let right_child_id = unsafe { node.child_id(slot + 1).inner_id_unchecked() };
        let left_child_id = unsafe { node.child_id(slot).inner_id_unchecked() };
        let slot_key = *node.key(slot);

        let prev_node = node_store.get_mut_inner(left_child_id);
        if prev_node.able_to_lend() {
            let (k, c) = prev_node.pop();
            let child = node_store.get_mut_inner(right_child_id);
            child.push_front(slot_key, c);

            node.set_key(slot, k);

            Some(())
        } else {
            None
        }
    }

    fn try_rotate_left_for_inner_node(
        node_store: &mut S,
        node: &mut S::InnerNode,
        slot: usize,
    ) -> Option<()> {
        //     1  3  5
        //       2  4..
        // rotate right
        //     1   4   5
        //      2,3  ..
        let right_child_id = unsafe { node.child_id(slot + 1).inner_id_unchecked() };
        let left_child_id = unsafe { node.child_id(slot).inner_id_unchecked() };
        let slot_key = node.key(slot).clone();

        let right = node_store.get_mut_inner(right_child_id);
        if right.able_to_lend() {
            let (k, c) = right.pop_front();
            let left = node_store.get_mut_inner(left_child_id);
            left.push(slot_key, c);

            node.set_key(slot, k);

            Some(())
        } else {
            None
        }
    }

    fn merge_inner_node(
        node_store: &mut S,
        node: &mut S::InnerNode,
        slot: usize,
    ) -> InnerMergeResult {
        //     1  3  5
        //       2  4
        //  merge 3
        //     1        5
        //       2,3,4
        debug_assert!(slot < node.size());

        let left_child_id = unsafe { node.child_id(slot).inner_id_unchecked() };
        let right_child_id = unsafe { node.child_id(slot + 1).inner_id_unchecked() };
        let slot_key = node.key(slot).clone();

        // merge right into left
        let mut right = node_store.take_inner(right_child_id);
        let left = node_store.get_mut_inner(left_child_id);

        left.merge_next(slot_key, &mut right);

        node.merge_child(slot)
    }

    fn try_rotate_right_for_leaf_node(
        node_store: &mut S,
        node: &mut S::InnerNode,
        slot: usize,
        delete_idx: usize,
    ) -> ((S::K, S::V), Option<CacheItem<S::K>>) {
        let left_id = unsafe { node.child_id(slot).leaf_id_unchecked() };
        let right_id = unsafe { node.child_id(slot + 1).leaf_id_unchecked() };

        let left = node_store.get_mut_leaf(left_id);
        debug_assert!(left.able_to_lend());

        let kv = left.pop();
        let new_slot_key = kv.0;
        let right = node_store.get_mut_leaf(right_id);
        let deleted = right.delete_with_push_front(delete_idx, kv);

        let cache_item = CacheItem::try_from(right_id, right);

        node.set_key(slot, new_slot_key);

        (deleted, cache_item)
    }

    fn rotate_left_for_leaf_node(
        node_store: &mut S,
        parent: &mut S::InnerNode,
        slot: usize,
        delete_idx: usize,
    ) -> ((S::K, S::V), Option<CacheItem<S::K>>) {
        let left_id = unsafe { parent.child_id(slot).leaf_id_unchecked() };
        let right_id = unsafe { parent.child_id(slot + 1).leaf_id_unchecked() };

        let right = node_store.get_mut_leaf(right_id);
        debug_assert!(right.able_to_lend());

        let kv = right.pop_front();
        let new_slot_key = *right.data_at(0).0;
        let left = node_store.get_mut_leaf(left_id);
        let deleted = left.delete_with_push(delete_idx, kv);

        let cache_item = CacheItem::try_from(left_id, left);

        parent.set_key(slot, new_slot_key);

        (deleted, cache_item)
    }

    fn merge_leaf_node_left(
        node_store: &mut S,
        parent: &mut S::InnerNode,
        slot: usize,
        delete_idx: usize,
    ) -> (DeleteDescendResult<S::K, S::V>, Option<CacheItem<S::K>>) {
        let left_leaf_id = unsafe { parent.child_id(slot).leaf_id_unchecked() };
        let right_leaf_id = unsafe { parent.child_id(slot + 1).leaf_id_unchecked() };

        let mut right = node_store.take_leaf(right_leaf_id);
        let left = node_store.get_mut_leaf(left_leaf_id);
        let kv = left.merge_right_delete_first(delete_idx, &mut right);

        let cache_item = CacheItem::try_from(left_leaf_id, left);

        if let Some(next) = left.next() {
            node_store.get_mut_leaf(next).set_prev(Some(left_leaf_id));
        }

        (
            match parent.merge_child(slot) {
                InnerMergeResult::Done => DeleteDescendResult::Done(kv),
                InnerMergeResult::UnderSize => DeleteDescendResult::InnerUnderSize(kv),
            },
            cache_item,
        )
    }

    fn merge_leaf_node_with_right(
        node_store: &mut S,
        parent: &mut S::InnerNode,
        slot: usize,
        delete_idx: usize,
    ) -> (DeleteDescendResult<S::K, S::V>, Option<CacheItem<S::K>>) {
        let left_leaf_id = unsafe { parent.child_id(slot).leaf_id_unchecked() };
        let right_leaf_id = unsafe { parent.child_id(slot + 1).leaf_id_unchecked() };

        let mut right = node_store.take_leaf(right_leaf_id);
        let left = node_store.get_mut_leaf(left_leaf_id);
        let kv = left.delete_at(delete_idx);
        left.merge_right(&mut right);

        let cache_item = CacheItem::try_from(left_leaf_id, left);

        if let Some(next) = left.next() {
            node_store.get_mut_leaf(next).set_prev(Some(left_leaf_id));
        }

        // the merge on inner, it could propagate
        (
            match parent.merge_child(slot) {
                InnerMergeResult::Done => DeleteDescendResult::Done(kv),
                InnerMergeResult::UnderSize => DeleteDescendResult::InnerUnderSize(kv),
            },
            cache_item,
        )
    }

    /// get the first leaf_id if exists
    pub fn first_leaf(&self) -> Option<LeafNodeId> {
        match self.root {
            NodeId::Inner(inner_id) => {
                let mut result = None;

                self.descend_visit_inner(inner_id, |inner_node| {
                    let first_child_id = inner_node.child_id(0);
                    match first_child_id {
                        NodeId::Inner(inner_id) => Some(inner_id),
                        NodeId::Leaf(leaf_id) => {
                            result = Some(leaf_id);
                            None
                        }
                    }
                });

                result
            }
            NodeId::Leaf(leaf_id) => Some(leaf_id),
        }
    }

    /// get the last leaf_id if exists
    pub fn last_leaf(&self) -> Option<LeafNodeId> {
        match self.root {
            NodeId::Inner(inner_id) => {
                let mut result = None;

                self.descend_visit_inner(inner_id, |inner_node| {
                    let child_id = inner_node.child_id(inner_node.size());
                    match child_id {
                        NodeId::Inner(inner_id) => Some(inner_id),
                        NodeId::Leaf(leaf_id) => {
                            result = Some(leaf_id);
                            None
                        }
                    }
                });

                result
            }
            NodeId::Leaf(leaf_id) => Some(leaf_id),
        }
    }

    /// Locate the leaf node for `k`.
    /// Returns the leaf whose range contains `k`.
    /// User should query the leaf and check key existance.
    pub fn locate_leaf(&self, k: &S::K) -> Option<LeafNodeId> {
        if let Some(cache) = self.leaf_cache.get().as_ref() {
            if cache.in_range(k) {
                // cache hit
                return Some(cache.leaf_id);
            }
        }

        let leaf_id = match self.root {
            NodeId::Inner(inner_id) => {
                let mut result = None;
                self.descend_visit_inner(inner_id, |inner_node| {
                    let (_idx, node_id) = inner_node.locate_child(k);
                    match node_id {
                        NodeId::Inner(inner_node) => Some(inner_node),
                        NodeId::Leaf(leaf_id) => {
                            result = Some(leaf_id);
                            None
                        }
                    }
                });
                result
            }
            NodeId::Leaf(leaf_id) => Some(leaf_id),
        }?;

        Some(leaf_id)
    }

    fn descend_visit_inner(
        &self,
        node_id: InnerNodeId,
        mut f: impl FnMut(&S::InnerNode) -> Option<InnerNodeId>,
    ) -> Option<()> {
        let inner = self.node_store.get_inner(node_id);
        match f(inner) {
            None => {
                return None;
            }
            Some(id_to_visit) => self.descend_visit_inner(id_to_visit, f),
        }
    }

    /// Create an iterator on (&K, &V) pairs
    pub fn iter(&self) -> iterator::Iter<S> {
        iterator::Iter::new(self)
    }

    pub fn into_iter(self) -> iterator::IntoIter<S> {
        iterator::IntoIter::new(self)
    }

    /// Create an cursor from first elem
    pub fn cursor_first(&self) -> Option<Cursor<S::K>> {
        Cursor::first(self).map(|c| c.0)
    }

    /// Create an cursor for k
    pub fn get_cursor(&self, k: &S::K) -> Option<(Cursor<S::K>, Option<&S::V>)> {
        let node_id = self.root;
        let leaf_id = match node_id {
            NodeId::Inner(inner_id) => {
                let mut result = None;
                self.descend_visit_inner(inner_id, |inner_node| {
                    let (_idx, node_id) = inner_node.locate_child(k);
                    match node_id {
                        NodeId::Inner(inner_node) => Some(inner_node),
                        NodeId::Leaf(leaf_id) => {
                            result = Some(leaf_id);
                            None
                        }
                    }
                });
                result
            }
            NodeId::Leaf(leaf_id) => Some(leaf_id),
        }?;

        let leaf = self.node_store.get_leaf(leaf_id);
        let (idx, v) = leaf.locate_slot_with_value(k);
        Some((Cursor::new(*k, leaf_id, idx), v))
    }

    #[cfg(test)]
    fn validate(&self) {
        let Some(mut leaf_id) = self.first_leaf() else { return; };
        let mut last_leaf_id: Option<LeafNodeId> = None;

        // ensures all prev and next are correct
        loop {
            let leaf = self.node_store.get_leaf(leaf_id);

            let p = leaf.prev();
            let n = leaf.next();

            if let Some(last_leaf_id) = last_leaf_id {
                assert_eq!(last_leaf_id, p.unwrap());
            }

            if n.is_none() {
                break;
            }

            last_leaf_id = Some(leaf_id);
            leaf_id = n.unwrap();
        }
    }
}

impl<S: NodeStore> Drop for BPlusTree<S> {
    fn drop(&mut self) {
        unsafe { drop(std::ptr::read(self).into_iter()) }
    }
}

#[derive(Default, Clone, Copy, Debug)]
pub struct Statistic {
    pub rotate_right_inner: u64,
    pub rotate_left_inner: u64,

    pub merge_with_left_inner: u64,
    pub merge_with_right_inner: u64,

    pub rotate_right_leaf: u64,
    pub rotate_left_leaf: u64,

    pub merge_with_left_leaf: u64,
    pub merge_with_right_leaf: u64,
}

#[derive(Clone, Copy)]
struct CacheItem<K> {
    start: Option<K>,
    end: Option<K>,
    leaf_id: LeafNodeId,
    // consider cache the item?
}

impl<K: std::fmt::Debug> std::fmt::Debug for CacheItem<K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheItem")
            .field("start", &self.start)
            .field("end", &self.end)
            .field("leaf_id", &self.leaf_id)
            .finish()
    }
}

impl<K: Key> CacheItem<K> {
    fn try_from<L: LNode<K, V>, V: Value>(id: LeafNodeId, leaf: &L) -> Option<Self> {
        let (start, end) = leaf.key_range();
        Some(Self {
            start,
            end,
            leaf_id: id,
        })
    }

    pub fn in_range(&self, k: &K) -> bool {
        let is_lt_start = self
            .start
            .as_ref()
            .map(|start| k.lt(start))
            .unwrap_or(false);
        if is_lt_start {
            return false;
        }

        let is_gt_end = self.end.as_ref().map(|end| k.gt(end)).unwrap_or(false);
        !is_gt_end
    }
}

enum FixAction {
    RotateRight,
    RotateLeft,
    MergeLeft,
    MergeRight,
}

enum DescendInsertResult<K, V> {
    /// Update existing value, V is the previous value
    Updated(V),
    /// Inserted, and not split
    Inserted,
    /// need to split
    Split(K, NodeId),
}

#[derive(Debug)]
enum DeleteDescendResult<K, V> {
    None,
    Done((K, V)),
    /// Inner node under size, the index and node_id to remove
    InnerUnderSize((K, V)),
}

pub trait NodeStore: Clone + Default {
    type K: Key;
    type V: Value;

    type InnerNode: INode<Self::K>;
    type LeafNode: LNode<Self::K, Self::V>;

    /// Get the max number of keys inner node can hold
    fn inner_n() -> u16;
    /// Get the max number of elements leaf node can hold
    fn leaf_n() -> u16;

    #[cfg(test)]
    fn new_empty_inner(&mut self) -> InnerNodeId;
    fn add_inner(&mut self, node: Box<Self::InnerNode>) -> InnerNodeId;
    fn get_inner(&self, id: InnerNodeId) -> &Self::InnerNode;
    fn try_get_inner(&self, id: InnerNodeId) -> Option<&Self::InnerNode>;
    fn get_mut_inner(&mut self, id: InnerNodeId) -> &mut Self::InnerNode;
    fn take_inner(&mut self, id: InnerNodeId) -> Box<Self::InnerNode>;
    fn put_back_inner(&mut self, id: InnerNodeId, node: Box<Self::InnerNode>);

    fn new_empty_leaf(&mut self) -> (LeafNodeId, &mut Self::LeafNode);
    fn reserve_leaf(&mut self) -> LeafNodeId;
    fn get_leaf(&self, id: LeafNodeId) -> &Self::LeafNode;
    fn try_get_leaf(&self, id: LeafNodeId) -> Option<&Self::LeafNode>;
    fn get_mut_leaf(&mut self, id: LeafNodeId) -> &mut Self::LeafNode;
    fn take_leaf(&mut self, id: LeafNodeId) -> Box<Self::LeafNode>;
    fn assign_leaf(&mut self, id: LeafNodeId, leaf: Box<Self::LeafNode>);

    #[cfg(test)]
    fn debug(&self);
}

pub trait Key:
    std::fmt::Debug + Copy + Clone + Ord + PartialOrd + Eq + PartialEq + 'static
{
}
impl<T> Key for T where
    T: std::fmt::Debug + Copy + Clone + Ord + PartialOrd + Eq + PartialEq + 'static
{
}

pub trait Value: Clone {}
impl<T> Value for T where T: Clone {}

/// Inner node trait
pub trait INode<K: Key> {
    /// Create a new inner node with `slot_keys` and `child_id`.
    fn new<I: Into<NodeId> + Copy + Clone, const N1: usize, const C1: usize>(
        slot_keys: [K; N1],
        child_id: [I; C1],
    ) -> Box<Self>;

    /// Create a new inner node from Iterators.
    /// Note: the count for keys must less than `IN`, and count for childs must be keys's count plus 1.
    /// Otherwise this method will panic!
    fn new_from_iter(
        keys: impl Iterator<Item = K>,
        childs: impl Iterator<Item = NodeId>,
    ) -> Box<Self>;

    /// Get the number of keys
    fn size(&self) -> usize;

    /// Check if the node is empty
    fn is_empty(&self) -> bool {
        self.size() == 0
    }

    /// Get the key at `slot`
    fn key(&self, slot: usize) -> &K;

    /// Set the key at `slot`
    fn set_key(&mut self, slot: usize, key: K);

    /// Get the child id at `idx`
    fn child_id(&self, idx: usize) -> NodeId;

    /// Locate child index and `NodeId` for `k`
    fn locate_child(&self, k: &K) -> (usize, NodeId);

    /// Check if the node is full
    fn is_full(&self) -> bool;

    /// Check if the node is able to lend a key to its sibling
    fn able_to_lend(&self) -> bool;

    /// Insert a key and the right child id at `slot`
    fn insert_at(&mut self, slot: usize, key: K, right_child: NodeId);

    /// Split the node at `child_idx` and return the key to be inserted to parent
    fn split(&mut self, child_idx: usize, k: K, new_child_id: NodeId) -> (K, Box<Self>);

    /// Remove the last key and its right child id
    fn pop(&mut self) -> (K, NodeId);

    /// Remove the first key and its left child id
    fn pop_front(&mut self) -> (K, NodeId);

    /// Insert a key and its right child id at the end
    fn push(&mut self, k: K, child: NodeId);

    /// Insert a key and its left child id at the front
    fn push_front(&mut self, k: K, child: NodeId);

    /// Merge the key and its right child id at `slot` with its right sibling
    fn merge_next(&mut self, slot_key: K, right: &mut Self);

    /// Merge children at slot
    fn merge_child(&mut self, slot: usize) -> InnerMergeResult;
}

/// Leaf node trait
pub trait LNode<K: Key, V: Value> {
    /// Create an empty LeafNode
    fn new() -> Box<Self>;

    /// Returns size of the leaf
    fn len(&self) -> usize;

    fn prev(&self) -> Option<LeafNodeId>;
    fn set_prev(&mut self, id: Option<LeafNodeId>);
    fn next(&self) -> Option<LeafNodeId>;
    fn set_next(&mut self, id: Option<LeafNodeId>);

    fn set_data(&mut self, data: impl IntoIterator<Item = (K, V)>);
    fn data_at(&self, slot: usize) -> (&K, &V);
    /// this takes data at `slot` out, makes original storage `uinit`.
    /// This should never called for same slot, or double free will happen.
    unsafe fn take_data(&mut self, slot: usize) -> (K, V);
    fn try_data_at(&self, idx: usize) -> Option<(&K, &V)>;
    fn in_range(&self, k: &K) -> bool;
    fn key_range(&self) -> (Option<K>, Option<K>);
    fn is_full(&self) -> bool;
    fn able_to_lend(&self) -> bool;
    fn try_upsert(&mut self, k: K, v: V) -> LeafUpsertResult<V>;
    fn split_new_leaf(
        &mut self,
        insert_idx: usize,
        item: (K, V),
        new_leaf_id: LeafNodeId,
        self_leaf_id: LeafNodeId,
    ) -> Box<Self>;
    fn locate_slot(&self, k: &K) -> Result<usize, usize>;
    fn locate_slot_with_value(&self, k: &K) -> (usize, Option<&V>);

    fn locate_slot_mut(&mut self, k: &K) -> (usize, Option<&mut V>);
    fn try_delete(&mut self, k: &K) -> LeafDeleteResult<K, V>;
    fn delete_at(&mut self, idx: usize) -> (K, V);
    fn delete_with_push_front(&mut self, idx: usize, item: (K, V)) -> (K, V);
    fn delete_with_push(&mut self, idx: usize, item: (K, V)) -> (K, V);
    fn merge_right_delete_first(&mut self, delete_idx_in_next: usize, right: &mut Self) -> (K, V);
    fn merge_right(&mut self, right: &mut Self);
    fn pop(&mut self) -> (K, V);
    fn pop_front(&mut self) -> (K, V);
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::*;

    #[test]
    fn test_round_trip_100() {
        for _ in 0..100 {
            test_round_trip();
        }
    }

    #[test]
    fn test_round_trip() {
        use rand::seq::SliceRandom;

        let node_store = NodeStoreVec::<i64, i64, 8, 9, 6>::new();
        let mut tree = BPlusTree::new(node_store);

        let size: i64 = 50;

        let mut keys = (0..size).collect::<Vec<_>>();
        keys.shuffle(&mut rand::thread_rng());

        for i in keys {
            tree.insert(i, i % 13);
        }
        tree.node_store.debug();

        let mut keys = (0..size).collect::<Vec<_>>();
        keys.shuffle(&mut rand::thread_rng());
        for i in keys {
            assert_eq!(*tree.get(&i).unwrap(), i % 13);
        }

        let mut keys = (0..size).collect::<Vec<_>>();
        keys.shuffle(&mut rand::thread_rng());

        for i in keys {
            let k = i;

            let delete_result = tree.remove(&k);
            assert!(delete_result.is_some());
        }

        assert!(tree.is_empty());
    }

    #[test]
    fn test_first_leaf() {
        let node_store = NodeStoreVec::<i64, i64, 8, 9, 6>::new();
        let mut tree = BPlusTree::new(node_store);
        let size: i64 = 500;
        let keys = (0..size).collect::<Vec<_>>();
        for i in keys {
            tree.insert(i, i % 13);
        }

        let first_leaf_id = tree.first_leaf().unwrap();
        let first_leaf = tree.node_store.get_leaf(first_leaf_id);
        assert_eq!(first_leaf.data_at(0).0.clone(), 0);
    }

    #[test]
    fn test_rotate_right() {
        let mut node_store = NodeStoreVec::<i64, i64, 4, 5, 4>::new();
        let parent_id = node_store.new_empty_inner();
        let child_0 = node_store.new_empty_inner();
        let child_1 = node_store.new_empty_inner();
        let child_2 = node_store.new_empty_inner();
        let child_3 = node_store.new_empty_inner();

        let parent_node = node_store.get_mut_inner(parent_id);
        parent_node.set_data([10, 30, 50], [child_0, child_1, child_2, child_3]);

        node_store.get_mut_inner(child_1).set_data(
            [10, 11, 12, 13],
            [
                LeafNodeId(1),
                LeafNodeId(2),
                LeafNodeId(3),
                LeafNodeId(4),
                LeafNodeId(5),
            ],
        );

        node_store
            .get_mut_inner(child_2)
            .set_data([40, 41], [LeafNodeId(6), LeafNodeId(7), LeafNodeId(8)]);

        let mut parent = node_store.take_inner(parent_id);
        assert!(
            BPlusTree::try_rotate_right_for_inner_node(&mut node_store, &mut parent, 1).is_some()
        );
        node_store.put_back_inner(parent_id, parent);

        {
            let parent = node_store.get_inner(parent_id);
            assert_eq!(parent.key(1).clone(), 13);
        }

        {
            let child_1 = node_store.get_inner(child_1);
            assert_eq!(child_1.size(), 3);
            assert_eq!(child_1.key_vec(), vec![10, 11, 12]);
            assert_eq!(
                child_1.child_id_vec(),
                vec![
                    LeafNodeId(1).into(),
                    LeafNodeId(2).into(),
                    LeafNodeId(3).into(),
                    LeafNodeId(4).into(),
                ]
            );
        }

        {
            let child_2 = node_store.get_inner(child_2);

            assert_eq!(child_2.size(), 3);

            assert_eq!(child_2.key_vec(), vec![30, 40, 41]);
            assert_eq!(
                child_2.child_id_vec(),
                vec![
                    LeafNodeId(5).into(),
                    LeafNodeId(6).into(),
                    LeafNodeId(7).into(),
                    LeafNodeId(8).into(),
                ]
            );
        }
    }

    #[test]
    fn test_rotate_left() {
        let mut node_store = NodeStoreVec::<i64, i64, 4, 5, 4>::new();
        let parent_id = node_store.new_empty_inner();
        let child_0 = node_store.new_empty_inner();
        let child_1 = node_store.new_empty_inner();
        let child_2 = node_store.new_empty_inner();
        let child_3 = node_store.new_empty_inner();

        node_store
            .get_mut_inner(parent_id)
            .set_data([10, 30, 50], [child_0, child_1, child_2, child_3]);

        node_store.get_mut_inner(child_1).set_data(
            [10, 11, 12],
            [LeafNodeId(1), LeafNodeId(2), LeafNodeId(3), LeafNodeId(4)],
        );

        node_store.get_mut_inner(child_2).set_data(
            [39, 40, 41],
            [LeafNodeId(5), LeafNodeId(6), LeafNodeId(7), LeafNodeId(8)],
        );

        let mut parent = node_store.take_inner(parent_id);
        assert!(
            BPlusTree::try_rotate_left_for_inner_node(&mut node_store, &mut parent, 1).is_some()
        );
        node_store.put_back_inner(parent_id, parent);

        {
            let parent = node_store.get_inner(parent_id);
            assert_eq!(parent.key(1).clone(), 39);
        }

        {
            let child_1 = node_store.get_inner(child_1);
            assert_eq!(child_1.size(), 4);
            assert_eq!(child_1.key_vec(), vec![10, 11, 12, 30]);
            assert_eq!(
                child_1.child_id_vec(),
                vec![
                    LeafNodeId(1).into(),
                    LeafNodeId(2).into(),
                    LeafNodeId(3).into(),
                    LeafNodeId(4).into(),
                    LeafNodeId(5).into(),
                ]
            );
        }

        {
            let child_2 = node_store.get_inner(child_2);

            assert_eq!(child_2.size(), 2);

            assert_eq!(child_2.key_vec(), vec![40, 41]);
            assert_eq!(
                child_2.child_id_vec(),
                vec![
                    LeafNodeId(6).into(),
                    LeafNodeId(7).into(),
                    LeafNodeId(8).into(),
                ]
            );
        }
    }

    #[test]
    fn test_merge() {
        let mut node_store = NodeStoreVec::<i64, i64, 4, 5, 4>::new();
        let parent_id = node_store.new_empty_inner();
        let child_0 = node_store.new_empty_inner();
        let child_1 = node_store.new_empty_inner();
        let child_2 = node_store.new_empty_inner();
        let child_3 = node_store.new_empty_inner();

        node_store
            .get_mut_inner(parent_id)
            .set_data([10, 30, 50], [child_0, child_1, child_2, child_3]);
        node_store
            .get_mut_inner(child_1)
            .set_data([10, 11], [LeafNodeId(1), LeafNodeId(2), LeafNodeId(3)]);
        node_store
            .get_mut_inner(child_2)
            .set_data([40], [LeafNodeId(5), LeafNodeId(6)]);

        let mut parent = node_store.take_inner(parent_id);
        let _result = BPlusTree::merge_inner_node(&mut node_store, &mut parent, 1);
        node_store.put_back_inner(parent_id, parent);

        {
            let parent = node_store.get_inner(parent_id);
            assert_eq!(parent.size(), 2);
            assert_eq!(parent.key(1).clone(), 50);
            assert_eq!(
                parent.child_id_vec(),
                vec![child_0.into(), child_1.into(), child_3.into(),]
            );
        }

        {
            let child_1 = node_store.get_inner(child_1);
            assert_eq!(child_1.size(), 4);
            assert_eq!(child_1.key_vec(), vec![10, 11, 30, 40]);
            assert_eq!(
                child_1.child_id_vec(),
                vec![
                    LeafNodeId(1).into(),
                    LeafNodeId(2).into(),
                    LeafNodeId(3).into(),
                    LeafNodeId(5).into(),
                    LeafNodeId(6).into(),
                ]
            );
        }

        {
            assert!(node_store.try_get_inner(child_2).is_none());
        }
    }

    #[test]
    fn test_rotate_right_for_leaf() {
        let mut node_store = NodeStoreVec::<i64, i64, 4, 5, 4>::new();
        let parent_id = node_store.new_empty_inner();
        let (child_0, _) = node_store.new_empty_leaf();
        let (child_1, _) = node_store.new_empty_leaf();
        let (child_2, _) = node_store.new_empty_leaf();
        let (child_3, _) = node_store.new_empty_leaf();

        node_store
            .get_mut_inner(parent_id)
            .set_data([10, 30, 50], [child_0, child_1, child_2, child_3]);

        node_store
            .get_mut_leaf(child_1)
            .set_data([(10, 1), (11, 1), (12, 1), (13, 1)]);

        node_store
            .get_mut_leaf(child_2)
            .set_data([(40, 1), (41, 1)]);

        let mut parent = node_store.take_inner(parent_id);
        BPlusTree::try_rotate_right_for_leaf_node(&mut node_store, &mut parent, 1, 0);
        node_store.put_back_inner(parent_id, parent);

        {
            let parent = node_store.get_inner(parent_id);
            assert_eq!(parent.key(1).clone(), 13);
        }

        {
            let child_1 = node_store.get_leaf(child_1);
            assert_eq!(child_1.len(), 3);
            assert_eq!(child_1.data_vec(), vec![(10, 1), (11, 1), (12, 1)]);
        }

        {
            let child_2 = node_store.get_leaf(child_2);
            assert_eq!(child_2.len(), 2);

            assert_eq!(child_2.data_vec(), vec![(13, 1), (41, 1)]);
        }
    }

    #[test]
    fn test_rotate_left_for_leaf() {
        let mut node_store = NodeStoreVec::<i64, i64, 4, 5, 4>::new();
        let parent_id = node_store.new_empty_inner();
        let (child_0, _) = node_store.new_empty_leaf();
        let (child_1, _) = node_store.new_empty_leaf();
        let (child_2, _) = node_store.new_empty_leaf();
        let (child_3, _) = node_store.new_empty_leaf();

        node_store
            .get_mut_inner(parent_id)
            .set_data([10, 30, 50], [child_0, child_1, child_2, child_3]);
        node_store
            .get_mut_leaf(child_1)
            .set_data([(10, 1), (11, 1), (12, 1)]);
        node_store
            .get_mut_leaf(child_2)
            .set_data([(39, 1), (40, 1), (41, 1)]);

        let mut parent = node_store.take_inner(parent_id);
        let result = BPlusTree::rotate_left_for_leaf_node(&mut node_store, &mut parent, 1, 0);
        node_store.put_back_inner(parent_id, parent);
        assert_eq!(result.0 .0, 10);

        {
            let parent = node_store.get_inner(parent_id);
            assert_eq!(parent.key(1).clone(), 40);
        }

        {
            let child_1 = node_store.get_leaf(child_1);
            assert_eq!(child_1.len(), 3);
            assert_eq!(child_1.data_vec(), vec![(11, 1), (12, 1), (39, 1),]);
        }

        {
            let child_2 = node_store.get_leaf(child_2);
            assert_eq!(child_2.len(), 2);

            assert_eq!(child_2.data_vec(), vec![(40, 1), (41, 1)]);
        }
    }

    #[test]
    fn test_merge_leaf_with_right() {
        let mut node_store = NodeStoreVec::<i64, i64, 4, 5, 4>::new();
        let parent_id = node_store.new_empty_inner();
        let (child_0, _) = node_store.new_empty_leaf();
        let (child_1, _) = node_store.new_empty_leaf();
        let (child_2, _) = node_store.new_empty_leaf();
        let (child_3, _) = node_store.new_empty_leaf();

        node_store
            .get_mut_inner(parent_id)
            .set_data([10, 30, 50], [child_0, child_1, child_2, child_3]);
        node_store
            .get_mut_leaf(child_1)
            .set_data([(10, 1), (11, 1)]);
        node_store
            .get_mut_leaf(child_2)
            .set_data([(39, 1), (40, 1)]);

        let mut parent = node_store.take_inner(parent_id);
        let _result = BPlusTree::merge_leaf_node_with_right(&mut node_store, &mut parent, 1, 0);
        node_store.put_back_inner(parent_id, parent);
        {
            let parent = node_store.get_inner(parent_id);
            assert_eq!(parent.key(1).clone(), 50);
        }

        {
            let child_1 = node_store.get_leaf(child_1);
            assert_eq!(child_1.len(), 3);
            assert_eq!(child_1.data_vec(), vec![(11, 1), (39, 1), (40, 1),]);
        }

        assert!(node_store.try_get_leaf(child_2).is_none());
    }

    #[test]
    fn test_merge_leaf_with_left() {
        let mut node_store = NodeStoreVec::<i64, i64, 4, 5, 4>::new();
        let parent_id = node_store.new_empty_inner();
        let (child_0, _) = node_store.new_empty_leaf();
        let (child_1, _) = node_store.new_empty_leaf();
        let (child_2, _) = node_store.new_empty_leaf();
        let (child_3, _) = node_store.new_empty_leaf();

        node_store
            .get_mut_inner(parent_id)
            .set_data([10, 30, 50], [child_0, child_1, child_2, child_3]);
        node_store
            .get_mut_leaf(child_1)
            .set_data([(10, 1), (11, 1)]);
        node_store
            .get_mut_leaf(child_2)
            .set_data([(39, 1), (40, 1)]);

        let mut parent = node_store.take_inner(parent_id);
        let _result = BPlusTree::merge_leaf_node_left(&mut node_store, &mut parent, 1, 0);
        node_store.put_back_inner(parent_id, parent);
        {
            let parent = node_store.get_inner(parent_id);
            assert_eq!(parent.key(1).clone(), 50);
        }

        {
            let child_1 = node_store.get_leaf(child_1);
            assert_eq!(child_1.len(), 3);
            assert_eq!(child_1.data_vec(), vec![(10, 1), (11, 1), (40, 1),]);
        }

        assert!(node_store.try_get_leaf(child_2).is_none());
    }

    #[test]
    fn test_modify_value() {
        let (mut tree, _) = create_test_tree::<30>();
        let v = tree.get_mut(&1).unwrap();
        *v = 100;
        assert_eq!(tree.get(&1).unwrap().clone(), 100);
    }

    #[test]
    fn test_cursor() {
        let (mut tree, _) = create_test_tree::<30>();

        let (cursor, _kv) = tree.get_cursor(&10).unwrap();
        assert_eq!(cursor.key().clone(), 10);
        assert_eq!(cursor.value(&tree).unwrap().clone(), 10);

        {
            let prev = cursor.prev(&tree).unwrap();
            assert_eq!(prev.key().clone(), 9);

            let next = cursor.next(&tree).unwrap();
            assert_eq!(next.key().clone(), 11);
        }

        tree.remove(&10);

        {
            assert_eq!(cursor.key().clone(), 10);
            assert!(cursor.value(&tree).is_none());

            let prev = cursor.prev(&tree).unwrap();
            assert_eq!(prev.key().clone(), 9);

            let next = cursor.next(&tree).unwrap();
            assert_eq!(next.key().clone(), 11);
        }

        let (cursor, kv) = tree.get_cursor(&10).unwrap();
        assert_eq!(cursor.key().clone(), 10);
        assert!(kv.is_none());
    }

    pub fn create_test_tree<const N: usize>(
    ) -> (BPlusTree<NodeStoreVec<i64, i64, 8, 9, 6>>, Vec<i64>) {
        use rand::seq::SliceRandom;

        let node_store = NodeStoreVec::<i64, i64, 8, 9, 6>::new();
        let mut tree = BPlusTree::new(node_store);

        let size: i64 = N as i64;

        let mut keys = (0..size).collect::<Vec<_>>();
        keys.shuffle(&mut rand::thread_rng());

        // println!("{:?}", keys);

        for i in keys.iter() {
            tree.insert(*i, i % 13);
        }

        assert_eq!(tree.len(), N);

        (tree, keys)
    }

    #[derive(Clone)]
    struct TestValue {
        counter: Rc<std::sync::atomic::AtomicU64>,
    }

    impl TestValue {
        fn new(counter: Rc<std::sync::atomic::AtomicU64>) -> Self {
            Self { counter }
        }
    }

    impl Drop for TestValue {
        fn drop(&mut self) {
            self.counter
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[test]
    fn test_drop() {
        // test drop
        let node_store = NodeStoreVec::<i64, TestValue, 8, 9, 6>::new();
        let mut tree = BPlusTree::new(node_store);
        let counter = Rc::new(std::sync::atomic::AtomicU64::new(0));
        for i in 0..10 {
            tree.insert(i, TestValue::new(counter.clone()));
        }
        drop(tree);

        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 10);
    }
}
