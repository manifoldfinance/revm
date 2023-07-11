use super::{
    plain_account::PlainStorage, AccountStatus, PlainAccount, StorageWithOriginalValues,
    TransitionAccount,
};
use revm_interpreter::primitives::{AccountInfo, StorageSlot, KECCAK_EMPTY, U256};
use revm_precompile::HashMap;

/// Cache account is to store account from database be able
/// to be updated from output of revm and while doing that
/// create TransitionAccount needed for BundleState.
#[derive(Clone, Debug)]
pub struct CacheAccount {
    pub account: Option<PlainAccount>,
    pub status: AccountStatus,
}

impl CacheAccount {
    /// Create new account that is loaded from database.
    pub fn new_loaded(info: AccountInfo, storage: PlainStorage) -> Self {
        Self {
            account: Some(PlainAccount { info, storage }),
            status: AccountStatus::Loaded,
        }
    }

    /// Create new account that is loaded empty from database.
    pub fn new_loaded_empty_eip161(storage: PlainStorage) -> Self {
        Self {
            account: Some(PlainAccount::new_empty_with_storage(storage)),
            status: AccountStatus::LoadedEmptyEIP161,
        }
    }

    /// Loaded not existing account.
    pub fn new_loaded_not_existing() -> Self {
        Self {
            account: None,
            status: AccountStatus::LoadedNotExisting,
        }
    }

    /// Create new account that is newly created (State is AccountStatus::New)
    pub fn new_newly_created(info: AccountInfo, storage: PlainStorage) -> Self {
        Self {
            account: Some(PlainAccount { info, storage }),
            status: AccountStatus::InMemoryChange,
        }
    }

    /// Create account that is destroyed.
    pub fn new_destroyed() -> Self {
        Self {
            account: None,
            status: AccountStatus::Destroyed,
        }
    }

    /// Create changed account
    pub fn new_changed(info: AccountInfo, storage: PlainStorage) -> Self {
        Self {
            account: Some(PlainAccount { info, storage }),
            status: AccountStatus::Changed,
        }
    }

    /// Return true if account is some
    pub fn is_some(&self) -> bool {
        matches!(
            self.status,
            AccountStatus::Changed
                | AccountStatus::InMemoryChange
                | AccountStatus::DestroyedChanged
                | AccountStatus::Loaded
                | AccountStatus::LoadedEmptyEIP161
        )
    }

    /// Return storage slot if it exist.
    pub fn storage_slot(&self, slot: U256) -> Option<U256> {
        self.account
            .as_ref()
            .and_then(|a| a.storage.get(&slot).cloned())
    }

    /// Fetch account info if it exist.
    pub fn account_info(&self) -> Option<AccountInfo> {
        self.account.as_ref().map(|a| a.info.clone())
    }

    /// Desolve account into components.
    pub fn into_components(self) -> (Option<(AccountInfo, PlainStorage)>, AccountStatus) {
        (self.account.map(|a| a.into_components()), self.status)
    }

    /// Touche empty account, related to EIP-161 state clear.
    ///
    /// This account returns Transition that is used to create the BundleState.
    pub fn touch_empty(&mut self) -> Option<TransitionAccount> {
        let previous_status = self.status;

        // zero all storage slot as they are removed now.
        // This is effecting only for pre state clear accounts, as some of
        // then can be empty but contain storage slots.

        let storage = self
            .account
            .as_mut()
            .map(|acc| {
                acc.storage
                    .drain()
                    .map(|(k, v)| (k, StorageSlot::new_cleared_value(v)))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();

        // Set account to None.
        let previous_info = self.account.take().map(|acc| acc.info);

        // Set account state to Destroyed as we need to clear the storage if it exist.
        let old_status = self.status;
        self.status = match self.status {
            // mark account as destroyed again.
            AccountStatus::DestroyedChanged => AccountStatus::DestroyedAgain,
            AccountStatus::InMemoryChange => {
                // account can be created empty them touched.
                // Note: we can probably set it to LoadedNotExisting.
                AccountStatus::Destroyed
            }
            AccountStatus::LoadedNotExisting => {
                // account can be touched but not existing.
                // This is a noop.
                AccountStatus::LoadedNotExisting
            }
            AccountStatus::Destroyed => {
                // do nothing
                AccountStatus::Destroyed
            }
            AccountStatus::DestroyedAgain => {
                // do nothing
                AccountStatus::DestroyedAgain
            }
            // We need to clear the storage if there is any.
            AccountStatus::LoadedEmptyEIP161 => AccountStatus::Destroyed,
            _ => {
                // do nothing
                unreachable!("Wrong state transition, touch empty is not possible from {self:?}");
            }
        };
        if matches!(
            old_status,
            AccountStatus::LoadedNotExisting
                | AccountStatus::Destroyed
                | AccountStatus::DestroyedAgain
        ) {
            None
        } else {
            Some(TransitionAccount {
                info: None,
                status: self.status,
                previous_info,
                previous_status,
                storage,
            })
        }
    }

    /// Consume self and make account as destroyed.
    ///
    /// Set account as None and set status to Destroyer or DestroyedAgain.
    pub fn selfdestruct(&mut self) -> Option<TransitionAccount> {
        // account should be None after selfdestruct so we can take it.
        let previous_info = self.account.take().map(|a| a.info);
        let previous_status = self.status;

        self.status = match self.status {
            AccountStatus::DestroyedChanged
            | AccountStatus::DestroyedAgain
            | AccountStatus::Destroyed => {
                // mark as destroyed again, this can happen if account is created and
                // then selfdestructed in same block.
                // Note: there is no big difference between Destroyed and DestroyedAgain
                // in this case, but was added for clarity.
                AccountStatus::DestroyedAgain
            }

            _ => AccountStatus::Destroyed,
        };

        if previous_status == AccountStatus::LoadedNotExisting {
            // not transitions for account loaded as not existing.
            None
        } else {
            Some(TransitionAccount {
                info: None,
                status: self.status,
                previous_info,
                previous_status,
                storage: HashMap::new(),
            })
        }
    }

    /// Newly created account.
    pub fn newly_created(
        &mut self,
        new_info: AccountInfo,
        new_storage: StorageWithOriginalValues,
    ) -> TransitionAccount {
        let previous_status = self.status;
        let mut previous_info = self.account.take();

        // For newly create accounts. Old storage needs to be discarded (set to zero).
        let mut storage_diff = previous_info
            .as_mut()
            .map(|a| {
                core::mem::take(&mut a.storage)
                    .into_iter()
                    .map(|(k, v)| (k, StorageSlot::new_cleared_value(v)))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let new_bundle_storage = new_storage
            .iter()
            .map(|(k, s)| (*k, s.present_value))
            .collect();

        storage_diff.extend(new_storage);

        self.status = match self.status {
            // if account was destroyed previously just copy new info to it.
            AccountStatus::DestroyedAgain
            | AccountStatus::Destroyed
            | AccountStatus::DestroyedChanged => AccountStatus::DestroyedChanged,
            // if account is loaded from db.
            AccountStatus::LoadedNotExisting
            // Loaded empty eip161 to creates is not possible as CREATE2 was added after EIP-161
            | AccountStatus::LoadedEmptyEIP161
            | AccountStatus::Loaded
            | AccountStatus::Changed
            | AccountStatus::InMemoryChange => {
                // if account is loaded and not empty this means that account has some balance
                // this does not mean that accoun't can be created.
                // We are assuming that EVM did necessary checks before allowing account to be created.
                AccountStatus::InMemoryChange
            }
        };
        let transition_account = TransitionAccount {
            info: Some(new_info.clone()),
            status: self.status,
            previous_status,
            previous_info: previous_info.map(|a| a.info),
            storage: storage_diff,
        };
        self.account = Some(PlainAccount {
            info: new_info,
            storage: new_bundle_storage,
        });
        transition_account
    }

    /// Increment balance by `balance` amount. Assume that balance will not
    /// overflow or be zero.
    ///
    /// Note: to skip some edgecases we assume that additional balance is never zero.
    /// And as increment is always related to block fee/reward and withdrawals this is correct.
    pub fn increment_balance(&mut self, balance: u128) -> TransitionAccount {
        self.account_info_change(|info| {
            info.balance += U256::from(balance);
        })
        .1
    }

    fn account_info_change<T, F: FnOnce(&mut AccountInfo) -> T>(
        &mut self,
        change: F,
    ) -> (T, TransitionAccount) {
        let previous_status = self.status;
        let previous_info = self.account_info();
        let mut account = self.account.take().unwrap_or_default();
        let output = change(&mut account.info);
        self.account = Some(account);

        self.status = match self.status {
            AccountStatus::Loaded => {
                // Account that have nonce zero are the ones that
                if previous_info.as_ref().map(|a| a.code_hash) == Some(KECCAK_EMPTY) {
                    AccountStatus::InMemoryChange
                } else {
                    AccountStatus::Changed
                }
            }
            AccountStatus::LoadedNotExisting => AccountStatus::InMemoryChange,
            AccountStatus::LoadedEmptyEIP161 => AccountStatus::InMemoryChange,
            AccountStatus::Changed => AccountStatus::Changed,
            AccountStatus::InMemoryChange => AccountStatus::InMemoryChange,
            AccountStatus::Destroyed => AccountStatus::DestroyedChanged,
            AccountStatus::DestroyedChanged => AccountStatus::DestroyedChanged,
            AccountStatus::DestroyedAgain => AccountStatus::DestroyedChanged,
        };

        (
            output,
            TransitionAccount {
                info: self.account_info(),
                status: self.status,
                previous_info,
                previous_status,
                storage: HashMap::new(),
            },
        )
    }

    /// Drain balance from account and return transition and drained amount
    ///
    /// Used for DAO hardfork transition.
    pub fn drain_balance(&mut self) -> (u128, TransitionAccount) {
        self.account_info_change(|info| {
            let output = info.balance;
            info.balance = U256::ZERO;
            output.try_into().unwrap()
        })
    }

    pub fn change(
        &mut self,
        new: AccountInfo,
        storage: StorageWithOriginalValues,
    ) -> TransitionAccount {
        let previous_status = self.status;
        let previous_info = self.account.as_ref().map(|a| a.info.clone());
        let mut this_storage = self
            .account
            .take()
            .map(|acc| acc.storage)
            .unwrap_or_default();
        let mut this_storage = core::mem::take(&mut this_storage);

        this_storage.extend(storage.iter().map(|(k, s)| (*k, s.present_value)));
        let changed_account = PlainAccount {
            info: new,
            storage: this_storage,
        };

        self.status = match self.status {
            AccountStatus::Loaded => {
                if previous_info.as_ref().map(|a| a.code_hash) == Some(KECCAK_EMPTY) {
                    // account can still be created but some balance is added to it.
                    AccountStatus::InMemoryChange
                } else {
                    // can be contract and some of storage slots can be present inside db.
                    AccountStatus::Changed
                }
            }
            AccountStatus::Changed => {
                // Update to new changed state.
                AccountStatus::Changed
            }
            AccountStatus::InMemoryChange => {
                // promote to NewChanged.
                // Check if account is empty is done outside of this fn.
                AccountStatus::InMemoryChange
            }
            AccountStatus::DestroyedChanged => {
                // have same state
                AccountStatus::DestroyedChanged
            }
            AccountStatus::LoadedEmptyEIP161 => {
                // Change on empty account, should transfer storage if there is any.
                // There is posibility that there are storage inside db.
                // That storage is used in merkle tree calculation before state clear EIP.
                AccountStatus::InMemoryChange
            }
            AccountStatus::LoadedNotExisting => {
                // if it is loaded not existing and then changed
                // This means this is balance transfer that created the account.
                AccountStatus::InMemoryChange
            }
            AccountStatus::Destroyed | AccountStatus::DestroyedAgain => {
                // If account is destroyed and then changed this means this is
                // balance tranfer.
                AccountStatus::DestroyedChanged
            }
        };
        self.account = Some(changed_account);

        TransitionAccount {
            info: self.account.as_ref().map(|a| a.info.clone()),
            status: self.status,
            previous_info,
            previous_status,
            storage,
        }
    }
}
