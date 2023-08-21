use std::{
    any::{Any, TypeId},
    collections::hash_map::{
        Entry as StdEntry, HashMap, OccupiedEntry as StdOccupiedEntry,
        VacantEntry as StdVacantEntry,
    },
    marker::PhantomData,
};

pub trait TypeMapKey: Any {
    type Value: Any + Send + Sync;
}

pub struct TypeMap(HashMap<TypeId, Box<dyn Any + Send + Sync>>);

// I know these methods are unused, but they're here for when I do decide to use them,
// since this is just a personal program.
#[allow(unused)]
impl TypeMap {
    pub fn new() -> TypeMap {
        Self(HashMap::new())
    }

    pub fn contains_key<K: TypeMapKey>(&self) -> bool {
        self.0.contains_key(&TypeId::of::<K>())
    }

    pub fn insert<K: TypeMapKey>(&mut self, value: K::Value) -> Option<K::Value> {
        self.0
            .insert(TypeId::of::<K>(), Box::new(value))
            .map(|old| *old.downcast().unwrap())
    }

    pub fn entry<K: TypeMapKey>(&mut self) -> Entry<'_, K> {
        match self.0.entry(TypeId::of::<K>()) {
            StdEntry::Occupied(entry) => Entry::Occupied(OccupiedEntry {
                entry,
                _spooky: PhantomData,
            }),
            StdEntry::Vacant(entry) => Entry::Vacant(VacantEntry {
                entry,
                _spooky: PhantomData,
            }),
        }
    }

    pub fn get<K: TypeMapKey>(&self) -> Option<&K::Value> {
        self.0
            .get(&TypeId::of::<K>())
            .map(|val| val.downcast_ref().unwrap())
    }

    pub fn get_mut<K: TypeMapKey>(&mut self) -> Option<&mut K::Value> {
        self.0
            .get_mut(&TypeId::of::<K>())
            .map(|val| val.downcast_mut().unwrap())
    }

    pub fn remove<K: TypeMapKey>(&mut self) -> Option<K::Value> {
        self.0
            .remove(&TypeId::of::<K>())
            .map(|val| *val.downcast().unwrap())
    }
}

pub enum Entry<'a, K: TypeMapKey> {
    Occupied(OccupiedEntry<'a, K>),
    Vacant(VacantEntry<'a, K>),
}

#[allow(unused)]
impl<'a, K: TypeMapKey> Entry<'a, K> {
    pub fn or_insert(self, value: K::Value) -> &'a mut K::Value {
        match self {
            Self::Occupied(entry) => entry.into_mut(),
            Self::Vacant(entry) => entry.insert(value),
        }
    }

    pub fn or_insert_with<F>(self, f: F) -> &'a mut K::Value
    where
        F: FnOnce() -> K::Value,
    {
        match self {
            Self::Occupied(entry) => entry.into_mut(),
            Self::Vacant(entry) => entry.insert(f()),
        }
    }

    pub fn and_modify<F>(self, f: F) -> Self
    where
        F: FnOnce(&mut K::Value),
    {
        match self {
            Self::Occupied(mut entry) => {
                f(entry.get_mut());
                Entry::Occupied(entry)
            }
            Self::Vacant(entry) => Self::Vacant(entry),
        }
    }
}

#[allow(unused)]
impl<'a, K> Entry<'a, K>
where
    K: TypeMapKey,
    K::Value: Default,
{
    pub fn or_default(self) -> &'a mut K::Value {
        match self {
            Self::Occupied(entry) => entry.into_mut(),
            Self::Vacant(entry) => entry.insert(Default::default()),
        }
    }
}

pub struct OccupiedEntry<'a, K: TypeMapKey> {
    entry: StdOccupiedEntry<'a, TypeId, Box<dyn Any + Send + Sync>>,
    _spooky: PhantomData<&'a mut K::Value>,
}

#[allow(unused)]
impl<'a, K: TypeMapKey> OccupiedEntry<'a, K> {
    pub fn get(&self) -> &K::Value {
        self.entry.get().downcast_ref().unwrap()
    }

    pub fn get_mut(&mut self) -> &mut K::Value {
        self.entry.get_mut().downcast_mut().unwrap()
    }

    pub fn into_mut(self) -> &'a mut K::Value {
        self.entry.into_mut().downcast_mut().unwrap()
    }

    pub fn insert(&mut self, value: K::Value) -> K::Value {
        *self.entry.insert(Box::new(value)).downcast().unwrap()
    }

    pub fn remove(self) {
        *self.entry.remove().downcast().unwrap()
    }
}

pub struct VacantEntry<'a, K: TypeMapKey> {
    entry: StdVacantEntry<'a, TypeId, Box<dyn Any + Send + Sync>>,
    _spooky: PhantomData<&'a mut K::Value>,
}

impl<'a, K: TypeMapKey> VacantEntry<'a, K> {
    pub fn insert(self, value: K::Value) -> &'a mut K::Value {
        self.entry.insert(Box::new(value)).downcast_mut().unwrap()
    }
}
