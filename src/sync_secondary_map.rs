use std::ops::{Deref, DerefMut, Index};

use std::{cell::UnsafeCell, future::Future, task::Poll};

use parking_lot::{
    lock_api::{RawRwLock as RwLockTrait, RawRwLockTimed},
    RawRwLock,
};
use slotmap::*;

use crate::sync_slot_map::{
    SyncSlotGuard, SyncSlotGuardMut, UnlockRwLockExclusive, UnlockRwLockShared,
};

pub struct SyncSecondarySlotMap<K: Key, V> {
    inner: UnsafeCell<SecondaryMap<K, V>>,
    locks: *mut (RawRwLock, SecondaryMap<K, RawRwLock>),
}

unsafe impl<K: Key, V> Send for SyncSecondarySlotMap<K, V> where V: Send {}
unsafe impl<K: Key, V> Sync for SyncSecondarySlotMap<K, V> where V: Send + Sync {}

impl<K: Key, V> Drop for SyncSecondarySlotMap<K, V> {
    fn drop(&mut self) {
        if !unsafe { &*self.locks }
            .0
            .try_lock_exclusive_for(std::time::Duration::from_secs(1))
        {
            panic!("Dropped SyncSlotMap which was in use.");
        }
        unsafe { drop(Box::from_raw(self.locks)) };
    }
}

impl<K: Key, V> Default for SyncSecondarySlotMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Key, V> SyncSecondarySlotMap<K, V> {
    pub fn new() -> Self {
        let locks = Box::new((RawRwLock::INIT, SecondaryMap::new()));
        let locks = Box::into_raw(locks);
        Self {
            inner: UnsafeCell::new(SecondaryMap::new()),
            locks,
        }
    }

    pub fn insert(&self, key: K, value: V) -> Option<V> {
        unsafe { &*self.locks }.0.lock_exclusive();
        let output = unsafe { &mut *self.inner.get() }.insert(key, value);
        unsafe { &mut *self.locks }.1.insert(key, RawRwLock::INIT);
        unsafe {
            (*self.locks).0.unlock_exclusive();
        }
        output
    }

    pub fn try_insert_for(
        &self,
        key: K,
        value: V,
        timeout: std::time::Duration,
    ) -> Option<Option<V>> {
        if !unsafe { &*self.locks }.0.try_lock_exclusive_for(timeout) {
            return None;
        }
        let output = unsafe { &mut *self.inner.get() }.insert(key, value);
        unsafe { &mut *self.locks }.1.insert(key, RawRwLock::INIT);
        unsafe {
            (*self.locks).0.unlock_exclusive();
        }
        Some(output)
    }

    pub async fn insert_async(&self, key: K, value: V) -> Option<V> {
        UnlockRwLockExclusive {
            lock: &unsafe { &*self.locks }.0 as *const RawRwLock as *mut RawRwLock,
        }
        .await;
        let output = unsafe { &mut *self.inner.get() }.insert(key, value);
        unsafe { &mut *self.locks }.1.insert(key, RawRwLock::INIT);
        unsafe {
            (*self.locks).0.unlock_exclusive();
        }
        output
    }

    pub fn remove(&self, key: K) -> Option<V> {
        unsafe { &*self.locks }.0.lock_exclusive();
        let result = unsafe { &mut *self.inner.get() }.remove(key);
        unsafe { &mut *self.locks }.1.remove(key);
        unsafe {
            (*self.locks).0.unlock_exclusive();
        }
        result
    }

    pub fn try_remove_for(&self, key: K, timeout: std::time::Duration) -> Option<Option<V>> {
        if !unsafe { &*self.locks }.0.try_lock_exclusive_for(timeout) {
            return None;
        }
        let result = unsafe { &mut *self.inner.get() }.remove(key);
        unsafe { &mut *self.locks }.1.remove(key);
        unsafe {
            (*self.locks).0.unlock_exclusive();
        }
        Some(result)
    }

    pub async fn remove_async(&self, key: K) -> Option<V> {
        UnlockRwLockExclusive {
            lock: &unsafe { &*self.locks }.0 as *const RawRwLock as *mut RawRwLock,
        }
        .await;
        let result = unsafe { &mut *self.inner.get() }.remove(key);
        unsafe { &mut *self.locks }.1.remove(key);
        unsafe {
            (*self.locks).0.unlock_exclusive();
        }
        result
    }

    pub fn get(&self, key: K) -> Option<SyncSlotGuard<V>> {
        let (lock, value) = unsafe {
            (*self.locks).0.lock_shared();
            let Some(lock) = (*self.locks).1.get(key) else { (*self.locks).0.unlock_shared(); return None; };
            lock.lock_shared();
            let Some(value) = (*self.inner.get()).get(key) else {
                (*self.locks).0.unlock_shared();
                lock.unlock_shared();
                return None;
            };
            (lock, value)
        };
        let locks = (
            &unsafe { &*self.locks }.0 as *const RawRwLock as *mut RawRwLock,
            lock as *const RawRwLock as *mut RawRwLock,
        );
        let value = value as *const V as *mut V;
        Some(SyncSlotGuard { value, locks })
    }

    pub fn get_mut(&self, key: K) -> Option<SyncSlotGuardMut<V>> {
        let (lock, value) = unsafe {
            (*self.locks).0.lock_shared();
            let Some(lock) = (*self.locks).1.get(key) else {(*self.locks).0.unlock_shared(); return None; };
            lock.lock_exclusive();
            let Some(value) = (*self.inner.get()).get(key) else {
                (*self.locks).0.unlock_shared();
                lock.unlock_exclusive();
                return None;
            };
            (lock, value)
        };
        let locks = (
            &unsafe { &*self.locks }.0 as *const RawRwLock as *mut RawRwLock,
            lock as *const RawRwLock as *mut RawRwLock,
        );
        let value = value as *const V as *mut V;
        Some(SyncSlotGuardMut { value, locks })
    }

    pub fn try_get_for(
        &self,
        key: K,
        timeout: std::time::Duration,
    ) -> Option<Option<SyncSlotGuard<V>>> {
        let (lock, value) = unsafe {
            if !(*self.locks).0.try_lock_shared_for(timeout) {
                return None;
            }
            let Some(lock) = (*self.locks).1.get(key) else { (*self.locks).0.unlock_shared(); return Some(None); };
            if !lock.try_lock_shared_for(timeout) {
                (*self.locks).0.unlock_shared();
                return None;
            }
            let Some(value) = (*self.inner.get()).get(key) else {
                (*self.locks).0.unlock_shared();
                lock.unlock_shared();
                return Some(None);
            };
            (lock, value)
        };
        let locks = (
            &unsafe { &*self.locks }.0 as *const RawRwLock as *mut RawRwLock,
            lock as *const RawRwLock as *mut RawRwLock,
        );
        let value = value as *const V as *mut V;
        Some(Some(SyncSlotGuard { value, locks }))
    }

    pub fn try_get_mut_for(
        &self,
        key: K,
        timeout: std::time::Duration,
    ) -> Option<Option<SyncSlotGuardMut<V>>> {
        let (lock, value) = unsafe {
            if !(*self.locks).0.try_lock_shared_for(timeout) {
                return None;
            }
            let Some(lock) = (*self.locks).1.get(key) else { (*self.locks).0.unlock_shared(); return Some(None); };
            if !lock.try_lock_exclusive_for(timeout) {
                (*self.locks).0.unlock_shared();
                return None;
            }
            let Some(value) = (*self.inner.get()).get(key) else {
                (*self.locks).0.unlock_shared();
                lock.unlock_exclusive();
                return Some(None);
            };
            (lock, value)
        };
        let locks = (
            &unsafe { &*self.locks }.0 as *const RawRwLock as *mut RawRwLock,
            lock as *const RawRwLock as *mut RawRwLock,
        );
        let value = value as *const V as *mut V;
        Some(Some(SyncSlotGuardMut { value, locks }))
    }

    pub async fn get_async(&self, key: K) -> Option<SyncSlotGuard<V>> {
        let (lock, value) = unsafe {
            UnlockRwLockShared {
                lock: &(*self.locks).0 as *const RawRwLock as *mut RawRwLock,
            }
            .await;
            let Some(lock) = (*self.locks).1.get(key) else { (*self.locks).0.unlock_shared(); return None; };
            UnlockRwLockShared {
                lock: lock as *const RawRwLock as *mut RawRwLock,
            }
            .await;
            let Some(value) = (*self.inner.get()).get(key) else {
                (*self.locks).0.unlock_shared();
                lock.unlock_shared();
                return None;
            };
            (lock, value)
        };
        let locks = (
            &unsafe { &*self.locks }.0 as *const RawRwLock as *mut RawRwLock,
            lock as *const RawRwLock as *mut RawRwLock,
        );
        let value = value as *const V as *mut V;
        Some(SyncSlotGuard { value, locks })
    }
    pub async fn get_mut_async(&self, key: K) -> Option<SyncSlotGuardMut<V>> {
        let (lock, value) = unsafe {
            UnlockRwLockShared {
                lock: &(*self.locks).0 as *const RawRwLock as *mut RawRwLock,
            }
            .await;
            let Some(lock) = (*self.locks).1.get(key) else { (*self.locks).0.unlock_shared(); return None; };
            UnlockRwLockExclusive {
                lock: lock as *const RawRwLock as *mut RawRwLock,
            }
            .await;
            let Some(value) = (*self.inner.get()).get(key) else {
                (*self.locks).0.unlock_shared();
                lock.unlock_exclusive();
                return None;
            };
            (lock, value)
        };
        let locks = (
            &unsafe { &*self.locks }.0 as *const RawRwLock as *mut RawRwLock,
            lock as *const RawRwLock as *mut RawRwLock,
        );
        let value = value as *const V as *mut V;
        Some(SyncSlotGuardMut { value, locks })
    }
}
