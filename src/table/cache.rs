use std::sync::{Arc, Mutex};
use std::ptr::NonNull;
use std_semaphore::Semaphore;

use lru::LruCache;
use crc::crc32;

use crate::table::sctable::ScTableFile;

use crate::table::tablefmt::{TABLE_MIN_SIZE, TABLE_MAGIC_SIZE, TABLE_MAGIC, TABLE_CATALOG_ITEM_SIZE,
                             TABLE_HEAD_SIZE, TABLE_MAX_SIZE, TABLE_DELETION_BITMASK};
use crate::encode::{encode_fixed32_ret, decode_fixed32, decode_fixed64, encode_fixed64_ret};
use crate::error::Error;
use crate::Comparator;
use crate::partition::{InternalKey, UserKey};

pub(crate) struct ScTableCatalogItem {
    pub(crate) key_seq: u64,
    pub(crate) key_off: u32,
    pub(crate) key_len: u32,
    pub(crate) value_off: u32,
    pub(crate) value_len: u32
}

impl ScTableCatalogItem {
    pub(crate) fn new(key_seq: u64, key_off: u32, key_len: u32, value_off: u32, value_len: u32) -> Self {
        Self { key_seq, key_off, key_len, value_off, value_len }
    }

    pub(crate) fn serialize(&self, dest: &mut Vec<u8>) {
        dest.extend_from_slice(&encode_fixed64_ret(self.key_seq));
        dest.extend_from_slice(&encode_fixed32_ret(self.key_off));
        dest.extend_from_slice(&encode_fixed32_ret(self.key_len));
        dest.extend_from_slice(&encode_fixed32_ret(self.value_off));
        dest.extend_from_slice(&encode_fixed32_ret(self.value_len));
    }

    pub(crate) fn deserialize(from: &[u8]) -> Self {
        debug_assert_eq!(from.len(), TABLE_CATALOG_ITEM_SIZE);
        Self {
            key_seq: decode_fixed64(&from[0..8]),
            key_off: decode_fixed32(&from[8..12]),
            key_len: decode_fixed32(&from[12..16]),
            value_off: decode_fixed32(&from[16..20]),
            value_len: decode_fixed32(&from[20..24]),
        }
    }
}

pub(crate) struct ScTableCache {
    catalog: Vec<ScTableCatalogItem>,
    data: Vec<u8>,
    quota: CacheQuota
}

impl ScTableCache {
    pub(crate) fn from_raw(raw: &[u8], quota: CacheQuota) -> Result<ScTableCache, Error> {
        if raw.len() < TABLE_MIN_SIZE {
            return Err(Error::sc_table_corrupt("too small to be a table file".into()))
        } else if raw.len() > TABLE_MAX_SIZE {
            return Err(Error::sc_table_corrupt("too large to be a table file".into()))
        }

        if &raw[raw.len()-TABLE_MAGIC_SIZE .. raw.len()] != TABLE_MAGIC {
            return Err(Error::sc_table_corrupt("incorrect table magic".into()))
        }

        let kv_catalog_size = decode_fixed32(&raw[0..4]) as usize;
        let data_size = decode_fixed32(&raw[4..8]) as usize;

        if kv_catalog_size % TABLE_CATALOG_ITEM_SIZE != 0 {
            return Err(Error::sc_table_corrupt("catalog size should be multiplication of 16".into()))
        }

        if (kv_catalog_size + data_size + TABLE_MIN_SIZE) != raw.len() {
            return Err(Error::sc_table_corrupt("incorrect table size".into()))
        }

        let kv_catalog_crc = decode_fixed32(&raw[8..12]);
        let data_crc = decode_fixed32(&raw[12..16]);

        let kv_catalog = &raw[TABLE_HEAD_SIZE..TABLE_HEAD_SIZE+ kv_catalog_size];
        let data = &raw[TABLE_HEAD_SIZE+ kv_catalog_size..TABLE_HEAD_SIZE+ kv_catalog_size +data_size];

        if crc32::checksum_ieee(kv_catalog) != kv_catalog_crc {
            return Err(Error::sc_table_corrupt("incorrect kv_catalog crc".into()))
        }

        if crc32::checksum_ieee(data) != data_crc {
            return Err(Error::sc_table_corrupt("incorrect data crc".into()))
        }

        let mut catalog_item = Vec::new();
        for i in 0..kv_catalog_size / TABLE_CATALOG_ITEM_SIZE {
            let base = i * TABLE_CATALOG_ITEM_SIZE;
            let index =
                ScTableCatalogItem::deserialize(&kv_catalog[base..base + TABLE_CATALOG_ITEM_SIZE]);
            if index.value_off & TABLE_DELETION_BITMASK != 0 {
            } else if (index.key_off + index.key_len) as usize > data.len()
                      || (index.value_off + index.value_len) as usize > data.len() {
                return Err(Error::sc_table_corrupt("incorrect key/value catalog data".into()))
            }
            catalog_item.push(index)
        }

        Ok(Self { catalog: catalog_item, data: data.to_vec(), quota })
    }

    pub(crate) fn get<Comp: Comparator>(&self, key: &InternalKey<Comp>) -> Option<Vec<u8>> {
        if let Ok(idx) = self.catalog.binary_search_by(
            |catalog_item| {
                let seq = catalog_item.key_seq;
                let user_key = self.key(catalog_item);
                let lookup_key = InternalKey::new(seq, UserKey::new_borrow(user_key));
                // TODO this is buggy.
                key.cmp(&lookup_key)
            }) {
            if self.catalog[idx].value_off & TABLE_DELETION_BITMASK != 0 {
                None
            } else {
                Some(self.value(&self.catalog[idx]).to_vec())
            }
        } else {
            None
        }
    }

    pub(crate) fn catalog_size(&self) -> usize {
        self.catalog.len()
    }

    pub(crate) fn nth_item(&self, n: usize) -> (u64, &[u8], &[u8]) {
        assert!(n < self.catalog_size());
        let catalog_item = &self.catalog[n];
        (catalog_item.key_seq, self.key(catalog_item), self.value(catalog_item))
    }

    fn key(&self, catalog_item: &ScTableCatalogItem) -> &[u8] {
        &self.data[catalog_item.key_off as usize .. (catalog_item.key_off + catalog_item.key_len) as usize]
    }

    fn value(&self, catalog_item: &ScTableCatalogItem) -> &[u8] {
        &self.data[catalog_item.value_off as usize .. (catalog_item.value_off + catalog_item.value_len) as usize]
    }
}

pub(crate) struct CacheQuota {
    cache_manager: NonNull<TableCacheManager>
}

impl CacheQuota {
    fn new(cache_manager: &TableCacheManager) -> Self {
        Self { cache_manager: unsafe { NonNull::new_unchecked(cache_manager as *const TableCacheManager as _) } }
    }
}

impl Drop for CacheQuota {
    fn drop(&mut self) {
        unsafe { self.cache_manager.as_ref().on_cache_released() }
    }
}

pub(crate) struct TableCacheManager {
    lru: Mutex<LruCache<ScTableFile, Arc<ScTableCache>>>,
    sem: Semaphore
}

/// Warning: make sure all `CacheQuota`s are dropped before the `TableCacheManager` drops.
/// Maybe we should mark the TableCacheManager to be `unsafe`.
impl TableCacheManager {
    pub(crate) fn new(cache_count: usize) -> Self {
        TableCacheManager {
            lru: Mutex::new(LruCache::new(cache_count)),
            sem: Semaphore::new(cache_count as isize)
        }
    }

    pub(crate) fn acquire_quota(&self) -> CacheQuota {
        self.sem.acquire();
        CacheQuota::new(self)
    }

    pub(crate) fn add_cache(&self, table_file: ScTableFile, table_cache: ScTableCache) -> Arc<ScTableCache> {
        let ret = Arc::new(table_cache);
        self.lru.lock().unwrap().put(table_file, ret.clone());
        ret
    }

    pub(crate) fn get_cache(&self, table_file: ScTableFile) -> Option<Arc<ScTableCache>> {
        self.lru.lock().unwrap().get(&table_file).and_then(|arc| Some(arc.clone()))
    }

    fn on_cache_released(&self) {
        self.sem.release()
    }
}
