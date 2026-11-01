//! Stopping a query that runs too long.
//!
//! The engine runs a query against an in-memory store whose futures are
//! always ready, so it never yields and an executor's timeout can't
//! interrupt it. Instead, [Bounded] checks the clock on every call the
//! engine makes into the store, and fails each one once the deadline has
//! passed, which stops the query where it is. Matching a pattern reads the
//! store at every step, so even a pattern whose matches grow exponentially
//! stops soon after its deadline.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Instant;

use slcr_gql::Value;
use slcr_gql::store::{
    EdgeId, EdgeRef, GraphStore, Incidence, NodeId, StoreError, StoreErrorKind, StoreResult,
    async_trait,
};

/// A store that fails every call once `deadline` has passed.
pub(super) struct Bounded<'a> {
    store: &'a dyn GraphStore,
    deadline: Instant,
}

impl<'a> Bounded<'a> {
    pub(super) fn new(store: &'a dyn GraphStore, deadline: Instant) -> Self {
        Self { store, deadline }
    }

    fn check(&self) -> StoreResult<()> {
        if Instant::now() < self.deadline {
            Ok(())
        } else {
            Err(StoreError::new(StoreErrorKind::Other, DeadlineReached))
        }
    }
}

/// The source of the [StoreError] a [Bounded] store fails with.
#[derive(Debug)]
pub(super) struct DeadlineReached;

impl fmt::Display for DeadlineReached {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the query's time limit was reached")
    }
}

impl std::error::Error for DeadlineReached {}

#[async_trait]
impl GraphStore for Bounded<'_> {
    async fn nodes(&self) -> StoreResult<Vec<NodeId>> {
        self.check()?;
        self.store.nodes().await
    }

    async fn nodes_with_label(&self, label: &str) -> StoreResult<Vec<NodeId>> {
        self.check()?;
        self.store.nodes_with_label(label).await
    }

    async fn node_labels(&self, node: NodeId) -> StoreResult<Vec<String>> {
        self.check()?;
        self.store.node_labels(node).await
    }

    async fn node_has_label(&self, node: NodeId, label: &str) -> StoreResult<bool> {
        self.check()?;
        self.store.node_has_label(node, label).await
    }

    async fn node_property(&self, node: NodeId, key: &str) -> StoreResult<Option<Value>> {
        self.check()?;
        self.store.node_property(node, key).await
    }

    async fn node_properties(&self, node: NodeId) -> StoreResult<BTreeMap<String, Value>> {
        self.check()?;
        self.store.node_properties(node).await
    }

    async fn edges(&self, node: NodeId, incidence: Incidence) -> StoreResult<Vec<EdgeRef>> {
        self.check()?;
        self.store.edges(node, incidence).await
    }

    async fn edge(&self, edge: EdgeId) -> StoreResult<EdgeRef> {
        self.check()?;
        self.store.edge(edge).await
    }

    async fn edge_labels(&self, edge: EdgeId) -> StoreResult<Vec<String>> {
        self.check()?;
        self.store.edge_labels(edge).await
    }

    async fn edge_has_label(&self, edge: EdgeId, label: &str) -> StoreResult<bool> {
        self.check()?;
        self.store.edge_has_label(edge, label).await
    }

    async fn edge_property(&self, edge: EdgeId, key: &str) -> StoreResult<Option<Value>> {
        self.check()?;
        self.store.edge_property(edge, key).await
    }

    async fn edge_properties(&self, edge: EdgeId) -> StoreResult<BTreeMap<String, Value>> {
        self.check()?;
        self.store.edge_properties(edge).await
    }
}

/// Whether `error` is the one a [Bounded] store fails with.
pub(super) fn is_deadline(error: &slcr_gql::Error) -> bool {
    let slcr_gql::Error::Store(error) = error else {
        return false;
    };
    std::error::Error::source(error).is_some_and(|source| source.is::<DeadlineReached>())
}
