use crate::{INode, InnerNode, InnerNodeId, Key, LNode, LeafNode, LeafNodeId, NodeStore, Value};

#[derive(Debug, Clone)]
pub struct NodeStoreVec<K: Key, V: Value, const IN: usize, const IC: usize, const LN: usize> {
    inner_nodes: Vec<InnerNode<K, IN, IC>>,
    leaf_nodes: Vec<LeafNode<K, V, LN>>,
}

impl<K: Key, V: Value, const IN: usize, const IC: usize, const LN: usize>
    NodeStoreVec<K, V, IN, IC, LN>
{
    /// Create a new `NodeStoreVec`
    pub fn new() -> Self {
        Self {
            inner_nodes: Vec::with_capacity(32),
            leaf_nodes: Vec::with_capacity(128),
        }
    }

    pub fn print(&self) {
        for (idx, inner) in self.inner_nodes.iter().enumerate() {
            println!(
                "inner: {idx} s:{} key: {:?} child: {:?}",
                inner.size(),
                inner.iter_key().collect::<Vec<_>>(),
                inner.iter_child().collect::<Vec<_>>()
            );
        }

        for (idx, leaf) in self.leaf_nodes.iter().enumerate() {
            println!(
                "leaf: {idx} p:{:?} n:{:?} items:{:?}",
                leaf.prev()
                    .map(|l| l.as_usize().to_string())
                    .unwrap_or("-".to_string()),
                leaf.next()
                    .map(|l| l.as_usize().to_string())
                    .unwrap_or("-".to_string()),
                leaf.iter().map(|kv| kv.0).collect::<Vec<_>>()
            );
        }
    }
}

impl<K: Key, V: Value, const IN: usize, const IC: usize, const LN: usize> NodeStore
    for NodeStoreVec<K, V, IN, IC, LN>
{
    type K = K;
    type V = V;
    type InnerNode = InnerNode<K, IN, IC>;
    type LeafNode = LeafNode<K, V, LN>;

    fn new_empty_inner(&mut self) -> InnerNodeId {
        let id = InnerNodeId::from_usize(self.inner_nodes.len());
        let node = Self::InnerNode::default();
        self.inner_nodes.push(node);
        id
    }

    fn add_inner(&mut self, node: Self::InnerNode) -> InnerNodeId {
        let id = InnerNodeId::from_usize(self.inner_nodes.len());
        self.inner_nodes.push(node);
        id
    }

    fn get_inner(&self, id: InnerNodeId) -> &Self::InnerNode {
        &self.inner_nodes[id.as_usize()]
    }

    fn get_mut_inner(&mut self, id: InnerNodeId) -> &mut Self::InnerNode {
        &mut self.inner_nodes[id.as_usize()]
    }

    fn create_leaf(&mut self) -> (LeafNodeId, &mut Self::LeafNode) {
        let id = LeafNodeId::from_u32(self.leaf_nodes.len());
        let node = Self::LeafNode::default();
        self.leaf_nodes.push(node);
        (id, &mut self.leaf_nodes[id.as_usize()])
    }

    fn get_leaf(&self, id: LeafNodeId) -> &Self::LeafNode {
        &self.leaf_nodes[id.as_usize()]
    }

    fn try_get_leaf(&self, id: LeafNodeId) -> Option<&Self::LeafNode> {
        let leaf_node = self.leaf_nodes.get(id.as_usize())?;
        if leaf_node.len() == 0 {
            None
        } else {
            Some(leaf_node)
        }
    }

    fn get_mut_leaf(&mut self, id: LeafNodeId) -> &mut Self::LeafNode {
        &mut self.leaf_nodes[id.as_usize()]
    }

    fn debug(&self) {
        self.print()
    }

    fn take_leaf(&mut self, id: LeafNodeId) -> Self::LeafNode {
        std::mem::take(&mut self.leaf_nodes[id.as_usize()])
    }

    fn take_inner(&mut self, id: InnerNodeId) -> Self::InnerNode {
        std::mem::take(&mut self.inner_nodes[id.as_usize()])
    }
}
