use std::fmt::{Debug, Display};
use std::ops::{Deref, DerefMut, Index, IndexMut};

use std::{cell::UnsafeCell, future::Future, task::Poll};

use parking_lot::{
    lock_api::{RawRwLock as RwLockTrait, RawRwLockTimed},
    RawRwLock,
};
use slotmap::*;

pub struct SyncSlotMap<K: Key, V> {
    inner: UnsafeCell<SlotMap<K, V>>,
    locks: *mut (RawRwLock, SecondaryMap<K, RawRwLock>),
}

unsafe impl<K: Key, V> Send for SyncSlotMap<K, V> where V: Send {}
unsafe impl<K: Key, V> Sync for SyncSlotMap<K, V> where V: Send + Sync {}

impl<K: Key, V> Drop for SyncSlotMap<K, V> {
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

impl<K: Key, V> Default for SyncSlotMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Key, V> SyncSlotMap<K, V> {
    pub fn new() -> Self {
        let locks = Box::new((RawRwLock::INIT, SecondaryMap::new()));
        let locks = Box::into_raw(locks);
        Self {
            inner: UnsafeCell::new(SlotMap::with_key()),
            locks,
        }
    }

    pub fn insert(&self, value: V) -> K {
        unsafe { &*self.locks }.0.lock_exclusive();
        let key = unsafe { &mut *self.inner.get() }.insert(value);
        unsafe { &mut *self.locks }.1.insert(key, RawRwLock::INIT);
        unsafe {
            (*self.locks).0.unlock_exclusive();
        }
        key
    }

    pub fn try_insert_for(&self, value: V, timeout: std::time::Duration) -> Option<K> {
        if !unsafe { &*self.locks }.0.try_lock_exclusive_for(timeout) {
            return None;
        }
        let key = unsafe { &mut *self.inner.get() }.insert(value);
        unsafe { &mut *self.locks }.1.insert(key, RawRwLock::INIT);
        unsafe {
            (*self.locks).0.unlock_exclusive();
        }
        Some(key)
    }

    pub async fn insert_async(&self, value: V) -> K {
        UnlockRwLockExclusive {
            lock: &unsafe { &*self.locks }.0 as *const RawRwLock as *mut RawRwLock,
        }
        .await;
        let key = unsafe { &mut *self.inner.get() }.insert(value);
        unsafe { &mut *self.locks }.1.insert(key, RawRwLock::INIT);
        unsafe {
            (*self.locks).0.unlock_exclusive();
        }
        key
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
            let Some(lock) = (*self.locks).1.get(key) else { (*self.locks).0.unlock_shared(); return None; };
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

pub struct SyncSlotGuard<V> {
    pub(crate) value: *mut V,
    pub(crate) locks: (*mut RawRwLock, *mut RawRwLock),
}

impl<V: Debug> Debug for SyncSlotGuard<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncSlotGuard")
            .field("value", unsafe { &*self.value })
            .finish()
    }
}

impl<V> Drop for SyncSlotGuard<V> {
    fn drop(&mut self) {
        unsafe {
            (*self.locks.1).unlock_shared();
            (*self.locks.0).unlock_shared()
        }
    }
}

impl<V: Display> Display for SyncSlotGuard<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        unsafe { &(*self.value) }.fmt(f)
    }
}

impl<V> Clone for SyncSlotGuard<V> {
    fn clone(&self) -> Self {
        unsafe {
            (*self.locks.0).lock_shared();
            (*self.locks.1).lock_shared();
        }
        Self {
            value: self.value.clone(),
            locks: self.locks.clone(),
        }
    }
}

unsafe impl<V> Sync for SyncSlotGuard<V> {}
unsafe impl<V: Send> Send for SyncSlotGuard<V> {}

impl<V> SyncSlotGuard<V> {
    pub fn get(&self) -> &V {
        unsafe { &*self.value }
    }
}

impl<V> Deref for SyncSlotGuard<V> {
    type Target = V;

    fn deref(&self) -> &Self::Target {
        self.get()
    }
}

pub struct SyncSlotGuardMut<V> {
    pub(crate) value: *mut V,
    pub(crate) locks: (*mut RawRwLock, *mut RawRwLock),
}

unsafe impl<V: Sync> Sync for SyncSlotGuardMut<V> {}
unsafe impl<V: Send> Send for SyncSlotGuardMut<V> {}

impl<V: Debug> Debug for SyncSlotGuardMut<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncSlotGuardMut")
            .field("value", unsafe { &*self.value })
            .finish()
    }
}

impl<V: Display> Display for SyncSlotGuardMut<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        unsafe { &(*self.value) }.fmt(f)
    }
}

impl<V> Drop for SyncSlotGuardMut<V> {
    fn drop(&mut self) {
        unsafe {
            (*self.locks.1).unlock_exclusive();
            (*self.locks.0).unlock_shared()
        }
    }
}

impl<V> SyncSlotGuardMut<V> {
    pub fn get(&self) -> &V {
        unsafe { &*self.value }
    }

    pub fn get_mut(&mut self) -> &mut V {
        unsafe { &mut *self.value }
    }
}

impl<V> Deref for SyncSlotGuardMut<V> {
    type Target = V;

    fn deref(&self) -> &Self::Target {
        self.get()
    }
}

impl<V> DerefMut for SyncSlotGuardMut<V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.get_mut()
    }
}

pub(crate) struct UnlockRwLockShared {
    pub(crate) lock: *mut RawRwLock,
}

impl Future for UnlockRwLockShared {
    type Output = ();

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if unsafe { &*self.lock }.try_lock_shared() {
            Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

pub(crate) struct UnlockRwLockExclusive {
    pub(crate) lock: *mut RawRwLock,
}

impl Future for UnlockRwLockExclusive {
    type Output = ();

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if unsafe { &*self.lock }.try_lock_exclusive() {
            Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}
