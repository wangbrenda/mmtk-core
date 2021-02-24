use crate::util::constants;
use crate::util::conversions;
use crate::util::side_metadata::load_atomic;
use crate::util::side_metadata::meta_bytes_per_chunk;
use crate::util::side_metadata::store_atomic;
use crate::util::side_metadata::try_mmap_metadata_chunk;
use crate::util::side_metadata::SideMetadataScope;
use crate::util::side_metadata::SideMetadataSpec;
use crate::util::Address;
use crate::util::ObjectReference;
use conversions::chunk_align_down;
use std::collections::HashSet;
use std::sync::RwLock;

lazy_static! {
    pub static ref ACTIVE_CHUNKS: RwLock<HashSet<Address>> = RwLock::default();
}

pub const ALLOC_METADATA_SPEC: SideMetadataSpec = SideMetadataSpec {
    scope: SideMetadataScope::Global,
    offset: 0,
    log_num_of_bits: 0,
    log_min_obj_size: constants::LOG_BYTES_IN_WORD as usize,
};

pub fn meta_space_mapped(address: Address) -> bool {
    let chunk_start = chunk_align_down(address);
    ACTIVE_CHUNKS.read().unwrap().contains(&chunk_start)
}

pub fn map_meta_space_for_chunk(chunk_start: Address) {
    let mut active_chunks = ACTIVE_CHUNKS.write().unwrap();
    if active_chunks.contains(&chunk_start) {
        return;
    }
    active_chunks.insert(chunk_start);
    try_mmap_metadata_chunk(
        chunk_start,
        meta_bytes_per_chunk(
            ALLOC_METADATA_SPEC.log_min_obj_size,
            ALLOC_METADATA_SPEC.log_num_of_bits,
        ),
        0,
    );
}

// Check if a given object was allocated
pub fn is_alloced(object: ObjectReference) -> bool {
    let address = object.to_address();
    meta_space_mapped(address) && load_atomic(ALLOC_METADATA_SPEC, address) == 1
}

// Find the object from the interior pointer
pub fn find_object(object: ObjectReference) -> ObjectReference {
    let mut address = object.to_address();
    loop {
        println!("is this an alloced pointer {:?}", address);
        assert!(meta_space_mapped(address));
        if load_atomic(ALLOC_METADATA_SPEC, address) == 1 {
            return unsafe { address.to_object_reference() }
        }
        println!("... no");
        address = address - constants::BYTES_IN_WORD as usize;
    }
}

pub fn set_alloc_bit(address: Address) {
    store_atomic(ALLOC_METADATA_SPEC, address, 1);
}

pub fn unset_alloc_bit(address: Address) {
    store_atomic(ALLOC_METADATA_SPEC, address, 0);
}