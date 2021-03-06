#![feature(fn_traits)]
#![feature(with_options)]
#![feature(map_first_last)]

use std::cmp::Ordering;
use std::marker::PhantomData;
use std::collections::VecDeque;
use std::sync::atomic::AtomicU64;

mod encode;
mod error;
mod table;
mod partition;
mod io;

pub use table::tablefmt;

pub trait Comparator {
    fn compare(lhs: &[u8], rhs: &[u8]) -> Ordering;
}

pub struct DefaultComparator();

impl Comparator for DefaultComparator {
    fn compare(lhs: &[u8], rhs: &[u8]) -> Ordering {
        lhs.cmp(rhs)
    }
}

pub struct Options {
    pub db_name: String,
    pub cache_count: usize,
    pub level0_size: usize,
    pub size_factor: usize,
    pub max_open_files: usize,
    pub table_size: usize,
    pub key_size_max: usize,
    pub value_size_max: usize,
}

impl Options {
    pub fn new(db_name: impl ToString,
               cache_count: usize,
               level0_size: usize,
               size_factor: usize,
               max_open_files: usize,
               table_size: usize,
               key_size_max: usize,
               value_size_max: usize) -> Self {
        Self {
            db_name: db_name.to_string(),
            cache_count,
            level0_size,
            size_factor,
            max_open_files,
            table_size,
            key_size_max,
            value_size_max,
        }
    }

    fn level_size(&self, level: usize) -> usize {
        self.level0_size * self.size_factor.pow(level as u32)
    }
}

use crate::io::IOManager;
use crate::table::cache::TableCacheManager;
use crate::partition::ArcPartition;

pub struct ScottDB<'a, Comp: 'static + Comparator> {
    phantom: PhantomData<Comp>,

    options: Options,
    seq: AtomicU64,
    partitions: VecDeque<ArcPartition<'a, Comp>>,
    cache_manager: TableCacheManager,
    io_manager: IOManager,
}

impl<'a, Comp: 'static + Comparator> ScottDB<'a, Comp> {
    pub fn new(options: Options) -> Self {
        let cache_count = options.cache_count;
        let max_open_files = options.max_open_files;
        Self {
            phantom: PhantomData,
            options,
            seq: AtomicU64::new(0),
            partitions: VecDeque::new(),
            cache_manager: TableCacheManager::new(cache_count),
            io_manager: IOManager::new(max_open_files),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
