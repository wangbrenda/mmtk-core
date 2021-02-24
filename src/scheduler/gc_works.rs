use super::work_bucket::WorkBucketStage;
use super::*;
use crate::plan::global::GcStatus;
use crate::util::*;
use crate::vm::*;
use crate::*;
use std::marker::PhantomData;
use std::mem;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::Ordering;

pub struct ScheduleCollection;

unsafe impl Sync for ScheduleCollection {}

impl<VM: VMBinding> GCWork<VM> for ScheduleCollection {
    fn do_work(&mut self, worker: &mut GCWorker<VM>, mmtk: &'static MMTK<VM>) {
        mmtk.plan.schedule_collection(worker.scheduler());
    }
}

impl<VM: VMBinding> CoordinatorWork<MMTK<VM>> for ScheduleCollection {}

/// GC Preparation Work (include updating global states)
pub struct Prepare<P: Plan, W: CopyContext + WorkerLocal> {
    pub plan: &'static P,
    _p: PhantomData<W>,
}

unsafe impl<P: Plan, W: CopyContext + WorkerLocal> Sync for Prepare<P, W> {}

impl<P: Plan, W: CopyContext + WorkerLocal> Prepare<P, W> {
    pub fn new(plan: &'static P) -> Self {
        Self {
            plan,
            _p: PhantomData,
        }
    }
}

impl<P: Plan, W: CopyContext + WorkerLocal> GCWork<P::VM> for Prepare<P, W> {
    fn do_work(&mut self, worker: &mut GCWorker<P::VM>, mmtk: &'static MMTK<P::VM>) {
        trace!("Prepare Global");
        self.plan.prepare(worker.tls);
        for mutator in <P::VM as VMBinding>::VMActivePlan::mutators() {
            mmtk.scheduler.work_buckets[WorkBucketStage::Prepare]
                .add(PrepareMutator::<P::VM>::new(mutator));
        }
        for w in &mmtk.scheduler.worker_group().workers {
            w.local_works.add(PrepareCollector::<W>::new());
        }
    }
}

/// GC Preparation Work (include updating global states)
pub struct PrepareMutator<VM: VMBinding> {
    // The mutator reference has static lifetime.
    // It is safe because the actual lifetime of this work-packet will not exceed the lifetime of a GC.
    pub mutator: &'static mut Mutator<VM>,
}

unsafe impl<VM: VMBinding> Sync for PrepareMutator<VM> {}

impl<VM: VMBinding> PrepareMutator<VM> {
    pub fn new(mutator: &'static mut Mutator<VM>) -> Self {
        Self { mutator }
    }
}

impl<VM: VMBinding> GCWork<VM> for PrepareMutator<VM> {
    fn do_work(&mut self, worker: &mut GCWorker<VM>, _mmtk: &'static MMTK<VM>) {
        trace!("Prepare Mutator");
        self.mutator.prepare(worker.tls);
    }
}

#[derive(Default)]
pub struct PrepareCollector<W: CopyContext + WorkerLocal>(PhantomData<W>);

impl<W: CopyContext + WorkerLocal> PrepareCollector<W> {
    pub fn new() -> Self {
        PrepareCollector(PhantomData)
    }
}

impl<VM: VMBinding, W: CopyContext + WorkerLocal> GCWork<VM> for PrepareCollector<W> {
    fn do_work(&mut self, worker: &mut GCWorker<VM>, _mmtk: &'static MMTK<VM>) {
        trace!("Prepare Collector");
        unsafe { worker.local::<W>() }.prepare();
    }
}

pub struct Release<P: Plan, W: CopyContext + WorkerLocal> {
    pub plan: &'static P,
    _p: PhantomData<W>,
}

unsafe impl<P: Plan, W: CopyContext + WorkerLocal> Sync for Release<P, W> {}

impl<P: Plan, W: CopyContext + WorkerLocal> Release<P, W> {
    pub fn new(plan: &'static P) -> Self {
        Self {
            plan,
            _p: PhantomData,
        }
    }
}

impl<P: Plan, W: CopyContext + WorkerLocal> GCWork<P::VM> for Release<P, W> {
    fn do_work(&mut self, worker: &mut GCWorker<P::VM>, mmtk: &'static MMTK<P::VM>) {
        trace!("Release Global");
        self.plan.release(worker.tls);
        for mutator in <P::VM as VMBinding>::VMActivePlan::mutators() {
            mmtk.scheduler.work_buckets[WorkBucketStage::Release]
                .add(ReleaseMutator::<P::VM>::new(mutator));
        }
        for w in &mmtk.scheduler.worker_group().workers {
            w.local_works.add(ReleaseCollector::<W>(PhantomData));
        }
        // TODO: Process weak references properly
        mmtk.reference_processors.clear();
    }
}

pub struct ReleaseMutator<VM: VMBinding> {
    // The mutator reference has static lifetime.
    // It is safe because the actual lifetime of this work-packet will not exceed the lifetime of a GC.
    pub mutator: &'static mut Mutator<VM>,
}

unsafe impl<VM: VMBinding> Sync for ReleaseMutator<VM> {}

impl<VM: VMBinding> ReleaseMutator<VM> {
    pub fn new(mutator: &'static mut Mutator<VM>) -> Self {
        Self { mutator }
    }
}

impl<VM: VMBinding> GCWork<VM> for ReleaseMutator<VM> {
    fn do_work(&mut self, worker: &mut GCWorker<VM>, _mmtk: &'static MMTK<VM>) {
        trace!("Release Mutator");
        self.mutator.release(worker.tls);
    }
}

#[derive(Default)]
pub struct ReleaseCollector<W: CopyContext + WorkerLocal>(PhantomData<W>);

impl<W: CopyContext + WorkerLocal> ReleaseCollector<W> {
    pub fn new() -> Self {
        ReleaseCollector(PhantomData)
    }
}

impl<VM: VMBinding, W: CopyContext + WorkerLocal> GCWork<VM> for ReleaseCollector<W> {
    fn do_work(&mut self, worker: &mut GCWorker<VM>, _mmtk: &'static MMTK<VM>) {
        trace!("Release Collector");
        unsafe { worker.local::<W>() }.release();
    }
}

/// Stop all mutators
///
/// Schedule a `ScanStackRoots` immediately after a mutator is paused
///
/// TODO: Smaller work granularity
#[derive(Default)]
pub struct StopMutators<NormalEdges: ProcessEdgesWork, InteriorEdges: ProcessEdgesWork>(PhantomData<NormalEdges>, PhantomData<InteriorEdges>);

impl<NormalEdges: ProcessEdgesWork, InteriorEdges: ProcessEdgesWork> StopMutators<NormalEdges, InteriorEdges> {
    pub fn new() -> Self {
        Self(PhantomData, PhantomData)
    }
}

impl<NE: ProcessEdgesWork, IE: ProcessEdgesWork<VM = NE::VM>> GCWork<NE::VM> for StopMutators<NE, IE> {
    fn do_work(&mut self, worker: &mut GCWorker<NE::VM>, mmtk: &'static MMTK<NE::VM>) {
        if worker.is_coordinator() {
            trace!("stop_all_mutators start");
            debug_assert_eq!(mmtk.plan.base().scanned_stacks.load(Ordering::SeqCst), 0);
            <NE::VM as VMBinding>::VMCollection::stop_all_mutators::<NE>(worker.tls);
            trace!("stop_all_mutators end");
            mmtk.scheduler.notify_mutators_paused(mmtk);
            if <NE::VM as VMBinding>::VMScanning::SCAN_MUTATORS_IN_SAFEPOINT {
                // Prepare mutators if necessary
                // FIXME: This test is probably redundant. JikesRVM requires to call `prepare_mutator` once after mutators are paused
                if !mmtk.plan.common().stacks_prepared() {
                    for mutator in <NE::VM as VMBinding>::VMActivePlan::mutators() {
                        <NE::VM as VMBinding>::VMCollection::prepare_mutator(
                            mutator.get_tls(),
                            mutator,
                        );
                    }
                }
                // Scan mutators
                if <NE::VM as VMBinding>::VMScanning::SINGLE_THREAD_MUTATOR_SCANNING {
                    mmtk.scheduler.work_buckets[WorkBucketStage::Prepare]
                        .add(ScanStackRoots::<NE>::new());
                } else {
                    for mutator in <NE::VM as VMBinding>::VMActivePlan::mutators() {
                        mmtk.scheduler.work_buckets[WorkBucketStage::Prepare]
                            .add(ScanStackRoot::<NE>(mutator));
                    }
                }
            }
            mmtk.scheduler.work_buckets[WorkBucketStage::Prepare]
                .add(ScanVMSpecificRoots::<NE, IE>::new());
        } else {
            mmtk.scheduler
                .add_coordinator_work(StopMutators::<NE, IE>::new(), worker);
        }
    }
}

impl<NE: ProcessEdgesWork, IE: ProcessEdgesWork<VM = NE::VM>> CoordinatorWork<MMTK<NE::VM>> for StopMutators<NE, IE> {}

#[derive(Default)]
pub struct EndOfGC;

impl<VM: VMBinding> GCWork<VM> for EndOfGC {
    fn do_work(&mut self, worker: &mut GCWorker<VM>, mmtk: &'static MMTK<VM>) {
        mmtk.plan.common().base.set_gc_status(GcStatus::NotInGC);
        <VM as VMBinding>::VMCollection::resume_mutators(worker.tls);
    }
}

impl<VM: VMBinding> CoordinatorWork<MMTK<VM>> for EndOfGC {}

#[derive(Default)]
pub struct ScanStackRoots<Edges: ProcessEdgesWork>(PhantomData<Edges>);

impl<E: ProcessEdgesWork> ScanStackRoots<E> {
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

impl<E: ProcessEdgesWork> GCWork<E::VM> for ScanStackRoots<E> {
    fn do_work(&mut self, worker: &mut GCWorker<E::VM>, mmtk: &'static MMTK<E::VM>) {
        trace!("ScanStackRoots");
        <E::VM as VMBinding>::VMScanning::scan_thread_roots::<E>();
        <E::VM as VMBinding>::VMScanning::notify_initial_thread_scan_complete(false, worker.tls);
        mmtk.plan.common().base.set_gc_status(GcStatus::GcProper);
    }
}

pub struct ScanStackRoot<Edges: ProcessEdgesWork>(pub &'static mut Mutator<Edges::VM>);

impl<E: ProcessEdgesWork> GCWork<E::VM> for ScanStackRoot<E> {
    fn do_work(&mut self, worker: &mut GCWorker<E::VM>, mmtk: &'static MMTK<E::VM>) {
        let base = &mmtk.plan.base();
        let mutators = <E::VM as VMBinding>::VMActivePlan::number_of_mutators();
        trace!("ScanStackRoot for mutator {:?}", self.0.get_tls());
        <E::VM as VMBinding>::VMScanning::scan_thread_root::<E>(
            unsafe { &mut *(self.0 as *mut _) },
            worker.tls,
        );
        self.0.flush();
        let old = base.scanned_stacks.fetch_add(1, Ordering::SeqCst);
        trace!(
            "mutator {:?} old scanned_stacks = {}, new scanned_stacks = {}",
            self.0.get_tls(),
            old,
            base.scanned_stacks.load(Ordering::Relaxed)
        );

        if old + 1 >= mutators {
            loop {
                let current = base.scanned_stacks.load(Ordering::Relaxed);
                if current < mutators {
                    break;
                } else if base.scanned_stacks.compare_exchange(
                    current,
                    current - mutators,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) == Ok(current)
                {
                    trace!(
                        "mutator {:?} old scanned_stacks = {}, new scanned_stacks = {}, number_of_mutators = {}",
                        self.0.get_tls(),
                        current,
                        base.scanned_stacks.load(Ordering::Relaxed),
                        mutators
                    );
                    <E::VM as VMBinding>::VMScanning::notify_initial_thread_scan_complete(
                        false, worker.tls,
                    );
                    base.set_gc_status(GcStatus::GcProper);
                    break;
                }
            }
        }
    }
}

#[derive(Default)]
pub struct ScanVMSpecificRoots<NormalEdges: ProcessEdgesWork, InteriorEdges: ProcessEdgesWork<VM = NormalEdges::VM>>(PhantomData<NormalEdges>, PhantomData<InteriorEdges>);

impl<NE: ProcessEdgesWork, IE: ProcessEdgesWork<VM = NE::VM>> ScanVMSpecificRoots<NE, IE> {
    pub fn new() -> Self {
        Self(PhantomData, PhantomData)
    }
}

impl<NE: ProcessEdgesWork, IE: ProcessEdgesWork<VM = NE::VM>> GCWork<NE::VM> for ScanVMSpecificRoots<NE,IE> {
    fn do_work(&mut self, _worker: &mut GCWorker<NE::VM>, _mmtk: &'static MMTK<NE::VM>) {
        trace!("ScanStaticRoots");
        <NE::VM as VMBinding>::VMScanning::scan_vm_specific_roots::<NE, IE>();
    }
}

pub struct ProcessEdgesBase<E: ProcessEdgesWork> {
    pub edges: Vec<Address>,
    pub nodes: Vec<ObjectReference>,
    mmtk: &'static MMTK<E::VM>,
    // Use raw pointer for fast pointer dereferencing, instead of using `Option<&'static mut GCWorker<E::VM>>`.
    // Because a copying gc will dereference this pointer at least once for every object copy.
    worker: *mut GCWorker<E::VM>,
}

unsafe impl<E: ProcessEdgesWork> Sync for ProcessEdgesBase<E> {}
unsafe impl<E: ProcessEdgesWork> Send for ProcessEdgesBase<E> {}

impl<E: ProcessEdgesWork> ProcessEdgesBase<E> {
    // Requires an MMTk reference. Each plan-specific type that uses ProcessEdgesBase can get a static plan reference
    // at creation. This avoids overhead for dynamic dispatch or downcasting plan for each object traced.
    pub fn new(edges: Vec<Address>, mmtk: &'static MMTK<E::VM>) -> Self {
        Self {
            edges,
            nodes: vec![],
            mmtk,
            worker: std::ptr::null_mut(),
        }
    }
    pub fn set_worker(&mut self, worker: &mut GCWorker<E::VM>) {
        self.worker = worker;
    }
    #[inline]
    pub fn worker(&self) -> &'static mut GCWorker<E::VM> {
        unsafe { &mut *self.worker }
    }
    #[inline]
    pub fn mmtk(&self) -> &'static MMTK<E::VM> {
        self.mmtk
    }
    #[inline]
    pub fn plan(&self) -> &'static dyn Plan<VM = E::VM> {
        &*self.mmtk.plan
    }
}

pub trait ProcessEdges : Send + Sync + 'static + Sized + Default
{
    fn new() -> Self;
    fn preprocess_edge(&mut self, ob: ObjectReference) -> ObjectReference;
    fn postprocess_edge(&mut self, ob: ObjectReference) -> ObjectReference;
}

#[derive(Default)]
pub struct NormalEdges;

#[derive(Default)]
pub struct InteriorEdges { offset: usize }

impl ProcessEdges for NormalEdges {
    fn new() -> Self { Self{} }

    fn preprocess_edge(&mut self, ob: ObjectReference) -> ObjectReference {
        // do nothing
        ob
    }
    fn postprocess_edge(&mut self, ob: ObjectReference) -> ObjectReference {
        // do nothing
        ob
    }
}

impl ProcessEdges for InteriorEdges {
    fn new() -> Self {
        let offset: usize = 0;
        Self { offset }
    }
    fn preprocess_edge(&mut self, ob: ObjectReference) -> ObjectReference {
        let new_address = interior::find_object(ob).to_address() + 16 as usize; // adjust for header
        self.offset = ob.to_address().get_extent(new_address);
        println!("preprocess interior edge [{:?}] [{:?}]", ob, new_address);
        unsafe { new_address.to_object_reference() }
    }
    fn postprocess_edge(&mut self, ob: ObjectReference) -> ObjectReference {
        let new_object = unsafe { ob.to_address().add(self.offset).to_object_reference() };
        println!("postprocess interior edge off[{:?}] orig[{:?}] new[{:?}]", self.offset, ob, new_object);
        new_object
    }
}

/// Scan & update a list of object slots
pub trait ProcessEdgesWork:
    Send + Sync + 'static + Sized + DerefMut + Deref<Target = ProcessEdgesBase<Self>>
{
    type VM: VMBinding;
    type PE: ProcessEdges;
    const CAPACITY: usize = 4096;
    const OVERWRITE_REFERENCE: bool = true;
    const SCAN_OBJECTS_IMMEDIATELY: bool = true;
    fn new(edges: Vec<Address>, roots: bool, mmtk: &'static MMTK<Self::VM>) -> Self;
    fn trace_object(&mut self, object: ObjectReference) -> ObjectReference;

    #[inline]
    fn process_node(&mut self, object: ObjectReference) {
        if self.nodes.is_empty() {
            self.nodes.reserve(Self::CAPACITY);
        }
        self.nodes.push(object);
        // No need to flush this `nodes` local buffer to some global pool.
        // The max length of `nodes` buffer is equal to `CAPACITY` (when every edge produces a node)
        // So maximum 1 `ScanObjects` work can be created from `nodes` buffer
    }

    #[cold]
    fn flush(&mut self) {
        let mut new_nodes = vec![];
        mem::swap(&mut new_nodes, &mut self.nodes);
        let scan_objects_work = ScanObjects::<Self>::new(new_nodes, false);

        if Self::SCAN_OBJECTS_IMMEDIATELY {
            // We execute this `scan_objects_work` immediately.
            // This is expected to be a useful optimization because,
            // say for _pmd_ with 200M heap, we're likely to have 50000~60000 `ScanObjects` works
            // being dispatched (similar amount to `ProcessEdgesWork`).
            // Executing these works now can remarkably reduce the global synchronization time.
            self.worker().do_work(scan_objects_work);
        } else {
            self.mmtk.scheduler.work_buckets[WorkBucketStage::Closure].add(scan_objects_work);
        }
    }

    #[inline]
    fn process_edge(&mut self, slot: Address) {
        let loaded_object = unsafe { slot.load::<ObjectReference>() };
        let mut edge = Self::PE::new();
        let object = edge.preprocess_edge(loaded_object);
        let traced_object = self.trace_object(object);
        let new_object = edge.postprocess_edge(traced_object);
        if Self::OVERWRITE_REFERENCE {
            unsafe { slot.store(new_object) };
        }
    }

    #[inline]
    fn process_edges(&mut self) {
        for i in 0..self.edges.len() {
            self.process_edge(self.edges[i])
        }
    }
}

impl<E: ProcessEdgesWork> GCWork<E::VM> for E {
    #[inline]
    default fn do_work(&mut self, worker: &mut GCWorker<E::VM>, _mmtk: &'static MMTK<E::VM>) {
        trace!("ProcessEdgesWork");
        self.set_worker(worker);
        self.process_edges();
        if !self.nodes.is_empty() {
            self.flush();
        }
        trace!("ProcessEdgesWork End");
    }
}

/// Scan & update a list of object slots
pub struct ScanObjects<Edges: ProcessEdgesWork> {
    buffer: Vec<ObjectReference>,
    #[allow(unused)]
    concurrent: bool,
    phantom: PhantomData<Edges>,
}

impl<Edges: ProcessEdgesWork> ScanObjects<Edges> {
    pub fn new(buffer: Vec<ObjectReference>, concurrent: bool) -> Self {
        Self {
            buffer,
            concurrent,
            phantom: PhantomData,
        }
    }
}

impl<E: ProcessEdgesWork> GCWork<E::VM> for ScanObjects<E> {
    fn do_work(&mut self, worker: &mut GCWorker<E::VM>, _mmtk: &'static MMTK<E::VM>) {
        trace!("ScanObjects");
        <E::VM as VMBinding>::VMScanning::scan_objects::<E>(&self.buffer, worker);
        trace!("ScanObjects End");
    }
}

#[derive(Default)]
pub struct ProcessModBuf<E: ProcessEdgesWork> {
    modified_nodes: Vec<ObjectReference>,
    modified_edges: Vec<Address>,
    phantom: PhantomData<E>,
}

impl<E: ProcessEdgesWork> ProcessModBuf<E> {
    pub fn new(modified_nodes: Vec<ObjectReference>, modified_edges: Vec<Address>) -> Self {
        Self {
            modified_nodes,
            modified_edges,
            phantom: PhantomData,
        }
    }
}

impl<E: ProcessEdgesWork> GCWork<E::VM> for ProcessModBuf<E> {
    #[inline]
    fn do_work(&mut self, worker: &mut GCWorker<E::VM>, mmtk: &'static MMTK<E::VM>) {
        if mmtk.plan.in_nursery() {
            let mut modified_nodes = vec![];
            ::std::mem::swap(&mut modified_nodes, &mut self.modified_nodes);
            worker.scheduler().work_buckets[WorkBucketStage::Closure]
                .add(ScanObjects::<E>::new(modified_nodes, false));

            let mut modified_edges = vec![];
            ::std::mem::swap(&mut modified_edges, &mut self.modified_edges);
            worker.scheduler().work_buckets[WorkBucketStage::Closure].add(E::new(
                modified_edges,
                true,
                mmtk,
            ));
        } else {
            // Do nothing
        }
    }
}
