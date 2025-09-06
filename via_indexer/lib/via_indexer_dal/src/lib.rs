//! Data access layer (DAL) for ZKsync Era.

// Linter settings.
#![warn(clippy::cast_lossless)]

pub use sqlx::{types::BigDecimal, Error as SqlxError};
use via_transactions_dal::ViaTransactionsDal;
use zksync_db_connection::connection::DbMarker;
pub use zksync_db_connection::{
    connection::{Connection, IsolationLevel},
    connection_pool::{ConnectionPool, ConnectionPoolBuilder},
    error::{DalError, DalResult},
};

use crate::{via_indexer_dal::ViaIndexerDal, via_wallet_dal::ViaWalletDal};

pub mod models;
pub mod via_indexer_dal;
pub mod via_transactions_dal;
pub mod via_wallet_dal;

// This module is private and serves as a way to seal the trait.
mod private {
    pub trait Sealed {}
}

// Here we are making the trait sealed, because it should be public to function correctly, but we don't
// want to allow any other downstream implementations of this trait.
pub trait IndexerDal<'a>: private::Sealed
where
    Self: 'a,
{
    fn via_transactions_dal(&mut self) -> ViaTransactionsDal<'_, 'a>;
    fn via_indexer_dal(&mut self) -> ViaIndexerDal<'_, 'a>;
    fn via_wallet_dal(&mut self) -> ViaWalletDal<'_, 'a>;
}

#[derive(Clone, Debug)]
pub struct Indexer;

// Implement the marker trait for the Core to be able to use it in Connection.
impl DbMarker for Indexer {}
// Implement the sealed trait for the struct itself.
impl private::Sealed for Connection<'_, Indexer> {}

impl<'a> IndexerDal<'a> for Connection<'a, Indexer> {
    fn via_transactions_dal(&mut self) -> ViaTransactionsDal<'_, 'a> {
        ViaTransactionsDal { storage: self }
    }

    fn via_indexer_dal(&mut self) -> ViaIndexerDal<'_, 'a> {
        ViaIndexerDal { storage: self }
    }

    fn via_wallet_dal(&mut self) -> ViaWalletDal<'_, 'a> {
        ViaWalletDal { storage: self }
    }
}
