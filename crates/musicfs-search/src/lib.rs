mod collections;
mod index;
mod indexer;
mod query;

pub use collections::{
    builtin_collections, BoolOp, CollectionError, CollectionQuery, CollectionStore, SmartCollection,
};
pub use index::{SearchError, SearchHit, SearchIndex};
pub use indexer::{Indexer, IndexerHandle, MetadataLookup};
pub use query::SearchQueryBuilder;
