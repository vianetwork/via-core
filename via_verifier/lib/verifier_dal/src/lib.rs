//! Data access layer (DAL) for ZKsync Era.

// Linter settings.
#![warn(clippy::cast_lossless)]

pub use sqlx::{types::BigDecimal, Error as SqlxError};
use zksync_db_connection::connection::DbMarker;
pub use zksync_db_connection::{
    connection::{Connection, IsolationLevel},
    connection_pool::{ConnectionPool, ConnectionPoolBuilder},
    error::{DalError, DalResult},
};

use crate::via_votes_dal::ViaVotesDal;

pub mod via_votes_dal;

#[cfg(test)]
mod tests;

// This module is private and serves as a way to seal the trait.
mod private {
    pub trait Sealed {}
}

// Here we are making the trait sealed, because it should be public to function correctly, but we don't
// want to allow any other downstream implementations of this trait.
pub trait VerifierDal<'a>: private::Sealed
where
    Self: 'a,
{
    fn via_votes_dal(&mut self) -> ViaVotesDal<'_, 'a>;
}

#[derive(Clone, Debug)]
pub struct Verifier;

// Implement the marker trait for the Core to be able to use it in Connection.
impl DbMarker for Verifier {}
// Implement the sealed trait for the struct itself.
impl private::Sealed for Connection<'_, Verifier> {}

impl<'a> VerifierDal<'a> for Connection<'a, Verifier> {
    fn via_votes_dal(&mut self) -> ViaVotesDal<'_, 'a> {
        ViaVotesDal { storage: self }
    }
}
