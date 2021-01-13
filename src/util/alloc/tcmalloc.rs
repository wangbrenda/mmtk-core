use libc::{size_t, c_void};

#[cfg(feature = "malloc_tcmalloc")]
#[link(name = "tcmalloc", kind = "static")]
extern "C" {
    pub fn TCMallocInternalCalloc(n: size_t, size: size_t) -> *mut c_void;
    pub fn TCMallocInternalFree(ptr: *mut c_void);
    pub fn TCMallocInternalMallocSize(ptr: *mut c_void) -> size_t;
}