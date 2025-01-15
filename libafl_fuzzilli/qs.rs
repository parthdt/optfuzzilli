#![allow(unused_imports, dead_code, unused_variables)]

use std::sync::{Arc, Mutex};
use libafl::{
    corpus::{InMemoryCorpus, OnDiskCorpus, Testcase, Corpus, CorpusId},
    feedbacks::MaxMapFeedback,
    inputs::{BytesInput, HasMutatorBytes},
    observers::{StdMapObserver, HitcountsMapObserver},
    schedulers::{QueueScheduler, Scheduler},
    state::{StdState, HasCorpus},
};
use libafl_bolts::{
    rands::RomuDuoJrRand,
    shmem::{MmapShMem, MmapShMemProvider, ShMemProvider, ShMemId},
    AsSliceMut,
};

#[derive(uniffi::Object, Debug)]
pub struct LibAflObject {
    state: Arc<Mutex<StdState<BytesInput, OnDiskCorpus<BytesInput>, RomuDuoJrRand, InMemoryCorpus<BytesInput>>>>,
    scheduler: Arc<Mutex<QueueScheduler>>,
    observer: Arc<Mutex<HitcountsMapObserver<StdMapObserver<'static, u8, false>>>>,
    feedback: Arc<Mutex<MaxMapFeedback<HitcountsMapObserver<StdMapObserver<'static, u8, false>>, HitcountsMapObserver<StdMapObserver<'static, u8, false>>>>>,
    _shmem: Arc<Mutex<MmapShMem>>, // Keep shared memory alive
}

unsafe impl Send for LibAflObject {}
unsafe impl Sync for LibAflObject {}

#[uniffi::export]
impl LibAflObject {
    #[uniffi::constructor]
    pub fn new(corpus_dir: String, shmem_key: String) -> Arc<Self> {
        // Initialize shared memory provider
        let mut shmem_provider = MmapShMemProvider::new().expect("Failed to create shared memory provider");
        let shmem_id = ShMemId::from_string(&shmem_key);
        let shmem = shmem_provider
            .shmem_from_id_and_size(shmem_id, 0x200000)
            .expect("Failed to attach to shared memory");

        // Wrap shared memory in Arc<Mutex>
        let shmem_arc = Arc::new(Mutex::new(shmem));

        // Get a mutable slice of the shared memory
        let shared_mem_slice: &'static mut [u8] = {
            let mut shmem_locked = shmem_arc.lock().unwrap();
            unsafe { std::mem::transmute::<&mut [u8], &'static mut [u8]>(shmem_locked.as_slice_mut()) }
        };

        // Create observer
        let std_observer = unsafe { StdMapObserver::new("shared_mem", shared_mem_slice) };
        let hitcounts_observer = HitcountsMapObserver::new(std_observer);

        // Initialize corpus
        let on_disk_corpus = OnDiskCorpus::new(&corpus_dir).expect("Failed to create OnDiskCorpus");
        let in_memory_corpus = InMemoryCorpus::new();

        // Initialize random generator
        let rng = RomuDuoJrRand::with_seed(12345); // Use a compatible random generator

        // Initialize feedbacks
        let mut feedback = MaxMapFeedback::new(&hitcounts_observer);
        let mut objective_feedback = MaxMapFeedback::new(&hitcounts_observer);

        // Initialize state
        let state = StdState::new(
            rng,
            on_disk_corpus,
            in_memory_corpus,
            &mut feedback,
            &mut objective_feedback,
        )
        .expect("Failed to initialize StdState");

        // Initialize scheduler
        let scheduler = QueueScheduler::new();

        Arc::new(Self {
            state: Arc::new(Mutex::new(state)),
            scheduler: Arc::new(Mutex::new(scheduler)),
            observer: Arc::new(Mutex::new(hitcounts_observer)),
            feedback: Arc::new(Mutex::new(feedback)),
            _shmem: shmem_arc,
        })
    }

    /// Add a new input to the corpus.
    pub fn add_input(&self, input_data: Vec<u8>) {
        let input = BytesInput::new(input_data);
        let testcase = Testcase::new(input);
        let mut state = self.state.lock().unwrap();
        state.corpus_mut().add(testcase).expect("Failed to add testcase to corpus");
    }

    /// Suggest the next input for fuzzing.
    pub fn suggest_next_input(&self) -> Vec<u8> {
        let mut scheduler = self.scheduler.lock().unwrap();
        let mut state = self.state.lock().unwrap();
        let next_id = <QueueScheduler as Scheduler<
            BytesInput,
            StdState<BytesInput, OnDiskCorpus<BytesInput>, RomuDuoJrRand, InMemoryCorpus<BytesInput>>,
        >>::next(&mut *scheduler, &mut *state).expect("Failed to fetch next input ID");
        let testcase = state.corpus().get(next_id).unwrap();
        let borrowed = testcase.borrow();
        let input = borrowed.input().as_ref().unwrap();
        input.bytes().to_vec()
    }

    pub fn count(&self) -> u64 {
        let state = self.state.lock().unwrap();
        state.corpus().count() as u64
    }

    pub fn first_index(&self) -> u64 {
        let state = self.state.lock().unwrap();
        let first_id = state.corpus().first().unwrap_or(CorpusId(0));
        let first_id_usize : usize = first_id.into();
        let first_id_u64 : u64 = first_id_usize as u64;
        first_id_u64
    }

    pub fn last_index(&self) -> u64 {
        let state = self.state.lock().unwrap();
        let last_id = state.corpus().last().unwrap_or(CorpusId(0));
        let last_id_usize : usize = last_id.into();
        let last_id_u64 : u64 = last_id_usize as u64;
        last_id_u64
    }

    pub fn get_element(&self, id: u64) -> Vec<u8> {
        let state = self.state.lock().unwrap();
        let corpus_id = CorpusId(id as usize);
        match state.corpus().get(corpus_id) {
            Ok(testcase) => {
                if let Some(input) = testcase.borrow().input() {
                    input.bytes().to_vec()
                } else {
                    Vec::new()
                }
            }
            Err(_) => Vec::new(),
        }
    }

}

uniffi::setup_scaffolding!();

