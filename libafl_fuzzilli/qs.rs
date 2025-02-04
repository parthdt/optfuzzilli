#![allow(unused_imports, dead_code, unused_variables)]

use std::sync::{Arc, Mutex};
use libafl::{
    corpus::{InMemoryCorpus, OnDiskCorpus, Testcase, Corpus, CorpusId},
    feedbacks::{MaxMapFeedback, ConstFeedback},
    inputs::{BytesInput, HasMutatorBytes, ValueInput},
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
    state: Arc<Mutex<StdState<OnDiskCorpus<BytesInput>, BytesInput, RomuDuoJrRand, InMemoryCorpus<BytesInput>>>>,
    scheduler: Arc<Mutex<QueueScheduler>>,
}

unsafe impl Send for LibAflObject {}
unsafe impl Sync for LibAflObject {}

#[uniffi::export]
impl LibAflObject {
    #[uniffi::constructor]
    pub fn new(corpus_dir: String) -> Arc<Self> {
        // Initialize corpus
        let on_disk_corpus = OnDiskCorpus::<BytesInput>::new(&corpus_dir).expect("Failed to create OnDiskCorpus");
        let in_memory_corpus = InMemoryCorpus::<BytesInput>::new();

        // Initialize random generator
        let rng = RomuDuoJrRand::with_seed(1337); // Use a compatible random generator

        // Initialize feedbacks
        let mut feedback = ConstFeedback::new(false);
        let mut objective = ConstFeedback::new(false);

        // Initialize state
        let state = StdState::new(
            rng,
            on_disk_corpus,
            in_memory_corpus,
            &mut feedback,
            &mut objective,
        )
        .expect("Failed to initialize StdState");

        // Initialize scheduler
        let scheduler = QueueScheduler::new();

        Arc::new(Self {
            state: Arc::new(Mutex::new(state)),
            scheduler: Arc::new(Mutex::new(scheduler)),
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
        let next_id = scheduler.next(&mut *state).expect("Failed to fetch next input ID");
        let testcase = state.corpus().get(next_id).unwrap();
        let borrowed = testcase.borrow();
        let input = borrowed.input().as_ref().unwrap();
        input.mutator_bytes().to_vec()
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
                    input.mutator_bytes().to_vec()
                } else {
                    Vec::new()
                }
            }
            Err(_) => Vec::new(),
        }
    }

}

uniffi::setup_scaffolding!();