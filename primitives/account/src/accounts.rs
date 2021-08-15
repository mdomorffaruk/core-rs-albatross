use std::collections::HashMap;

use crate::{
    Account, AccountError, AccountInherentInteraction, AccountTransactionInteraction, Inherent,
    Receipt, Receipts,
};
use nimiq_database::{
    Environment, ReadTransaction, Transaction as DBTransaction, WriteTransaction,
};
use nimiq_hash::Blake2bHash;
use nimiq_keys::Address;
use nimiq_primitives::account::AccountType;
use nimiq_transaction::{Transaction, TransactionFlags};
use nimiq_trie::key_nibbles::KeyNibbles;
use nimiq_trie::trie::MerkleRadixTrie;

/// An alias for the accounts tree.
pub type AccountsTrie = MerkleRadixTrie<Account>;

type ReceiptsMap<'a> = HashMap<u16, &'a Vec<u8>>;

#[derive(Debug)]
pub struct Accounts {
    env: Environment,
    tree: AccountsTrie,
}

impl Accounts {
    pub fn new(env: Environment) -> Self {
        let tree = AccountsTrie::new(env.clone(), "AccountsTrie");
        Accounts { env, tree }
    }

    pub fn init(&self, txn: &mut WriteTransaction, genesis_accounts: Vec<(KeyNibbles, Account)>) {
        for (key, account) in genesis_accounts {
            self.tree.put(txn, &key, account);
        }
    }

    pub fn get(&self, key: &KeyNibbles, txn_option: Option<&DBTransaction>) -> Option<Account> {
        match txn_option {
            Some(txn) => self.tree.get(txn, key),
            None => self.tree.get(&ReadTransaction::new(&self.env), key),
        }
    }

    pub fn get_root(&self, txn_option: Option<&DBTransaction>) -> Blake2bHash {
        match txn_option {
            Some(txn) => self.tree.root_hash(txn),
            None => self.tree.root_hash(&ReadTransaction::new(&self.env)),
        }
    }

    pub fn get_root_with(
        &self,
        transactions: &[Transaction],
        inherents: &[Inherent],
        block_height: u32,
        timestamp: u64,
    ) -> Result<Blake2bHash, AccountError> {
        let mut txn = WriteTransaction::new(&self.env);

        self.commit(&mut txn, transactions, inherents, block_height, timestamp)?;

        let hash = self.get_root(Some(&txn));

        txn.abort();

        Ok(hash)
    }

    pub fn commit(
        &self,
        txn: &mut WriteTransaction,
        transactions: &[Transaction],
        inherents: &[Inherent],
        block_height: u32,
        timestamp: u64,
    ) -> Result<Receipts, AccountError> {
        let mut receipts = Vec::new();

        receipts.append(&mut self.process_inherents(
            txn,
            inherents.iter().filter(|i| i.is_pre_transactions()),
            HashMap::new(),
            |account, inherent, _| {
                Account::commit_inherent(&self.tree, txn, inherent, block_height, timestamp)
            },
        )?);

        receipts.append(&mut self.process_senders(
            txn,
            transactions,
            block_height,
            timestamp,
            HashMap::new(),
            |account, transaction, block_height, _| {
                Account::commit_outgoing_transaction(
                    &self.tree,
                    txn,
                    transaction,
                    block_height,
                    timestamp,
                )
            },
        )?);

        receipts.append(&mut self.process_recipients(
            txn,
            transactions,
            block_height,
            timestamp,
            HashMap::new(),
            |account, transaction, block_height, _| {
                Account::commit_incoming_transaction(
                    &self.tree,
                    txn,
                    transaction,
                    block_height,
                    timestamp,
                )
            },
        )?);

        self.create_contracts(txn, transactions, block_height, timestamp)?;

        receipts.append(&mut self.process_inherents(
            txn,
            inherents.iter().filter(|i| !i.is_pre_transactions()),
            HashMap::new(),
            |account, inherent, _| {
                Account::commit_inherent(&self.tree, txn, inherent, block_height, timestamp)
            },
        )?);

        Ok(Receipts::from(receipts))
    }

    pub fn revert(
        &self,
        txn: &mut WriteTransaction,
        transactions: &[Transaction],
        inherents: &[Inherent],
        block_height: u32,
        timestamp: u64,
        receipts: &Receipts,
    ) -> Result<(), AccountError> {
        let (
            sender_receipts,
            recipient_receipts,
            pre_tx_inherent_receipts,
            post_tx_inherent_receipts,
        ) = Self::prepare_receipts(receipts);

        self.process_inherents(
            txn,
            inherents.iter().filter(|i| !i.is_pre_transactions()),
            post_tx_inherent_receipts,
            |account, inherent, receipt| {
                account
                    .revert_inherent(inherent, block_height, timestamp, receipt)
                    .map(|_| None)
            },
        )?;

        self.revert_contracts(txn, transactions, block_height, timestamp)?;

        self.process_recipients(
            txn,
            transactions,
            block_height,
            timestamp,
            recipient_receipts,
            |account, transaction, block_height, receipt| {
                account
                    .revert_incoming_transaction(transaction, block_height, timestamp, receipt)
                    .map(|_| None)
            },
        )?;

        self.process_senders(
            txn,
            transactions,
            block_height,
            timestamp,
            sender_receipts,
            |account, transaction, block_height, receipt| {
                account
                    .revert_outgoing_transaction(transaction, block_height, timestamp, receipt)
                    .map(|_| None)
            },
        )?;

        self.process_inherents(
            txn,
            inherents.iter().filter(|i| i.is_pre_transactions()),
            pre_tx_inherent_receipts,
            |account, inherent, receipt| {
                account
                    .revert_inherent(inherent, block_height, timestamp, receipt)
                    .map(|_| None)
            },
        )?;

        Ok(())
    }

    fn process_senders<F>(
        &self,
        txn: &mut WriteTransaction,
        transactions: &[Transaction],
        block_height: u32,
        timestamp: u64,
        mut receipts: HashMap<u16, &Vec<u8>>,
        account_op: F,
    ) -> Result<Vec<Receipt>, AccountError>
    where
        F: Fn(
            &mut Account,
            &Transaction,
            u32,
            Option<&Vec<u8>>,
        ) -> Result<Option<Vec<u8>>, AccountError>,
    {
        let mut new_receipts = Vec::new();
        for (index, transaction) in transactions.iter().enumerate() {
            if let Some(data) = self.process_transaction(
                txn,
                &transaction.sender,
                Some(transaction.sender_type),
                transaction,
                block_height,
                timestamp,
                receipts.remove(&(index as u16)),
                &account_op,
            )? {
                new_receipts.push(Receipt::Transaction {
                    index: index as u16,
                    sender: true,
                    data,
                });
            }
        }
        Ok(new_receipts)
    }

    fn process_recipients<F>(
        &self,
        txn: &mut WriteTransaction,
        transactions: &[Transaction],
        block_height: u32,
        timestamp: u64,
        mut receipts: HashMap<u16, &Vec<u8>>,
        account_op: F,
    ) -> Result<Vec<Receipt>, AccountError>
    where
        F: Fn(
            &mut Account,
            &Transaction,
            u32,
            Option<&Vec<u8>>,
        ) -> Result<Option<Vec<u8>>, AccountError>,
    {
        let mut new_receipts = Vec::new();
        for (index, transaction) in transactions.iter().enumerate() {
            // FIXME This doesn't check that account_type == transaction.recipient_type when reverting
            let recipient_type = if transaction
                .flags
                .contains(TransactionFlags::CONTRACT_CREATION)
            {
                None
            } else {
                Some(transaction.recipient_type)
            };

            if let Some(data) = self.process_transaction(
                txn,
                &transaction.recipient,
                recipient_type,
                transaction,
                block_height,
                timestamp,
                receipts.remove(&(index as u16)),
                &account_op,
            )? {
                new_receipts.push(Receipt::Transaction {
                    index: index as u16,
                    sender: false,
                    data,
                });
            }
        }
        Ok(new_receipts)
    }

    fn process_transaction<F>(
        &self,
        txn: &mut WriteTransaction,
        address: &Address,
        account_type: Option<AccountType>,
        transaction: &Transaction,
        block_height: u32,
        _timestamp: u64,
        receipt: Option<&Vec<u8>>,
        account_op: &F,
    ) -> Result<Option<Vec<u8>>, AccountError>
    where
        F: Fn(
            &mut Account,
            &Transaction,
            u32,
            Option<&Vec<u8>>,
        ) -> Result<Option<Vec<u8>>, AccountError>,
    {
        // TODO Eliminate copy
        let mut account = self.get(address, Some(txn));

        // Check account type.
        if let Some(account_type) = account_type {
            if account.account_type() != account_type {
                return Err(AccountError::TypeMismatch {
                    expected: account.account_type(),
                    got: account_type,
                });
            }
        }

        // Apply transaction.
        let receipt = account_op(&mut account, transaction, block_height, receipt)?;

        // TODO Eliminate copy
        self.tree.put_batch(txn, address, account);

        Ok(receipt)
    }

    fn create_contracts(
        &self,
        txn: &mut WriteTransaction,
        transactions: &[Transaction],
        block_height: u32,
        timestamp: u64,
    ) -> Result<(), AccountError> {
        for transaction in transactions {
            if transaction
                .flags
                .contains(TransactionFlags::CONTRACT_CREATION)
            {
                self.create_contract(txn, transaction, block_height, timestamp)?;
            }
        }
        Ok(())
    }

    fn create_contract(
        &self,
        txn: &mut WriteTransaction,
        transaction: &Transaction,
        block_height: u32,
        timestamp: u64,
    ) -> Result<(), AccountError> {
        assert!(transaction
            .flags
            .contains(TransactionFlags::CONTRACT_CREATION));

        let recipient_account = self.get(&transaction.recipient, Some(txn));
        let new_recipient_account = Account::new_contract(
            transaction.recipient_type,
            recipient_account.balance(),
            transaction,
            block_height,
            timestamp,
        )?;
        self.tree
            .put_batch(txn, &transaction.recipient, new_recipient_account);
        Ok(())
    }

    fn revert_contracts(
        &self,
        txn: &mut WriteTransaction,
        transactions: &[Transaction],
        block_height: u32,
        timestamp: u64,
    ) -> Result<(), AccountError> {
        for transaction in transactions {
            if transaction
                .flags
                .contains(TransactionFlags::CONTRACT_CREATION)
            {
                self.revert_contract(txn, transaction, block_height, timestamp)?;
            }
        }
        Ok(())
    }

    fn revert_contract(
        &self,
        txn: &mut WriteTransaction,
        transaction: &Transaction,
        _block_height: u32,
        _timestamp: u64,
    ) -> Result<(), AccountError> {
        assert!(transaction
            .flags
            .contains(TransactionFlags::CONTRACT_CREATION));

        let recipient_account = self.get(&transaction.recipient, Some(txn));
        if recipient_account.account_type() != transaction.recipient_type {
            return Err(AccountError::TypeMismatch {
                expected: recipient_account.account_type(),
                got: transaction.recipient_type,
            });
        }

        let new_recipient_account = Account::new_basic(recipient_account.balance());
        self.tree
            .put_batch(txn, &transaction.recipient, new_recipient_account);
        Ok(())
    }

    fn process_inherents<'a, F, I>(
        &self,
        txn: &mut WriteTransaction,
        inherents: I,
        mut receipts: HashMap<u16, &Vec<u8>>,
        account_op: F,
    ) -> Result<Vec<Receipt>, AccountError>
    where
        F: Fn(&mut Account, &Inherent, Option<&Vec<u8>>) -> Result<Option<Vec<u8>>, AccountError>,
        I: Iterator<Item = &'a Inherent>,
    {
        let mut new_receipts = Vec::new();
        for (index, inherent) in inherents.enumerate() {
            if let Some(data) =
                self.process_inherent(txn, inherent, receipts.remove(&(index as u16)), &account_op)?
            {
                new_receipts.push(Receipt::Inherent {
                    pre_transactions: inherent.is_pre_transactions(),
                    index: index as u16,
                    data,
                });
            }
        }
        Ok(new_receipts)
    }

    fn process_inherent<F>(
        &self,
        txn: &mut WriteTransaction,
        inherent: &Inherent,
        receipt: Option<&Vec<u8>>,
        account_op: &F,
    ) -> Result<Option<Vec<u8>>, AccountError>
    where
        F: Fn(&mut Account, &Inherent, Option<&Vec<u8>>) -> Result<Option<Vec<u8>>, AccountError>,
    {
        // TODO Eliminate copy
        let mut account = self.get(&inherent.target, Some(txn));

        // Apply inherent.
        let receipt = account_op(&mut account, inherent, receipt)?;

        // TODO Eliminate copy
        self.tree.put_batch(txn, &inherent.target, account);

        Ok(receipt)
    }

    fn prepare_receipts(
        receipts: &Receipts,
    ) -> (ReceiptsMap, ReceiptsMap, ReceiptsMap, ReceiptsMap) {
        let mut sender_receipts = HashMap::new();
        let mut recipient_receipts = HashMap::new();
        let mut pre_tx_inherent_receipts = HashMap::new();
        let mut post_tx_inherent_receipts = HashMap::new();

        for receipt in &receipts.receipts {
            match receipt {
                Receipt::Transaction {
                    index,
                    sender,
                    data,
                } => {
                    if *sender {
                        sender_receipts.insert(*index, data);
                    } else {
                        recipient_receipts.insert(*index, data);
                    }
                }
                Receipt::Inherent {
                    index,
                    data,
                    pre_transactions,
                } => {
                    if *pre_transactions {
                        pre_tx_inherent_receipts.insert(*index, data);
                    } else {
                        post_tx_inherent_receipts.insert(*index, data);
                    }
                }
            }
        }
        (
            sender_receipts,
            recipient_receipts,
            pre_tx_inherent_receipts,
            post_tx_inherent_receipts,
        )
    }
}