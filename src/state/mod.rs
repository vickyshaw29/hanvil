//! The one chain state. Everything the three servers read or write lives here, behind one lock.

pub mod accounts;

use std::collections::{BTreeMap, HashMap};

use alloy_primitives::Address;

pub use accounts::{Account, EntityId, Key};

use crate::evm::units::Tinybar;
use crate::keys;

/// Parameters that shape the initial state.
#[derive(Debug, Clone)]
pub struct Genesis {
    /// EVM chain id reported by `eth_chainId`.
    pub chain_id: u64,
    /// Predefined accounts per key type (1..=10).
    pub accounts_per_type: u8,
    /// Balance given to every predefined account.
    pub balance: Tinybar,
}

/// Errors while building or mutating state.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A predefined key could not be parsed or derived.
    #[error(transparent)]
    Key(#[from] keys::Error),
}

/// First entity id handed out to user accounts. Matches hiero-local-node.
pub const FIRST_USER_ID: u64 = 1002;

/// In-memory chain. Cloning it is how snapshots work.
#[derive(Debug, Clone)]
pub struct Chain {
    chain_id: u64,
    accounts: BTreeMap<EntityId, Account>,
    by_evm: HashMap<Address, EntityId>,
    next_id: u64,
    block_number: u64,
}

impl Chain {
    /// Build genesis: system accounts and the predefined, funded user accounts.
    pub fn genesis(genesis: &Genesis) -> Result<Self, Error> {
        let mut chain = Self {
            chain_id: genesis.chain_id,
            accounts: BTreeMap::new(),
            by_evm: HashMap::new(),
            next_id: FIRST_USER_ID,
            block_number: 0,
        };
        for account in keys::predefined::accounts(genesis.accounts_per_type, genesis.balance)? {
            chain.insert_account(account);
        }
        Ok(chain)
    }

    fn insert_account(&mut self, account: Account) {
        self.by_evm.insert(account.evm_address(), account.id);
        if let Some(alias) = account.alias {
            self.by_evm.insert(alias, account.id);
        }
        self.next_id = self.next_id.max(account.id.0 + 1);
        self.accounts.insert(account.id, account);
    }

    /// EVM chain id.
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// Latest block number.
    pub fn block_number(&self) -> u64 {
        self.block_number
    }

    /// Account by either its alias or its long-zero address.
    pub fn account_by_evm(&self, address: &Address) -> Option<&Account> {
        self.by_evm
            .get(address)
            .and_then(|id| self.accounts.get(id))
    }

    /// Balance for an EVM address; zero when unknown, as the relay reports.
    pub fn balance_by_evm(&self, address: &Address) -> Tinybar {
        self.account_by_evm(address)
            .map(|a| a.balance)
            .unwrap_or_default()
    }

    /// Accounts grouped for the boot banner, in id order within each group.
    pub fn accounts_by_group(&self) -> Vec<(&'static str, Vec<&Account>)> {
        let mut ecdsa = Vec::new();
        let mut alias = Vec::new();
        let mut ed = Vec::new();
        for account in self.accounts.values() {
            match (&account.key, account.alias) {
                (Key::EcdsaSecp256k1(_), Some(_)) => alias.push(account),
                (Key::EcdsaSecp256k1(_), None) => ecdsa.push(account),
                (Key::Ed25519(_), _) => ed.push(account),
            }
        }
        vec![
            ("Accounts (ECDSA, long-zero address)", ecdsa),
            ("Accounts (ECDSA with EVM alias)", alias),
            ("Accounts (ED25519)", ed),
        ]
    }
}
