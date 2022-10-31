use futures::stream::BoxStream;
use std::sync::Arc;

use nimiq_block::{Block, MacroBlock};
use nimiq_blockchain::ChainInfo;
use nimiq_blockchain::{AbstractBlockchain, Blockchain, BlockchainEvent};
use nimiq_database::Transaction;
use nimiq_genesis::NetworkId;
use nimiq_hash::Blake2bHash;
use nimiq_primitives::slots::{Validator, Validators};
use parking_lot::{RwLock, RwLockReadGuard};

macro_rules! gen_blockchain_match {
    ($self: ident, $t: ident, $f: ident $(, $arg:expr )*) => {
        match $self {
            $t::Full(ref blockchain) => AbstractBlockchain::$f(&***blockchain, $( $arg ),*),
        }
    };
}

/// The `BlockchainProxy` is our abstraction over multiple types of blockchains.
/// Currently, it holds the:
/// - (Full)Blockchain, which is capable of storing the full history, transactions, and full blocks.
pub enum BlockchainProxy {
    /// (Full)Blockchain, stores the full history, transactions, and full blocks.
    Full(Arc<RwLock<Blockchain>>),
}

impl Clone for BlockchainProxy {
    fn clone(&self) -> Self {
        match self {
            Self::Full(blockchain) => Self::Full(Arc::clone(blockchain)),
        }
    }
}

impl From<Arc<RwLock<Blockchain>>> for BlockchainProxy {
    fn from(blockchain: Arc<RwLock<Blockchain>>) -> Self {
        Self::Full(blockchain)
    }
}

impl<'a> From<&'a Arc<RwLock<Blockchain>>> for BlockchainProxy {
    fn from(blockchain: &'a Arc<RwLock<Blockchain>>) -> Self {
        Self::Full(Arc::clone(blockchain))
    }
}

impl BlockchainProxy {
    /// Returns a wrapper/proxy around a read locked blockchain.
    /// The `BlockchainReadProxy` implements `AbstractBlockchain` and allows to access common blockchain functions.
    pub fn read(&self) -> BlockchainReadProxy {
        match self {
            BlockchainProxy::Full(blockchain) => {
                BlockchainReadProxy::Full(Arc::new(blockchain.read()))
            }
        }
    }
}

/// The `BlockchainReadProxy` implements `AbstractBlockchain` and allows to access common blockchain functions.
/// It is a wrapper around read locked versions of our blockchain types.
pub enum BlockchainReadProxy<'a> {
    Full(Arc<RwLockReadGuard<'a, Blockchain>>),
}

impl<'a> AbstractBlockchain for BlockchainReadProxy<'a> {
    fn network_id(&self) -> NetworkId {
        gen_blockchain_match!(self, BlockchainReadProxy, network_id)
    }

    fn now(&self) -> u64 {
        gen_blockchain_match!(self, BlockchainReadProxy, now)
    }

    fn head(&self) -> Block {
        gen_blockchain_match!(self, BlockchainReadProxy, head)
    }

    fn macro_head(&self) -> MacroBlock {
        gen_blockchain_match!(self, BlockchainReadProxy, macro_head)
    }

    fn election_head(&self) -> MacroBlock {
        gen_blockchain_match!(self, BlockchainReadProxy, election_head)
    }

    fn current_validators(&self) -> Option<Validators> {
        gen_blockchain_match!(self, BlockchainReadProxy, current_validators)
    }

    fn previous_validators(&self) -> Option<Validators> {
        gen_blockchain_match!(self, BlockchainReadProxy, previous_validators)
    }

    fn contains(&self, hash: &Blake2bHash, include_forks: bool) -> bool {
        gen_blockchain_match!(self, BlockchainReadProxy, contains, hash, include_forks)
    }

    fn get_block_at(
        &self,
        height: u32,
        include_body: bool,
        txn_option: Option<&Transaction>,
    ) -> Option<Block> {
        gen_blockchain_match!(
            self,
            BlockchainReadProxy,
            get_block_at,
            height,
            include_body,
            txn_option
        )
    }

    fn get_block(
        &self,
        hash: &Blake2bHash,
        include_body: bool,
        txn_option: Option<&Transaction>,
    ) -> Option<Block> {
        gen_blockchain_match!(
            self,
            BlockchainReadProxy,
            get_block,
            hash,
            include_body,
            txn_option
        )
    }

    fn get_blocks(
        &self,
        start_block_hash: &Blake2bHash,
        count: u32,
        include_body: bool,
        direction: nimiq_blockchain::Direction,
        txn_option: Option<&Transaction>,
    ) -> Vec<Block> {
        gen_blockchain_match!(
            self,
            BlockchainReadProxy,
            get_blocks,
            start_block_hash,
            count,
            include_body,
            direction,
            txn_option
        )
    }

    fn get_chain_info(
        &self,
        hash: &Blake2bHash,
        include_body: bool,
        txn_option: Option<&Transaction>,
    ) -> Option<ChainInfo> {
        gen_blockchain_match!(
            self,
            BlockchainReadProxy,
            get_chain_info,
            hash,
            include_body,
            txn_option
        )
    }

    fn get_slot_owner_at(
        &self,
        block_number: u32,
        offset: u32,
        txn_option: Option<&Transaction>,
    ) -> Option<(Validator, u16)> {
        gen_blockchain_match!(
            self,
            BlockchainReadProxy,
            get_slot_owner_at,
            block_number,
            offset,
            txn_option
        )
    }

    fn notifier_as_stream(&self) -> BoxStream<'static, BlockchainEvent> {
        gen_blockchain_match!(self, BlockchainReadProxy, notifier_as_stream)
    }
}
