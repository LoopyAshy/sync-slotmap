# sync-slotmap
I made this crate for a very specific use case I had where I needed to have a slotmap with secondarymaps but I required them to be sync and safe.
The issue with that however is that it would mean only 1 value could be accessed at a time which was problematic as that reduced the use heavily.
So I made a slotmap and secondarymap wrapper which adds RwLock logic to the collection itself and each value inside the collection.

These collections can be thought of as very similiar to `parking_lot::RwLock<SlotMap<_, parking_lot::RwLock<T>>` with more specialised functionality such as async un/locking and timeout un/locking.
Admittedly this project was very niche and likely not useful for most people but matched my needs at the time.
