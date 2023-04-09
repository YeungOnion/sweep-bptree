use std::borrow::Borrow;

use crate::{BPlusTree, Key, NodeStoreVec};

/// A B+ tree based set
pub struct BPlusTreeSet<K: crate::Key> {
    tree: BPlusTree<NodeStoreVec<K, (), 64, 65, 64>>,
}

impl<K: Key> BPlusTreeSet<K> {
    /// Create a new BPlusTreeSet
    ///
    /// # Examples
    /// ```rust
    /// use sweep_bptree::BPlusTreeSet;
    ///
    /// BPlusTreeSet::<i32>::new();
    /// ```
    pub fn new() -> Self {
        let store = NodeStoreVec::new();

        Self {
            tree: BPlusTree::new(store),
        }
    }

    /// Returns key count in the set
    ///
    /// # Examples
    /// ```rust
    /// use sweep_bptree::BPlusTreeSet;
    ///
    /// let mut set = BPlusTreeSet::<i32>::new();
    /// assert_eq!(set.len(), 0);

    /// set.insert(1);
    /// assert_eq!(set.len(), 1);
    ///
    /// set.insert(1);
    /// assert_eq!(set.len(), 1);
    /// ```
    pub fn len(&self) -> usize {
        self.tree.len()
    }

    /// Returns true if the set contains no key
    ///
    /// # Examples
    /// ```rust
    /// use sweep_bptree::BPlusTreeSet;
    ///
    /// let mut set = BPlusTreeSet::<i32>::new();
    /// assert!(set.is_empty());
    ///
    /// set.insert(1);
    /// assert!(!set.is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        self.tree.is_empty()
    }

    /// Insert a key into the set
    /// Returns true if the key was inserted, false if it already existed
    ///
    /// # Examples
    /// ```rust
    /// use sweep_bptree::BPlusTreeSet;
    ///
    /// let mut set = BPlusTreeSet::<i32>::new();
    /// assert!(set.insert(1));
    /// assert!(!set.insert(1));
    /// ```
    pub fn insert(&mut self, k: impl Into<K>) -> bool {
        self.tree.insert(k.into(), ()).is_none()
    }

    /// Remove a key from the set
    /// Returns true if the key was removed, false if it didn't exist
    ///
    /// # Examples
    /// ```rust
    /// use sweep_bptree::BPlusTreeSet;
    ///
    /// let mut set = BPlusTreeSet::<i32>::new();
    /// set.insert(1);
    /// assert!(set.remove(&1));
    /// assert!(!set.remove(&2));
    /// ```
    pub fn remove<Q: ?Sized>(&mut self, k: &Q) -> bool
    where
        Q: Ord,
        K: Borrow<Q>,
    {
        self.tree.remove(k).is_some()
    }

    /// Returns true if the set contains the key
    ///
    /// # Examples
    /// ```rust
    /// use sweep_bptree::BPlusTreeSet;
    ///
    /// let mut set = BPlusTreeSet::<i32>::new();
    /// set.insert(1);
    /// assert!(set.contains(&1));
    /// set.remove(&1);
    /// assert!(!set.contains(&1));
    /// ```
    pub fn contains<Q: ?Sized>(&self, k: &Q) -> bool
    where
        Q: Ord,
        K: Borrow<Q>,
    {
        self.tree.get(k).is_some()
    }

    /// Clears the set
    ///
    /// # Examples
    /// ```rust
    /// use sweep_bptree::BPlusTreeSet;
    ///
    /// let mut set = BPlusTreeSet::<i32>::new();
    /// set.insert(1);
    /// set.insert(2);
    ///
    /// set.clear();
    /// assert!(set.is_empty());
    /// ```
    pub fn clear(&mut self) {
        self.tree.clear();
    }

    /// Returns a reference to the first key in the set, if any
    ///
    /// # Examples
    /// ```rust
    /// use sweep_bptree::BPlusTreeSet;
    ///
    /// let mut set = BPlusTreeSet::<i32>::new();
    ///
    /// assert!(set.first().is_none());
    ///
    /// set.insert(1);
    /// set.insert(2);
    ///
    /// assert_eq!(*set.first().unwrap(), 1);
    /// ```
    pub fn first(&self) -> Option<&K> {
        self.tree.first().map(|(k, _)| k)
    }

    /// Returns a reference to the last key in the set, if any
    ///
    /// # Examples
    /// ```rust
    /// use sweep_bptree::BPlusTreeSet;
    ///
    /// let mut set = BPlusTreeSet::<i32>::new();
    ///
    /// assert!(set.last().is_none());
    ///
    /// set.insert(1);
    /// set.insert(2);
    ///
    /// assert_eq!(*set.last().unwrap(), 2);
    /// ```
    pub fn last(&self) -> Option<&K> {
        self.tree.last().map(|(k, _)| k)
    }

    /// Returns a iterator over the keys in the set
    ///
    /// # Examples
    /// ```rust
    /// use sweep_bptree::BPlusTreeSet;
    ///
    /// let mut set = BPlusTreeSet::<i32>::new();
    ///
    /// set.insert(1);
    /// set.insert(2);
    ///
    /// let keys = set.iter().collect::<Vec<_>>();
    /// assert_eq!(keys.len(), 2);
    ///
    /// ```
    pub fn iter(&self) -> iter::Iter<K> {
        iter::Iter {
            inner: self.tree.iter(),
        }
    }

    /// Returns a iterator over the keys in the set
    ///
    /// # Examples
    /// ```rust
    /// use sweep_bptree::BPlusTreeSet;
    ///
    /// let mut set = BPlusTreeSet::<String>::new();
    ///
    /// set.insert(1.to_string());
    /// set.insert(2.to_string());
    ///
    /// let keys = set.into_iter().collect::<Vec<_>>();
    /// assert_eq!(keys.len(), 2);
    ///
    /// ```
    pub fn into_iter(self) -> iter::IntoIter<K> {
        iter::IntoIter {
            inner: self.tree.into_iter(),
        }
    }
}

pub mod iter {
    use super::*;

    pub struct Iter<'a, K: crate::Key> {
        pub(super) inner: crate::tree::Iter<'a, NodeStoreVec<K, (), 64, 65, 64>>,
    }

    impl<'a, K: crate::Key> Iterator for Iter<'a, K> {
        type Item = &'a K;

        fn next(&mut self) -> Option<Self::Item> {
            self.inner.next().map(|(k, _)| k)
        }
    }

    impl<'a, K: crate::Key> DoubleEndedIterator for Iter<'a, K> {
        fn next_back(&mut self) -> Option<Self::Item> {
            self.inner.next_back().map(|(k, _)| k)
        }
    }

    pub struct IntoIter<K: crate::Key> {
        pub(super) inner: crate::tree::IntoIter<NodeStoreVec<K, (), 64, 65, 64>>,
    }

    impl<K: crate::Key> Iterator for IntoIter<K> {
        type Item = K;

        fn next(&mut self) -> Option<Self::Item> {
            self.inner.next().map(|(k, _)| k)
        }
    }

    impl<K: crate::Key> DoubleEndedIterator for IntoIter<K> {
        fn next_back(&mut self) -> Option<Self::Item> {
            self.inner.next_back().map(|(k, _)| k)
        }
    }
}
